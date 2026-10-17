//! Single existing branch publication from an isolated bare repository.
use crate::{
    core::{now, AppError, ErrorCode, Result, WorkingTree},
    database::Registry,
    git::{check_cancel, Git},
    safe_fs::{self, Directory},
    sync,
    workspace::{reject_hooks, LockedWorkspace},
};
use serde::{Deserialize, Serialize};
use std::{
    os::{fd::AsRawFd, unix::process::CommandExt},
    path::Path,
    process::Command,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
fn blocked(s: &str) -> AppError {
    AppError::new(ErrorCode::Unsupported, s)
}
fn stale() -> AppError {
    AppError::new(ErrorCode::RecoveryRequired,"Push authorization is stale. HEAD, branch, destination, or remote state changed. Review again.")
}
fn uncertain() -> AppError {
    AppError::new(ErrorCode::RecoveryRequired,"Remote outcome uncertain. Do not retry automatically. Inspect the exact remote branch and refresh push preflight before deciding what to do next.")
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushJob {
    pub id: String,
    pub project_id: i64,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    pub state: String,
    pub phase: String,
    pub repository: String,
    pub branch: String,
    pub head: String,
    pub remote: String,
    pub url: String,
    pub destination: String,
    pub remote_before: Option<String>,
    pub remote_after: Option<String>,
    pub ahead: u64,
    pub commits: Vec<String>,
    pub dirty: WorkingTree,
    pub error: Option<AppError>,
}
pub struct PreparedPush {
    workspace: LockedWorkspace,
    config: Vec<(String, String)>,
    stage: Directory,
    pub job: PushJob,
}
type Policy = (
    Vec<(String, String)>,
    String,
    String,
    String,
    String,
    String,
);
fn policy(git: &Git, w: &LockedWorkspace, cancel: &AtomicBool) -> Result<Policy> {
    let (branch, head) = w.branch(git, cancel)?;
    reject_hooks(&w.meta.path)?;
    let cfg = sync::config(git, &w.root.path, cancel)?;
    if cfg.iter().any(|(k, _)| k.ends_with(".pushurl")) {
        return Err(blocked(
            "Push URLs are unsupported. Push must use the single fetch URL and existing upstream.",
        ));
    }
    let remotes = sync::values(&cfg, &format!("branch.{branch}.remote"));
    let merges = sync::values(&cfg, &format!("branch.{branch}.merge"));
    if remotes.len() != 1 || merges.len() != 1 || !merges[0].starts_with("refs/heads/") {
        return Err(blocked(
            "Push requires one existing remote upstream branch.",
        ));
    }
    let remote = remotes[0].to_string();
    let destination = merges[0].to_string();
    let url = sync::fetch_config(&cfg, &remote)?;
    if url.starts_with('/') {
        reject_hooks(Path::new(&url))?;
    }
    Ok((cfg, branch, head, remote, destination, url))
}
pub fn prepare(
    db: &Registry,
    git: &Git,
    id: i64,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, Option<u8>),
) -> Result<PreparedPush> {
    let workspace = LockedWorkspace::open(db, id)?;
    let (config, branch, head, remote, destination, url) = policy(git, &workspace, cancel)?;
    let a = sync::assess(git, &workspace.root.path, cancel)?;
    let mut job = PushJob {
        id: safe_fs::random_id()?,
        project_id: id,
        started_at: now(),
        completed_at: None,
        state: "running".into(),
        phase: "preflight".into(),
        repository: workspace.root.path.to_string_lossy().into(),
        branch,
        head,
        remote,
        url,
        destination,
        remote_before: None,
        remote_after: None,
        ahead: 0,
        commits: vec![],
        dirty: a.inspection.working_tree,
        error: None,
    };
    db.save_push(&job)?;
    let result = (|| {
        sync::inspect(db, git, id, cancel)?;
        let token = db.sync_record(id)?.assessment.ok_or_else(stale)?.token;
        progress("Refreshing destination before push confirmation", None);
        let fetched = sync::fetch_held(db, git, id, &token, &job.remote, cancel, progress)?;
        let before = fetched
            .fetched_refs
            .get(&job.destination)
            .ok_or_else(|| {
                blocked("Destination branch does not exist. First-time publication is unsupported.")
            })?
            .clone();
        if policy(git, &workspace, cancel)?
            != (
                config.clone(),
                job.branch.clone(),
                job.head.clone(),
                job.remote.clone(),
                job.destination.clone(),
                job.url.clone(),
            )
        {
            return Err(stale());
        }
        if git
            .run(
                &workspace.root.path,
                &["merge-base", "--is-ancestor", &before, &job.head],
                cancel,
                &[1],
            )?
            .0
            != 0
        {
            return Err(blocked("Remote is ahead or histories have diverged. A normal fast-forward push is unavailable."));
        }
        job.ahead = sync::output(
            git,
            &workspace.root.path,
            &["rev-list", "--count", &format!("{before}..{}", job.head)],
            cancel,
        )?
        .parse()
        .map_err(|_| blocked("Invalid ahead count."))?;
        job.commits = sync::output(
            git,
            &workspace.root.path,
            &[
                "log",
                "-20",
                "--format=%h %s",
                &format!("{before}..{}", job.head),
                "--",
            ],
            cancel,
        )?
        .lines()
        .map(str::to_owned)
        .collect();
        job.remote_before = Some(before);
        let stage = Directory::open(Path::new(fetched.staging.as_ref().ok_or_else(stale)?))?;
        job.phase = "awaiting_confirmation".into();
        db.save_push(&job)?;
        Ok(stage)
    })();
    match result {
        Ok(stage) => Ok(PreparedPush {
            workspace,
            config,
            stage,
            job,
        }),
        Err(e) => {
            job.state = before_dispatch_state(&e).into();
            job.error = Some(e.clone());
            job.completed_at = Some(now());
            db.save_push(&job)?;
            Err(e)
        }
    }
}
impl PreparedPush {
    pub fn stop(&mut self, db: &Registry, e: AppError) -> Result<()> {
        self.job.state = if matches!(
            self.job.phase.as_str(),
            "dispatching" | "verifying" | "verified"
        ) {
            "uncertain"
        } else {
            before_dispatch_state(&e)
        }
        .into();
        self.job.error = Some(e);
        self.job.completed_at = Some(now());
        db.save_push(&self.job)
    }
    pub fn execute(
        &mut self,
        db: &Registry,
        git: &Git,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(&str, Option<u8>),
    ) -> Result<()> {
        let outcome = self.execute_inner(db, git, cancel, progress);
        if let Err(e) = &outcome {
            if self.job.state != "rejected" && self.job.state != "uncertain" {
                self.stop(db, e.clone())?;
            }
        }
        outcome
    }
    fn validate(&self, git: &Git, cancel: &AtomicBool) -> Result<()> {
        if policy(git, &self.workspace, cancel)?
            != (
                self.config.clone(),
                self.job.branch.clone(),
                self.job.head.clone(),
                self.job.remote.clone(),
                self.job.destination.clone(),
                self.job.url.clone(),
            )
        {
            return Err(stale());
        }
        Ok(())
    }
    fn execute_inner(
        &mut self,
        db: &Registry,
        git: &Git,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(&str, Option<u8>),
    ) -> Result<()> {
        check_cancel(cancel)?;
        self.validate(git, cancel)?;
        self.job.phase = "revalidating".into();
        db.save_push(&self.job)?;
        sync::inspect(db, git, self.job.project_id, cancel)?;
        let token = db
            .sync_record(self.job.project_id)?
            .assessment
            .ok_or_else(stale)?
            .token;
        let fetched = sync::fetch_held(
            db,
            git,
            self.job.project_id,
            &token,
            &self.job.remote,
            cancel,
            progress,
        )?;
        if fetched.fetched_refs.get(&self.job.destination) != self.job.remote_before.as_ref() {
            return Err(stale());
        }
        self.validate(git, cancel)?;
        if git
            .run(
                &self.workspace.root.path,
                &[
                    "merge-base",
                    "--is-ancestor",
                    self.job.remote_before.as_deref().ok_or_else(stale)?,
                    &self.job.head,
                ],
                cancel,
                &[1],
            )?
            .0
            != 0
        {
            return Err(stale());
        }
        let runner = PushRunner { git };
        if runner.remote(&self.stage, &self.job.url, &self.job.destination, cancel)?
            != self.job.remote_before
        {
            return Err(stale());
        }
        if self.job.remote_before.as_ref() == Some(&self.job.head) {
            self.job.remote_after = Some(self.job.head.clone());
            self.job.state = "already_synchronized".into();
        } else {
            progress(
                "Push ready; checking exact authorized HEAD and destination",
                None,
            );
            check_cancel(cancel)?;
            self.validate(git, cancel)?;
            // The fixed source ref lives only in the isolated fetch repository.
            runner.seed(&self.stage, &self.workspace, &self.job.head, cancel)?;
            check_cancel(cancel)?;
            self.validate(git, cancel)?;
            self.job.phase = "dispatching".into();
            db.save_push(&self.job)?;
            progress(
                "Publishing one branch; interruption may leave an uncertain remote outcome",
                None,
            );
            let dispatched = runner.push(
                &self.stage,
                &self.workspace,
                &self.job.url,
                &self.job.destination,
                cancel,
            );
            self.job.phase = "verifying".into();
            db.save_push(&self.job).map_err(|_| uncertain())?;
            progress("Independently checking the exact remote branch", None);
            let observed = runner.remote(
                &self.stage,
                &self.job.url,
                &self.job.destination,
                &AtomicBool::new(false),
            );
            self.job.remote_after = observed.as_ref().ok().cloned().flatten();
            if self.job.remote_after.as_ref() == Some(&self.job.head) {
                self.job.state = "published_verified".into();
            } else {
                let error = match dispatched {
                    Err(e) if e.code == ErrorCode::PushRejected && observed.is_ok() => {
                        self.job.state = "rejected".into();
                        e
                    }
                    _ => {
                        self.job.state = "uncertain".into();
                        uncertain()
                    }
                };
                self.job.error = Some(error.clone());
                self.job.completed_at = Some(now());
                db.save_push(&self.job)?;
                return Err(error);
            }
        }
        // Refresh tracking independently. A failure here must not turn a verified
        // remote result into a claimed synchronized local cache.
        let fresh = AtomicBool::new(false);
        sync::inspect(db, git, self.job.project_id, &fresh).map_err(|_| uncertain())?;
        let token = db
            .sync_record(self.job.project_id)?
            .assessment
            .ok_or_else(uncertain)?
            .token;
        sync::fetch_held(
            db,
            git,
            self.job.project_id,
            &token,
            &self.job.remote,
            &fresh,
            progress,
        )
        .map_err(|_| uncertain())?;
        self.job.phase = "verified".into();
        self.job.completed_at = Some(now());
        db.save_push(&self.job).map_err(|_| uncertain())?;
        progress(
            if self.job.state == "already_synchronized" {
                "Already synchronized; remote ref verified."
            } else {
                "Published exact commit; remote ref verified and sync refreshed."
            },
            Some(100),
        );
        Ok(())
    }
}
fn before_dispatch_state(e: &AppError) -> &'static str {
    match e.code {
        ErrorCode::Cancelled => "cancelled",
        ErrorCode::Authentication => "authentication_failed",
        ErrorCode::Network => "network_failed",
        _ => "blocked",
    }
}
// Separate authority from fetch: only seed, one fixed push refspec, and exact-ref
// lookup. Runs outside the user's Git config, using its validated object store.
struct PushRunner<'a> {
    git: &'a Git,
}
impl PushRunner<'_> {
    fn command(
        &self,
        stage: &Directory,
        url: &str,
        objects: Option<&Directory>,
    ) -> Result<Command> {
        stage.validate_path()?;
        let mut c = Command::new(self.git.executable());
        c.env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_ASKPASS", "/usr/bin/false")
            .env("SSH_ASKPASS", "/usr/bin/false")
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_HOST", "github.com");
        for key in ["HOME", "GH_CONFIG_DIR", "XDG_CONFIG_HOME"] {
            if let Some(v) = std::env::var_os(key).filter(|v| Path::new(v).is_absolute()) {
                c.env(key, v);
            }
        }
        for setting in [
            "core.hooksPath=/dev/null",
            "core.fsmonitor=false",
            "credential.helper=",
            "credential.useHttpPath=true",
            "protocol.allow=never",
            "push.followTags=false",
            "push.recurseSubmodules=no",
            "push.gpgSign=false",
            "gc.auto=0",
            "maintenance.auto=false",
            "http.followRedirects=false",
            "http.sslVerify=true",
            "core.pager=cat",
        ] {
            c.args(["-c", setting]);
        }
        c.args([
            "-c",
            if url.starts_with('/') {
                "protocol.file.allow=always"
            } else {
                "protocol.https.allow=always"
            },
        ]);
        let gh = crate::github::Github::default();
        c.args([
            "-c",
            &format!(
                "credential.https://github.com.helper={} auth git-credential",
                gh.executable().display()
            ),
        ]);
        if let Some(o) = objects {
            o.validate_path()?;
            c.env("GIT_OBJECT_DIRECTORY", &o.path);
        }
        let fd = stage.file.as_raw_fd();
        unsafe {
            c.pre_exec(move || {
                if libc::fchdir(fd) == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
        c.process_group(0);
        Ok(c)
    }
    fn run(&self, c: Command, cancel: &AtomicBool) -> Result<Vec<u8>> {
        check_cancel(cancel)?;
        crate::git_process::supervise(
            c,
            cancel,
            Instant::now() + Duration::from_secs(120),
            false,
            &mut |_, _| {},
            None,
            ErrorCode::PushFailed,
        )
        .map(|(_, b)| b)
    }
    fn seed(
        &self,
        stage: &Directory,
        w: &LockedWorkspace,
        head: &str,
        cancel: &AtomicBool,
    ) -> Result<()> {
        if !sync::oid(head) {
            return Err(stale());
        }
        let objects = w.meta.child("objects")?;
        let mut c = self.command(stage, "", Some(&objects))?;
        c.args(["update-ref", "refs/heads/stow-publish", head]);
        self.run(c, cancel).map(|_| ())
    }
    fn push(
        &self,
        stage: &Directory,
        w: &LockedWorkspace,
        url: &str,
        destination: &str,
        cancel: &AtomicBool,
    ) -> Result<()> {
        let objects = w.meta.child("objects")?;
        let mut c = self.command(stage, url, Some(&objects))?;
        c.args([
            "push",
            "--porcelain",
            "--no-force",
            "--no-follow-tags",
            "--no-verify",
            "--recurse-submodules=no",
            "--signed=false",
            "--",
            url,
            &format!("refs/heads/stow-publish:{destination}"),
        ]);
        self.run(c, cancel).map(|_| ())
    }
    fn remote(
        &self,
        stage: &Directory,
        url: &str,
        destination: &str,
        cancel: &AtomicBool,
    ) -> Result<Option<String>> {
        let mut c = self.command(stage, url, None)?;
        c.args(["ls-remote", "--refs", "--", url, destination]);
        let bytes = self.run(c, cancel)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| uncertain())?;
        let lines: Vec<_> = text.lines().collect();
        if lines.is_empty() {
            return Ok(None);
        }
        if lines.len() != 1 {
            return Err(uncertain());
        }
        let (oid, r) = lines[0].split_once('\t').ok_or_else(uncertain)?;
        if !sync::oid(oid) || r != destination {
            return Err(uncertain());
        }
        Ok(Some(oid.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn authentication_and_network_errors_are_redacted_before_dispatch() {
        for (diagnostic, code) in [
            ("Authentication failed secret", ErrorCode::Authentication),
            ("Could not resolve host secret", ErrorCode::Network),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().canonicalize().unwrap();
            let script = path.join("git-fixture");
            std::fs::write(
                &script,
                format!("#!/bin/sh\necho '{diagnostic}' >&2\nexit 1\n"),
            )
            .unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
            let git = Git::new(script);
            let stage = Directory::open(&path).unwrap();
            let e = PushRunner { git: &git }
                .remote(
                    &stage,
                    "https://github.com/example/fixture.git",
                    "refs/heads/main",
                    &AtomicBool::new(false),
                )
                .unwrap_err();
            assert_eq!(e.code, code);
            assert!(!e.message.contains("secret"));
            assert_ne!(before_dispatch_state(&e), "uncertain");
        }
    }
}
