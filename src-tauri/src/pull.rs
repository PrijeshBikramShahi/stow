//! Explicit authorization and a separate, non-cancellable working-tree policy.
use crate::{
    core::{now, AppError, ErrorCode, Result},
    database::Registry,
    git::{check_cancel, Git},
    safe_fs::{self, Directory},
    sync::{self, Assessment},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    os::{
        fd::AsRawFd,
        unix::{fs::MetadataExt, process::CommandExt},
    },
    path::Path,
    process::{Command, Stdio},
    sync::atomic::AtomicBool,
};
fn blocked(message: &str) -> AppError {
    AppError::new(ErrorCode::Unsupported, message)
}
fn stale() -> AppError {
    AppError::new(
        ErrorCode::RecoveryRequired,
        "Remote state or workspace changed. Review again. No integration was started.",
    )
}
fn uncertain() -> AppError {
    AppError::new(ErrorCode::RecoveryRequired, "Pull requires inspection: Git may have updated HEAD, index, or files. Stop other Git activity and inspect status, HEAD and history in your terminal. Stow will not reset or retry automatically.")
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullJob {
    pub id: String,
    pub project_id: i64,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    pub state: String,
    pub phase: String,
    pub branch: String,
    pub upstream: String,
    pub before_head: String,
    pub target: Option<String>,
    pub after_head: Option<String>,
    pub after_relation: Option<String>,
    pub error: Option<AppError>,
}
#[derive(Clone, PartialEq, Eq)]
struct Snapshot {
    config: Vec<(String, String)>,
    index: Vec<u8>,
    heads: BTreeMap<String, String>,
    tracking: BTreeMap<String, String>,
    head: String,
    branch: String,
    upstream: String,
}
pub struct PreparedPull {
    root: Directory,
    meta: Directory,
    snapshot: Snapshot,
    assessment: Assessment,
    remote: String,
    remote_branch: String,
    pub job: PullJob,
}
// Cheap assessment messaging; full filesystem/index policy is checked under lock.
pub fn eligibility(a: &Assessment) -> Option<String> {
    let reason = if a.identity.is_none() {
        "Pull requires a verified ordinary workspace."
    } else if a.inspection.detached
        || a.inspection.branch.is_none()
        || a.inspection.commit.is_none()
    {
        "Pull requires an ordinary checked-out branch with a commit."
    } else if !a.inspection.working_tree.is_clean() {
        "Uncommitted changes must be handled before pulling."
    } else if !a.operations.is_empty() {
        "A Git operation or lock is active. Finish it before pulling."
    } else {
        match a.relation.as_str() {
        "diverged" => "Local and upstream histories have diverged.",
        "ahead" => "Local branch is ahead only. Pull requires the upstream to descend from HEAD.",
        "behind" | "up_to_date" => return None,
        _ => "Pull requires a single valid upstream and an available ordinary tracking reference.",
    }
    };
    Some(reason.into())
}
fn tree_policy(git: &Git, root: &Path, commit: &str, cancel: &AtomicBool) -> Result<()> {
    if !sync::oid(commit) {
        return Err(blocked("Unsupported commit format."));
    }
    let (_, bytes) = git.run(root, &["ls-tree", "-r", "-z", commit], cancel, &[])?;
    for record in bytes.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let line =
            std::str::from_utf8(record).map_err(|_| blocked("Pull requires UTF-8 file names."))?;
        let (entry, path) = line
            .split_once('\t')
            .ok_or_else(|| blocked("Invalid tree entry."))?;
        if !(entry.starts_with("100644 blob ") || entry.starts_with("100755 blob "))
            || path.split('/').any(|part| {
                [".gitattributes", ".gitmodules", ".git"]
                    .iter()
                    .any(|name| part.eq_ignore_ascii_case(name))
            })
        {
            return Err(blocked("Pull does not support attributes, submodules, symlinks, or special tree entries in either revision."));
        }
    }
    Ok(())
}
fn filesystem_policy(root: &Directory, meta: &Directory) -> Result<()> {
    sync::checked_layout(root, meta)?;
    for name in ["info/attributes", "info/sparse-checkout", "modules"] {
        if fs::symlink_metadata(meta.path.join(name)).is_ok() {
            return Err(blocked(
                "Pull does not support local attributes, sparse checkout, or submodule storage.",
            ));
        }
    }
    if let Ok(entries) = fs::read_dir(meta.path.join("hooks")) {
        for entry in entries {
            if !entry
                .map_err(AppError::io)?
                .file_name()
                .to_string_lossy()
                .ends_with(".sample")
            {
                return Err(blocked("Repository hooks must be reviewed in your terminal; pull is blocked while non-sample hooks exist."));
            }
        }
    }
    if let Ok(value) = fs::read(meta.path.join("ORIG_HEAD")) {
        if value.starts_with(b"ref:") {
            return Err(blocked("Symbolic ORIG_HEAD is unsupported."));
        }
    }
    let mut stack = vec![root.path.clone()];
    let mut count = 0;
    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(&path).map_err(AppError::io)? {
            let entry = entry.map_err(AppError::io)?;
            if entry.path() == meta.path {
                continue;
            }
            count += 1;
            if count > 100_000 {
                return Err(blocked("Workspace exceeds the pull validation limit."));
            }
            let m = fs::symlink_metadata(entry.path()).map_err(AppError::io)?;
            if m.file_type().is_symlink()
                || (!m.is_dir() && !m.is_file())
                || m.uid() != unsafe { libc::geteuid() }
                || m.mode() & 0o022 != 0
                || m.is_file() && m.nlink() != 1
            {
                return Err(blocked("Pull requires private ordinary workspace files without symlinks, hardlinks, or shared write access."));
            }
            if m.is_dir() {
                Directory::open(&entry.path())?.owned()?;
                stack.push(entry.path());
            }
        }
    }
    Ok(())
}
fn snapshot(
    git: &Git,
    root: &Directory,
    meta: &Directory,
    cancel: &AtomicBool,
) -> Result<(Assessment, Snapshot)> {
    check_cancel(cancel)?;
    filesystem_policy(root, meta)?;
    let a = sync::assess(git, &root.path, cancel)?;
    if let Some(reason) = eligibility(&a) {
        return Err(blocked(&reason));
    }
    let branch = a.inspection.branch.clone().ok_or_else(stale)?;
    let full = format!("refs/heads/{branch}");
    if sync::output(
        git,
        &root.path,
        &["symbolic-ref", "--no-recurse", "HEAD"],
        cancel,
    )? != full
    {
        return Err(blocked(
            "HEAD must directly reference the checked-out local branch.",
        ));
    }
    // Includes ignored files: Git merge otherwise may overwrite ignored obstructions.
    if !git
        .run(&root.path, &["ls-files", "--others", "-z"], cancel, &[])?
        .1
        .is_empty()
    {
        return Err(blocked(
            "Untracked or ignored files must be handled before pulling.",
        ));
    }
    let (_, flags) = git.run(&root.path, &["ls-files", "-v", "-z"], cancel, &[])?;
    if flags
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .any(|s| !s.starts_with(b"H "))
    {
        return Err(blocked(
            "Pull does not support assume-unchanged, skip-worktree, or unmerged index entries.",
        ));
    }
    let head = a.inspection.commit.clone().ok_or_else(stale)?;
    tree_policy(git, &root.path, &head, cancel)?;
    let cfg = sync::config(git, &root.path, cancel)?;
    let remotes = sync::values(&cfg, &format!("branch.{branch}.remote"));
    if remotes.len() != 1 {
        return Err(blocked("A single remote is required."));
    }
    sync::fetch_config(&cfg, remotes[0])?;
    // Pin the byte representation as well as semantic cleanliness; external Git
    // index refreshes require renewed review, even if they preserve file contents.
    let index_path = meta.path.join("index");
    if fs::metadata(&index_path).map_err(AppError::io)?.len() > 32 * 1024 * 1024 {
        return Err(blocked("Index exceeds the pull validation limit."));
    }
    let snapshot = Snapshot {
        config: cfg,
        index: fs::read(index_path).map_err(AppError::io)?,
        heads: sync::refs(git, &root.path, "refs/heads/", cancel)?,
        tracking: sync::refs(git, &root.path, "refs/remotes/", cancel)?,
        head,
        branch,
        upstream: a.upstream_ref.clone().ok_or_else(stale)?,
    };
    if snapshot.heads.get(&snapshot.branch_ref()) != Some(&snapshot.head) {
        return Err(blocked(
            "Symbolic or inconsistent local branch is unsupported.",
        ));
    }
    Ok((a, snapshot))
}
impl Snapshot {
    fn branch_ref(&self) -> String {
        format!("refs/heads/{}", self.branch)
    }
}
pub fn prepare(
    db: &Registry,
    git: &Git,
    id: i64,
    token: &str,
    cancel: &AtomicBool,
) -> Result<PreparedPull> {
    // Persist refusals as well as authorized attempts; invalid/unregistered IDs
    // never gain a journal or filesystem authority.
    db.get(id)?;
    let saved = db.sync_record(id)?.assessment;
    let result = prepare_inner(db, git, id, token, cancel);
    if let Err(error) = &result {
        db.save_pull(&PullJob {
            id: safe_fs::random_id()?,
            project_id: id,
            started_at: now(),
            completed_at: Some(now()),
            state: if error.code == ErrorCode::Cancelled {
                "cancelled"
            } else {
                "failed"
            }
            .into(),
            phase: "preflight".into(),
            branch: saved
                .as_ref()
                .and_then(|a| a.inspection.branch.clone())
                .unwrap_or_default(),
            upstream: saved
                .as_ref()
                .and_then(|a| a.upstream_ref.clone())
                .unwrap_or_default(),
            before_head: saved
                .as_ref()
                .and_then(|a| a.inspection.commit.clone())
                .unwrap_or_default(),
            target: None,
            after_head: None,
            after_relation: None,
            error: Some(error.clone()),
        })?;
    }
    result
}
fn prepare_inner(
    db: &Registry,
    git: &Git,
    id: i64,
    token: &str,
    cancel: &AtomicBool,
) -> Result<PreparedPull> {
    let project = db.get(id)?;
    let saved = db
        .sync_record(id)?
        .assessment
        .filter(|a| a.token == token)
        .ok_or_else(stale)?;
    let root = Directory::open(Path::new(&project.path))?;
    let meta = root.child(".git")?;
    if saved.identity
        != Some(sync::WorkspaceIdentity {
            root: root.identity()?,
            git: meta.identity()?,
        })
    {
        return Err(safe_fs::changed());
    }
    root.lock()?;
    meta.lock()?;
    let (mut assessment, snapshot) = snapshot(git, &root, &meta, cancel)?;
    if assessment.inspection.commit != saved.inspection.commit
        || assessment.inspection.branch != saved.inspection.branch
        || assessment.upstream_commit != saved.upstream_commit
        || assessment.upstream_ref != saved.upstream_ref
        || assessment.remotes != saved.remotes
    {
        return Err(stale());
    }
    let remote = sync::values(
        &snapshot.config,
        &format!("branch.{}.remote", snapshot.branch),
    )[0]
    .to_owned();
    let remote_branch = sync::values(
        &snapshot.config,
        &format!("branch.{}.merge", snapshot.branch),
    )[0]
    .to_owned();
    let expected = format!(
        "refs/remotes/{remote}/{}",
        remote_branch
            .strip_prefix("refs/heads/")
            .ok_or_else(stale)?
    );
    if expected != snapshot.upstream {
        return Err(stale());
    }
    assessment.token = saved.token;
    let job = PullJob {
        id: safe_fs::random_id()?,
        project_id: id,
        started_at: now(),
        completed_at: None,
        state: "running".into(),
        phase: "awaiting_confirmation".into(),
        branch: snapshot.branch.clone(),
        upstream: snapshot.upstream.clone(),
        before_head: snapshot.head.clone(),
        target: None,
        after_head: None,
        after_relation: None,
        error: None,
    };
    db.save_pull(&job)?;
    Ok(PreparedPull {
        root,
        meta,
        snapshot,
        assessment,
        remote,
        remote_branch,
        job,
    })
}
impl PreparedPull {
    pub fn stop(&mut self, db: &Registry, error: AppError) -> Result<()> {
        self.job.state = if self.job.phase == "integrating"
            || self.job.phase == "verifying"
            || self.job.phase == "verified"
        {
            "requires_inspection"
        } else if error.code == ErrorCode::Cancelled {
            "cancelled"
        } else {
            "failed"
        }
        .into();
        self.job.error = Some(error);
        self.job.completed_at = Some(now());
        db.save_pull(&self.job)
    }
    pub fn execute(
        &mut self,
        db: &Registry,
        git: &Git,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(&str, Option<u8>),
    ) -> Result<()> {
        let result = self.execute_inner(db, git, cancel, progress).map_err(|e| {
            if matches!(
                self.job.phase.as_str(),
                "integrating" | "verifying" | "verified"
            ) {
                AppError::new(
                    ErrorCode::RecoveryRequired,
                    format!("{} {}", e.message, uncertain().message),
                )
            } else {
                e
            }
        });
        if let Err(e) = &result {
            if sync::inspect(db, git, self.job.project_id, &AtomicBool::new(false)).is_ok() {
                if let Some(a) = db.sync_record(self.job.project_id)?.assessment {
                    self.job.after_head = a.inspection.commit;
                    self.job.after_relation = Some(a.relation);
                }
            }
            self.stop(db, e.clone())?;
        }
        result
    }
    fn execute_inner(
        &mut self,
        db: &Registry,
        git: &Git,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(&str, Option<u8>),
    ) -> Result<()> {
        check_cancel(cancel)?;
        if snapshot(git, &self.root, &self.meta, cancel)?.1 != self.snapshot {
            return Err(stale());
        }
        self.job.phase = "fetching".into();
        db.save_pull(&self.job)?;
        // Fetch uses the same descriptors' still-held advisory locks throughout.
        let fetched = sync::fetch_held(
            db,
            git,
            self.job.project_id,
            &self.assessment.token,
            &self.remote,
            cancel,
            progress,
        )?;
        progress("Fetch finished; checking pull authorization again", None);
        check_cancel(cancel)?;
        let target = fetched.fetched_refs.get(&self.remote_branch).ok_or_else(|| blocked("Configured upstream was not advertised by the remote. Cached deleted refs cannot authorize pull."))?.clone();
        let (a, after_fetch) = snapshot(git, &self.root, &self.meta, cancel)?;
        let mut expected = self.snapshot.clone();
        // Compare every tracking ref to precisely the changes this fetch reported,
        // including retained deleted refs and unrelated remotes.
        for (reference, hash) in &fetched.fetched_refs {
            let suffix = reference.strip_prefix("refs/heads/").ok_or_else(stale)?;
            expected.tracking.insert(
                format!("refs/remotes/{}/{suffix}", self.remote),
                hash.clone(),
            );
        }
        if expected != after_fetch || a.upstream_commit.as_ref() != Some(&target) {
            return Err(stale());
        }
        tree_policy(git, &self.root.path, &target, cancel)?;
        if git
            .run(
                &self.root.path,
                &["merge-base", "--is-ancestor", &self.snapshot.head, &target],
                cancel,
                &[1],
            )?
            .0
            != 0
        {
            return Err(blocked("Local and upstream histories have diverged or the local branch is ahead. Fast-forward refused."));
        }
        self.job.target = Some(target.clone());
        self.job.phase = "ready".into();
        db.save_pull(&self.job)?;
        progress("Ready to integrate; repeating critical validation", None);
        check_cancel(cancel)?;
        if snapshot(git, &self.root, &self.meta, cancel)?.1 != after_fetch {
            return Err(stale());
        }
        if self.snapshot.head != target {
            self.job.phase = "integrating".into();
            db.save_pull(&self.job)?;
            // No cancellation checks, kills, or timeout once this critical section starts.
            progress(
                "Updating working tree; cancellation deferred until verification completes",
                None,
            );
            if snapshot(git, &self.root, &self.meta, &AtomicBool::new(false))?.1 != after_fetch {
                return Err(stale());
            }
            IntegrationRunner::run(git, &self.root, &self.meta, &target)?;
        }
        self.job.phase = "verifying".into();
        db.save_pull(&self.job).map_err(|_| uncertain())?;
        let verify = AtomicBool::new(false);
        let (observed, observed_snapshot) =
            snapshot(git, &self.root, &self.meta, &verify).map_err(|_| uncertain())?;
        self.job.after_head = observed.inspection.commit.clone();
        self.job.after_relation = Some(observed.relation.clone());
        let mut heads = self.snapshot.heads.clone();
        heads.insert(self.snapshot.branch_ref(), target.clone());
        if observed_snapshot.head != target
            || observed_snapshot.branch != self.snapshot.branch
            || observed_snapshot.heads != heads
            || observed_snapshot.tracking != after_fetch.tracking
            || observed_snapshot.config != self.snapshot.config
            || observed_snapshot.upstream != self.snapshot.upstream
        {
            return Err(uncertain());
        }
        sync::inspect(db, git, self.job.project_id, &verify).map_err(|_| uncertain())?;
        self.job.state = "completed".into();
        self.job.phase = "verified".into();
        self.job.completed_at = Some(now());
        db.save_pull(&self.job).map_err(|_| uncertain())?;
        progress(
            if self.snapshot.head == target {
                "Already up to date."
            } else {
                "Fast-forward completed; HEAD, index, and working files verified."
            },
            Some(100),
        );
        Ok(())
    }
}
// The integration runner accepts exactly one validated commit, never caller flags.
// It deliberately does not reuse cancellable inspection/transfer supervision.
struct IntegrationRunner;
impl IntegrationRunner {
    fn run(git: &Git, root: &Directory, meta: &Directory, target: &str) -> Result<()> {
        if !sync::oid(target) {
            return Err(blocked("Invalid integration target."));
        }
        root.validate_path()?;
        meta.validate_path()?;
        let mut cmd = Command::new(git.executable());
        cmd.env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_NO_REPLACE_OBJECTS", "1");
        for setting in [
            "core.hooksPath=/dev/null",
            "core.fsmonitor=false",
            "core.attributesFile=/dev/null",
            "core.excludesFile=/dev/null",
            "core.autocrlf=false",
            "core.safecrlf=true",
            "core.untrackedCache=false",
            "core.preloadIndex=false",
            "credential.helper=",
            "protocol.allow=never",
            "submodule.recurse=false",
            "merge.autoStash=false",
            "merge.verifySignatures=false",
            "commit.gpgSign=false",
            "gc.auto=0",
            "maintenance.auto=false",
            "core.pager=cat",
        ] {
            cmd.args(["-c", setting]);
        }
        let fd = root.file.as_raw_fd();
        unsafe {
            cmd.pre_exec(move || {
                if libc::fchdir(fd) == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
        // A separate group survives UI shutdown signals. Output goes to /dev/null;
        // no pipe growth/deadlock can force us to kill a working-tree update.
        cmd.process_group(0)
            .args([
                "merge",
                "--ff-only",
                "--no-edit",
                "--no-stat",
                "--no-progress",
                "--no-overwrite-ignore",
                "--no-autostash",
                "--no-verify",
                "--no-gpg-sign",
                target,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = cmd.spawn().map_err(|_| uncertain())?;
        let status = child.wait().map_err(|_| uncertain())?;
        if status.success() {
            Ok(())
        } else {
            Err(uncertain())
        }
    }
}
