//! Local synchronization assessment and explicitly authorized, staged fetch.
use crate::{
    core::{now, AppError, ErrorCode, Inspection, Result},
    database::Registry,
    git::{self, check_cancel, Git},
    safe_fs::{self, Directory, Identity},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    os::{
        fd::AsRawFd,
        unix::{fs::MetadataExt, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
fn error(code: ErrorCode, message: &str) -> AppError {
    AppError::new(code, message)
}
fn unsupported(message: &str) -> AppError {
    error(ErrorCode::Unsupported, message)
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceIdentity {
    pub root: Identity,
    pub git: Identity,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FetchRemote {
    pub name: String,
    pub url: String,
    pub reason: Option<String>,
    #[serde(default)]
    pub fetch_key: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assessment {
    pub token: String,
    pub inspected_at: i64,
    pub inspection: Inspection,
    pub identity: Option<WorkspaceIdentity>,
    pub upstream_ref: Option<String>,
    pub upstream_commit: Option<String>,
    pub ahead: Option<u64>,
    pub behind: Option<u64>,
    pub relation: String,
    pub operations: Vec<String>,
    pub remotes: Vec<FetchRemote>,
    pub warnings: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncRecord {
    pub project_id: i64,
    pub assessment: Option<Assessment>,
    pub error: Option<AppError>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchJob {
    pub id: String,
    pub project_id: i64,
    pub remote: String,
    pub url: String,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    pub state: String,
    pub phase: String,
    pub staging: Option<String>,
    #[serde(default)]
    pub fetched_refs: BTreeMap<String, String>,
    pub error: Option<AppError>,
}
#[derive(Serialize)]
pub struct SyncView {
    pub record: SyncRecord,
    pub history: Vec<FetchJob>,
    pub pulls: Vec<crate::pull::PullJob>,
}
pub(crate) fn config(git: &Git, path: &Path, cancel: &AtomicBool) -> Result<Vec<(String, String)>> {
    let marker = path.join(".git");
    let meta = fs::symlink_metadata(&marker).map_err(AppError::io)?;
    let config = git::repository_config(path, &marker, meta.is_file())?;
    let (_, bytes) = git.run(
        Path::new("/"),
        &[
            "config",
            "--file",
            &crate::core::path_string(&config)?,
            "--no-includes",
            "--null",
            "--list",
        ],
        cancel,
        &[],
    )?;
    let values = git::parse_config(&bytes)?;
    git::validate_config(&values)?;
    Ok(values)
}
pub(crate) fn values<'a>(config: &'a [(String, String)], key: &str) -> Vec<&'a str> {
    config
        .iter()
        .filter(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .collect()
}
pub(crate) fn output(git: &Git, path: &Path, args: &[&str], cancel: &AtomicBool) -> Result<String> {
    git::text_line(git.run(path, args, cancel, &[])?.1)
}
pub(crate) fn oid(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn operation_markers(gitdir: &Path) -> Vec<String> {
    [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "REBASE_HEAD",
        "rebase-apply",
        "rebase-merge",
        "sequencer",
        "BISECT_LOG",
        "index.lock",
        "HEAD.lock",
        "packed-refs.lock",
        "config.lock",
        "shallow.lock",
    ]
    .iter()
    .filter(|name| fs::symlink_metadata(gitdir.join(name)).is_ok())
    .map(|s| s.to_string())
    .collect()
}
fn remote_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && !name.starts_with(['.', '-'])
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
}
fn transport(url: &str) -> Result<bool> {
    if let Some(rest) = url.strip_prefix("https://github.com/") {
        let Some((owner, name)) = rest.split_once('/') else {
            return Err(unsupported(
                "Use a canonical https://github.com/owner/repository.git remote.",
            ));
        };
        let Some(name) = name.strip_suffix(".git") else {
            return Err(unsupported(
                "Use a canonical GitHub HTTPS URL ending in .git.",
            ));
        };
        if owner.is_empty()
            || name.is_empty()
            || owner.len() > 100
            || name.len() > 100
            || !owner
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
            || name == "."
            || name == ".."
        {
            return Err(unsupported(
                "Remote URL contains unsupported identity or credential syntax.",
            ));
        }
        return Ok(false);
    }
    if url.starts_with('/') {
        Directory::open(Path::new(url))?.owned()?;
        return Ok(true);
    }
    Err(unsupported("Fetch supports canonical GitHub HTTPS and absolute local bare-repository paths. SSH, custom helpers, URL credentials, and other transports are unsupported."))
}
// A closed config vocabulary prevents new Git transport/helper settings from gaining
// authority accidentally. Standard '+' fetch mappings are recognized but never executed.
pub(crate) fn fetch_config(config: &[(String, String)], remote: &str) -> Result<String> {
    if !remote_name(remote) {
        return Err(unsupported(
            "Remote name must use letters, numbers, hyphens, or underscores.",
        ));
    }
    for (key, _) in config {
        let allowed = [
            "core.repositoryformatversion",
            "core.filemode",
            "core.bare",
            "core.logallrefupdates",
            "core.ignorecase",
            "core.precomposeunicode",
            "core.symlinks",
            "user.name",
            "user.email",
        ]
        .contains(&key.as_str())
            || key.starts_with("remote.")
                && [".url", ".fetch", ".pushurl"]
                    .iter()
                    .any(|end| key.ends_with(end))
            || key.starts_with("branch.")
                && [".remote", ".merge"].iter().any(|end| key.ends_with(end));
        if !allowed {
            return Err(unsupported("Fetch requires ordinary Git configuration. Includes, helpers, filters, URL rewrites, hooks, SSH/HTTP overrides, maintenance, and extra settings are unsupported; review configuration in your terminal. Stow will not rewrite it."));
        }
    }
    let urls = values(config, &format!("remote.{remote}.url"));
    if urls.len() != 1 {
        return Err(unsupported(
            "The selected remote must have exactly one fetch URL.",
        ));
    }
    let specs = values(config, &format!("remote.{remote}.fetch"));
    let expected = format!("refs/heads/*:refs/remotes/{remote}/*");
    if specs.len() != 1 || specs[0].strip_prefix('+').unwrap_or(specs[0]) != expected {
        return Err(unsupported("Fetch requires one ordinary heads-to-remote-tracking mapping. Custom, negative, mirror, or branch-writing refspecs are unsupported."));
    }
    transport(urls[0])?;
    Ok(urls[0].into())
}
fn identity(path: &Path) -> Result<WorkspaceIdentity> {
    let root = Directory::open(path)?;
    let meta = root.child(".git")?;
    Ok(WorkspaceIdentity {
        root: root.identity()?,
        git: meta.identity()?,
    })
}
pub fn assess(git: &Git, path: &Path, cancel: &AtomicBool) -> Result<Assessment> {
    let (_, inspection) = git.inspect(path, cancel)?;
    let cfg = config(git, path, cancel)?;
    let gitdir = PathBuf::from(output(
        git,
        path,
        &["rev-parse", "--absolute-git-dir"],
        cancel,
    )?);
    let operations = operation_markers(&gitdir);
    let mut warnings=vec!["Counts compare locally cached tracking refs, not live remote state. Clean files and an upstream do not prove recoverability. Fetch is explicit and never pushes.".into()];
    let mut result = Assessment {
        token: safe_fs::random_id()?,
        inspected_at: now(),
        inspection,
        identity: identity(path).ok(),
        upstream_ref: None,
        upstream_commit: None,
        ahead: None,
        behind: None,
        relation: "no_upstream".into(),
        operations,
        remotes: vec![],
        warnings: vec![],
    };
    let mut names: Vec<_> = result
        .inspection
        .remotes
        .iter()
        .map(|r| r.name.clone())
        .collect();
    names.sort();
    names.dedup();
    for name in names {
        let raw = values(&cfg, &format!("remote.{name}.url"));
        let url = raw
            .first()
            .map(|v| git::redact_remote(v))
            .unwrap_or_default();
        let checked = fetch_config(&cfg, &name);
        let fetch_key = checked.as_ref().ok().cloned();
        let mut reason = checked.err().map(|e| e.message);
        if result.identity.is_none() {
            reason=Some("Fetch requires an ordinary .git directory with no symlink path components. Linked worktrees remain inspection-only.".into());
        }
        result.remotes.push(FetchRemote {
            name,
            url,
            reason,
            fetch_key,
        });
    }
    if !result.operations.is_empty() {
        warnings.push("A Git operation or lock is present. Finish it in your editor/terminal before fetching.".into());
    }
    let unsupported_history = gitdir.join("shallow").exists()
        || gitdir.join("info/grafts").exists()
        || !output(
            git,
            path,
            &["for-each-ref", "--format=%(refname)", "refs/replace/"],
            cancel,
        )?
        .is_empty();
    if unsupported_history {
        result.relation = "unsupported".into();
        warnings.push("Shallow, grafted, or replace-ref history is unsupported for synchronization assessment and fetch.".into());
    } else if result.inspection.detached {
        result.relation = "detached".into();
    } else if let Some(branch) = &result.inspection.branch {
        let remotes = values(&cfg, &format!("branch.{branch}.remote"));
        let merges = values(&cfg, &format!("branch.{branch}.merge"));
        if remotes.is_empty() && merges.is_empty() {
        } else if remotes.len() != 1
            || merges.len() != 1
            || remotes[0] == "."
            || !merges[0].starts_with("refs/heads/")
        {
            result.relation = "ambiguous_upstream".into();
        } else {
            let full = format!("refs/heads/{branch}");
            let data = output(
                git,
                path,
                &[
                    "for-each-ref",
                    "--format=%(refname)%09%(upstream)",
                    "--",
                    &full,
                ],
                cancel,
            )?;
            let upstream = data.lines().find_map(|line| {
                line.split_once('\t')
                    .filter(|(r, _)| *r == full)
                    .map(|(_, u)| u.to_owned())
            });
            if let Some(upstream) = upstream.filter(|s| !s.is_empty()) {
                if !upstream.starts_with(&format!("refs/remotes/{}/", remotes[0])) {
                    result.relation = "ambiguous_upstream".into();
                } else {
                    result.upstream_ref = Some(upstream.clone());
                    let (code, bytes) = git.run(
                        path,
                        &[
                            "rev-parse",
                            "--verify",
                            "--quiet",
                            "--end-of-options",
                            &format!("{upstream}^{{commit}}"),
                        ],
                        cancel,
                        &[1],
                    )?;
                    if code == 1 {
                        result.relation = "missing_tracking_ref".into();
                    } else {
                        let commit = git::text_line(bytes)?;
                        if !oid(&commit) {
                            return Err(unsupported("Unsupported tracking object format."));
                        }
                        result.upstream_commit = Some(commit.clone());
                        if let Some(head) = &result.inspection.commit {
                            let counts = output(
                                git,
                                path,
                                &[
                                    "rev-list",
                                    "--left-right",
                                    "--count",
                                    &format!("{head}...{commit}"),
                                    "--",
                                ],
                                cancel,
                            )?;
                            let parts: Vec<_> = counts.split_whitespace().collect();
                            if parts.len() != 2 {
                                return Err(unsupported(
                                    "Unexpected revision relationship response.",
                                ));
                            }
                            let ahead = parts[0]
                                .parse::<u64>()
                                .map_err(|_| unsupported("Invalid ahead count."))?;
                            let behind = parts[1]
                                .parse::<u64>()
                                .map_err(|_| unsupported("Invalid behind count."))?;
                            result.ahead = Some(ahead);
                            result.behind = Some(behind);
                            result.relation = match (ahead, behind) {
                                (0, 0) => "up_to_date",
                                (0, _) => "behind",
                                (_, 0) => "ahead",
                                _ => "diverged",
                            }
                            .into();
                        } else {
                            result.relation = "unborn".into();
                        }
                    }
                }
            } else {
                result.relation = if result.inspection.commit.is_none() {
                    "unborn"
                } else {
                    "missing_tracking_ref"
                }
                .into();
            }
        }
    }
    result.warnings = warnings;
    Ok(result)
}
pub fn inspect(db: &Registry, git: &Git, id: i64, cancel: &AtomicBool) -> Result<()> {
    let project = db.get(id)?;
    let previous = db.sync_record(id)?;
    let outcome = assess(git, Path::new(&project.path), cancel).and_then(|assessment| {
        if let Some(old) = previous
            .assessment
            .as_ref()
            .and_then(|a| a.identity.as_ref())
        {
            if assessment.identity.as_ref() != Some(old) {
                return Err(safe_fs::changed());
            }
        }
        db.register(Path::new(&project.path), &assessment.inspection)?;
        db.save_sync(&SyncRecord {
            project_id: id,
            assessment: Some(assessment),
            error: None,
        })
    });
    if let Err(e) = &outcome {
        if e.code != ErrorCode::Cancelled {
            db.save_sync(&SyncRecord {
                error: Some(e.clone()),
                ..previous
            })?;
        }
    }
    outcome
}
// Fetch-specific closed policy. It never starts from repository-controlled PATH,
// and network commands run in a fresh bare staging repository with no local config.
struct FetchRunner {
    git: PathBuf,
    timeout: Duration,
}
impl FetchRunner {
    fn new(git: &Git) -> Self {
        Self {
            git: git.executable().into(),
            timeout: Duration::from_secs(900),
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn run(
        &self,
        dir: &Directory,
        args: &[&str],
        objects: Option<&Directory>,
        local: bool,
        input: Option<Vec<u8>>,
        cancel: &AtomicBool,
        deadline: Instant,
        progress: &mut dyn FnMut(&str, Option<u8>),
    ) -> Result<Vec<u8>> {
        check_cancel(cancel)?;
        dir.validate_path()?;
        let mut cmd = Command::new(&self.git);
        cmd.env_clear()
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
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("GH_HOST", "github.com");
        for key in ["HOME", "GH_CONFIG_DIR", "XDG_CONFIG_HOME"] {
            if let Some(value) = std::env::var_os(key).filter(|v| PathBuf::from(v).is_absolute()) {
                cmd.env(key, value);
            }
        }
        for setting in [
            "core.hooksPath=/dev/null",
            "core.fsmonitor=false",
            "core.pager=cat",
            "credential.helper=",
            "credential.useHttpPath=true",
            "protocol.allow=never",
            "submodule.recurse=false",
            "fetch.recurseSubmodules=false",
            "fetch.writeCommitGraph=false",
            "fetch.prune=false",
            "fetch.pruneTags=false",
            "remote.origin.prune=false",
            "gc.auto=0",
            "maintenance.auto=false",
            "fetch.fsckObjects=true",
            "transfer.fsckObjects=true",
            "http.followRedirects=false",
            "http.sslVerify=true",
            "core.logAllRefUpdates=false",
        ] {
            cmd.args(["-c", setting]);
        }
        cmd.args([
            "-c",
            if local {
                "protocol.file.allow=always"
            } else {
                "protocol.https.allow=always"
            },
        ]);
        let gh = crate::github::Github::default();
        let gh = gh
            .executable()
            .to_str()
            .ok_or_else(|| unsupported("Unsupported gh path."))?;
        cmd.args([
            "-c",
            &format!("credential.https://github.com.helper={gh} auth git-credential"),
        ]);
        if let Some(objects) = objects {
            objects.validate_path()?;
            cmd.env("GIT_OBJECT_DIRECTORY", &objects.path);
        }
        let fd = dir.file.as_raw_fd();
        unsafe {
            cmd.pre_exec(move || {
                if libc::fchdir(fd) == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
        cmd.process_group(0);
        cmd.args(args);
        crate::git_process::supervise(
            cmd,
            cancel,
            deadline,
            false,
            progress,
            input,
            ErrorCode::FetchFailed,
        )
        .map(|(_, bytes)| bytes)
    }
}
// Check only Git metadata (never worktree contents). Fail closed on redirection,
// hardlinked files, unsupported layout, active locks, and excessive metadata size.
fn metadata_tree(root: &Directory) -> Result<()> {
    let mut stack = vec![root.path.clone()];
    let mut count = 0;
    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(&path).map_err(AppError::io)? {
            let entry = entry.map_err(AppError::io)?;
            count += 1;
            if count > 100_000 {
                return Err(unsupported(
                    "Git metadata exceeds the safe validation limit.",
                ));
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(".stow-fetch-") {
                continue;
            }
            let meta = fs::symlink_metadata(entry.path()).map_err(AppError::io)?;
            if meta.file_type().is_symlink()
                || (!meta.is_dir() && !meta.is_file())
                || meta.uid() != unsafe { libc::geteuid() }
                || meta.mode() & 0o022 != 0
                || meta.is_file() && meta.nlink() != 1
            {
                return Err(unsupported("Fetch requires private ordinary Git metadata without symlinks, hardlinks, shared write access, or special files."));
            }
            if name.ends_with(".lock") {
                return Err(error(ErrorCode::Busy,"Git metadata is locked by another operation. Retry after it finishes; Stow will not remove locks."));
            }
            if meta.is_dir() {
                Directory::open(&entry.path())?.owned()?;
                stack.push(entry.path());
            }
        }
    }
    Ok(())
}
pub(crate) fn checked_layout(root: &Directory, meta: &Directory) -> Result<()> {
    root.owned()?;
    meta.owned()?;
    root.validate_path()?;
    meta.validate_path()?;
    for name in [
        "commondir",
        "gitdir",
        "worktrees",
        "shallow",
        "info/grafts",
        "objects/info/alternates",
        "objects/info/http-alternates",
    ] {
        if fs::symlink_metadata(meta.path.join(name)).is_ok() {
            return Err(unsupported("Fetch does not support linked/shared worktrees, shallow history, grafts, or alternate object stores."));
        }
    }
    metadata_tree(meta)?;
    if !operation_markers(&meta.path).is_empty() {
        return Err(error(
            ErrorCode::Busy,
            "A Git operation is in progress. Complete it before fetching.",
        ));
    }
    Ok(())
}
pub(crate) fn refs(
    git: &Git,
    path: &Path,
    prefix: &str,
    cancel: &AtomicBool,
) -> Result<BTreeMap<String, String>> {
    let data = output(
        git,
        path,
        &[
            "for-each-ref",
            "--format=%(refname)%09%(objectname)%09%(symref)",
            prefix,
        ],
        cancel,
    )?;
    let mut refs = BTreeMap::new();
    for line in data.lines() {
        let parts: Vec<_> = line.split('\t').collect();
        if parts.len() != 3 || !oid(parts[1]) {
            return Err(unsupported("Invalid tracking reference."));
        }
        if !parts[2].is_empty() {
            if parts[0] == format!("{prefix}HEAD")
                || prefix == "refs/remotes/" && parts[0].ends_with("/HEAD")
            {
                continue;
            }
            return Err(unsupported(
                "Symbolic remote-tracking branches are unsupported.",
            ));
        }
        refs.insert(parts[0].to_string(), parts[1].to_string());
    }
    Ok(refs)
}
pub fn fetch(
    db: &Registry,
    git: &Git,
    id: i64,
    token: &str,
    remote: &str,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, Option<u8>),
) -> Result<()> {
    fetch_with(
        db,
        git,
        &FetchRunner::new(git),
        id,
        token,
        remote,
        cancel,
        progress,
    )
}
#[allow(clippy::too_many_arguments)]
fn fetch_with(
    db: &Registry,
    git: &Git,
    runner: &FetchRunner,
    id: i64,
    token: &str,
    remote: &str,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, Option<u8>),
) -> Result<()> {
    fetch_impl(db, git, runner, id, token, remote, cancel, progress, false).map(|_| ())
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn fetch_held(
    db: &Registry,
    git: &Git,
    id: i64,
    token: &str,
    remote: &str,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, Option<u8>),
) -> Result<FetchJob> {
    fetch_impl(
        db,
        git,
        &FetchRunner::new(git),
        id,
        token,
        remote,
        cancel,
        progress,
        true,
    )
}
#[allow(clippy::too_many_arguments)]
fn fetch_impl(
    db: &Registry,
    git: &Git,
    runner: &FetchRunner,
    id: i64,
    token: &str,
    remote: &str,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, Option<u8>),
    held: bool,
) -> Result<FetchJob> {
    let project = db.get(id)?;
    let saved = db.sync_record(id)?;
    let saved = saved
        .assessment
        .filter(|a| a.token == token)
        .ok_or_else(|| {
            error(
                ErrorCode::RecoveryRequired,
                "Inspect sync again before authorizing fetch.",
            )
        })?;
    let root = Directory::open(Path::new(&project.path))?;
    let meta = root.child(".git")?;
    if !held {
        root.lock()?;
        meta.lock()?;
    }
    if saved.identity.as_ref()
        != Some(&WorkspaceIdentity {
            root: root.identity()?,
            git: meta.identity()?,
        })
    {
        return Err(safe_fs::changed());
    }
    checked_layout(&root, &meta)?;
    let (head_code, head_bytes) = git.run(
        &root.path,
        &["symbolic-ref", "--quiet", "HEAD"],
        cancel,
        &[1],
    )?;
    let head_ref = git::text_line(head_bytes)?;
    if head_code == 0 && !head_ref.starts_with("refs/heads/") {
        return Err(unsupported(
            "Fetch requires HEAD to be detached or point to a local branch.",
        ));
    }
    let current = assess(git, &root.path, cancel)?;
    if current.inspection.commit != saved.inspection.commit
        || current.inspection.branch != saved.inspection.branch
        || current.remotes != saved.remotes
        || current.upstream_ref != saved.upstream_ref
    {
        return Err(error(ErrorCode::RecoveryRequired,"Workspace HEAD, tracking configuration, or remote changed. Inspect sync and review again before fetching."));
    }
    if current.relation == "unsupported" || !current.operations.is_empty() {
        return Err(unsupported(
            "Resolve unsupported history or the current Git operation before fetching.",
        ));
    }
    let version = git.version()?;
    let mut parts = version
        .strip_prefix("git version ")
        .unwrap_or("")
        .split('.');
    let major = parts
        .next()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);
    let minor = parts
        .next()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);
    if (major, minor) < (2, 46) {
        return Err(unsupported("Fetch requires Git 2.46 or newer for safe symbolic HEAD verification. Update the installed Git executable and retry."));
    }
    let cfg = config(git, &root.path, cancel)?;
    let url = fetch_config(&cfg, remote)?;
    let local = transport(&url)?;
    let local_dir = if local {
        let dir = Directory::open(Path::new(&url))?;
        dir.owned()?;
        metadata_tree(&dir)?;
        for name in [
            "commondir",
            "gitdir",
            "worktrees",
            "shallow",
            "info/grafts",
            "objects/info/alternates",
            "objects/info/http-alternates",
            "refs/replace",
        ] {
            if fs::symlink_metadata(dir.path.join(name)).is_ok() {
                return Err(unsupported(
                    "Local source uses unsupported redirected or shallow history.",
                ));
            }
        }
        let source_cfg = config_bare(git, &dir, cancel)?;
        for (key, _) in &source_cfg {
            if ![
                "core.repositoryformatversion",
                "core.filemode",
                "core.bare",
                "core.ignorecase",
                "core.precomposeunicode",
                "core.logallrefupdates",
            ]
            .contains(&key.as_str())
            {
                return Err(unsupported("Local fetch sources must be ordinary bare repositories without extra configuration."));
            }
        }
        if values(&source_cfg, "core.bare") != vec!["true"] {
            return Err(unsupported(
                "Local fetch sources must be bare repositories.",
            ));
        }
        Some(dir)
    } else {
        None
    };
    let mut job = FetchJob {
        id: safe_fs::random_id()?,
        project_id: id,
        remote: remote.into(),
        url: url.clone(),
        started_at: now(),
        completed_at: None,
        state: "running".into(),
        phase: "reserved".into(),
        staging: None,
        fetched_refs: BTreeMap::new(),
        error: None,
    };
    let stage_name = format!(".stow-fetch-{}", job.id);
    job.staging = Some(meta.path.join(&stage_name).to_string_lossy().into_owned());
    db.save_fetch(&job)?;
    let outcome = (|| {
        progress("Validating workspace and preparing fetch", None);
        checked_layout(&root, &meta)?;
        let stage = meta.create(&stage_name)?;
        let objects = meta.child("objects")?;
        let deadline = Instant::now() + runner.timeout;
        runner.run(
            &stage,
            &["init", "--bare", "--template=", "."],
            None,
            false,
            None,
            cancel,
            deadline,
            progress,
        )?;
        let prefix = format!("refs/remotes/{remote}/");
        let before = refs(git, &root.path, &prefix, cancel)?;
        let mut seed = String::new();
        for (reference, hash) in &before {
            let suffix = reference
                .strip_prefix(&prefix)
                .ok_or_else(|| unsupported("Invalid remote ref."))?;
            seed.push_str(&format!("create refs/heads/{suffix} {hash}\n"));
        }
        if !seed.is_empty() {
            runner.run(
                &stage,
                &["update-ref", "--no-deref", "--stdin"],
                Some(&objects),
                false,
                Some(seed.into_bytes()),
                cancel,
                deadline,
                progress,
            )?;
        }
        job.phase = "transferring".into();
        db.save_fetch(&job)?;
        progress("Fetching selected remote into isolated tracking refs", None);
        if let Some(source) = &local_dir {
            source.validate_path()?;
        }
        let transfer = runner.run(
            &stage,
            &[
                "fetch",
                "--porcelain",
                "--verbose",
                "--atomic",
                "--no-force",
                "--no-prune",
                "--no-prune-tags",
                "--no-tags",
                "--no-recurse-submodules",
                "--no-auto-maintenance",
                "--no-write-fetch-head",
                "--refmap=",
                "--progress",
                "--",
                &url,
                "refs/heads/*:refs/heads/*",
            ],
            Some(&objects),
            local,
            None,
            cancel,
            deadline,
            progress,
        )?;
        // Porcelain reports only refs actually advertised by this fetch. Seeded refs
        // for deleted branches remain cached, but must never authorize a pull.
        for line in std::str::from_utf8(&transfer)
            .map_err(|_| unsupported("Invalid fetch report."))?
            .lines()
        {
            let fields: Vec<_> = line.get(2..).unwrap_or("").split_whitespace().collect();
            if !matches!(line.as_bytes().first(), Some(b' ' | b'=' | b'*'))
                || fields.len() != 3
                || !oid(fields[1])
                || !fields[2].starts_with("refs/heads/")
            {
                return Err(unsupported("Unexpected fetch report."));
            }
            job.fetched_refs.insert(fields[2].into(), fields[1].into());
        }
        // Staging refs use heads namespace, where Git rejects non-fast-forward updates.
        root.validate_path()?;
        meta.validate_path()?;
        stage.validate_path()?;
        objects.validate_path()?;
        checked_layout(&root, &meta)?;
        if config(git, &root.path, cancel)? != cfg {
            return Err(error(
                ErrorCode::RecoveryRequired,
                "Git configuration changed during fetch. Tracking refs were not applied.",
            ));
        }
        let after_local = assess(git, &root.path, cancel)?;
        if after_local.inspection.commit != current.inspection.commit
            || after_local.inspection.branch != current.inspection.branch
        {
            return Err(error(
                ErrorCode::RecoveryRequired,
                "HEAD changed concurrently. Tracking refs were not applied; inspect again.",
            ));
        }
        let fetched_bytes = runner.run(
            &stage,
            &[
                "for-each-ref",
                "--format=%(refname)%09%(objectname)",
                "refs/heads/",
            ],
            Some(&objects),
            false,
            None,
            cancel,
            deadline,
            progress,
        )?;
        let fetched = std::str::from_utf8(&fetched_bytes)
            .map_err(|_| unsupported("Non-UTF-8 fetched refs are unsupported."))?;
        let mut transaction = String::from("start\n");
        if head_code == 0 {
            // Lock/verify the symbolic HEAD without writing it. A separate branch
            // verify conflicts with Git's implicit HEAD assertion. Reinspection
            // detects concurrent branch commits; this transaction only writes refs/remotes.
            transaction.push_str(&format!("symref-verify HEAD {head_ref}\n"));
        } else if let Some(head) = &current.inspection.commit {
            transaction.push_str(&format!("verify HEAD {head}\n"));
        }

        // Atomic expected-old updates prevent concurrent tracking updates from being lost.
        for line in fetched.lines() {
            let (reference, hash) = line
                .split_once('\t')
                .ok_or_else(|| unsupported("Malformed fetched refs."))?;
            if !oid(hash) {
                return Err(unsupported("Invalid fetched object ID."));
            }
            let suffix = reference
                .strip_prefix("refs/heads/")
                .ok_or_else(|| unsupported("Unexpected fetched namespace."))?;
            if suffix == "HEAD" {
                return Err(unsupported(
                    "A remote branch named HEAD conflicts with the tracking alias.",
                ));
            }
            let target = format!("{prefix}{suffix}");
            let old = before
                .get(&target)
                .map(String::as_str)
                .unwrap_or("0000000000000000000000000000000000000000");
            transaction.push_str(&format!("update {target} {hash} {old}\n"));
        }
        transaction.push_str("prepare\ncommit\n");
        job.phase = "applying_refs".into();
        db.save_fetch(&job)?;
        check_cancel(cancel)?;
        progress("Applying verified fast-forward tracking updates", None);
        checked_layout(&root, &meta)?;
        if config(git, &root.path, cancel)? != cfg {
            return Err(error(
                ErrorCode::RecoveryRequired,
                "Git configuration changed before applying refs. Inspect again.",
            ));
        }
        runner.run(
            &meta,
            &["--git-dir=.", "update-ref", "--no-deref", "--stdin"],
            None,
            false,
            Some(transaction.into_bytes()),
            cancel,
            deadline,
            progress,
        )?;
        job.phase = "verifying".into();
        db.save_fetch(&job)?;
        inspect(db, git, id, cancel)?;
        let after = db
            .sync_record(id)?
            .assessment
            .ok_or_else(|| unsupported("Post-fetch inspection missing."))?;
        if after.inspection.commit != current.inspection.commit
            || after.inspection.branch != current.inspection.branch
        {
            return Err(error(ErrorCode::RecoveryRequired,"HEAD changed concurrently. Fetch is not recorded as successful; inspect the workspace."));
        }
        job.phase = "verified".into();
        job.state = "completed".into();
        job.completed_at = Some(now());
        db.save_fetch(&job)?;
        progress(
            "Fetch completed; cached tracking state refreshed. No push or pull performed.",
            Some(100),
        );
        Ok(())
    })();
    if let Err(e) = &outcome {
        job.state = if e.code == ErrorCode::Cancelled {
            "cancelled"
        } else {
            "failed"
        }
        .into();
        job.error = Some(e.clone());
        job.completed_at = Some(now());
        let _ = db.save_fetch(&job);
        // Reinspect with a fresh cancellation token: cancelled fetch may have committed
        // metadata just before termination. Never claim rollback or network success.
        let _ = inspect(db, git, id, &AtomicBool::new(false));
    }
    outcome.map(|()| job)
}
fn config_bare(git: &Git, dir: &Directory, cancel: &AtomicBool) -> Result<Vec<(String, String)>> {
    let file = dir.path.join("config");
    let meta = fs::symlink_metadata(&file).map_err(AppError::io)?;
    if !meta.is_file() || meta.len() > 1024 * 1024 {
        return Err(unsupported("Invalid bare source configuration."));
    }
    let (_, bytes) = git.run(
        Path::new("/"),
        &[
            "config",
            "--file",
            &crate::core::path_string(&file)?,
            "--no-includes",
            "--null",
            "--list",
        ],
        cancel,
        &[],
    )?;
    git::parse_config(&bytes)
}
#[cfg(test)]
#[path = "sync_tests.rs"]
mod tests;
