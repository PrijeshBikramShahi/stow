//! Creation-only Git workflow. Never fetches/checks out into a registered workspace.
use crate::{
    core::{now, AppError, ErrorCode, Inspection, Result},
    database::Registry,
    git::{check_cancel, Git},
    github::{Github, Repository},
    safe_fs::{self, Directory, Identity},
};
use serde::{Deserialize, Serialize};
use std::{
    os::{fd::AsRawFd, unix::process::CommandExt},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Preferences {
    pub root: Option<String>,
    pub editor: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloneJob {
    pub id: String,
    pub repository: Repository,
    pub root: String,
    pub root_identity: Identity,
    pub staging: String,
    pub stage_identity: Option<Identity>,
    pub repo_identity: Option<Identity>,
    pub name: String,
    pub destination: String,
    pub phase: String,
    pub state: String,
    pub updated_at: i64,
    pub verified: Option<Inspection>,
    pub project_id: Option<i64>,
    pub error: Option<AppError>,
    pub editor_error: Option<AppError>,
}
#[derive(Serialize)]
pub struct Destination {
    pub root: String,
    pub destination: String,
}
pub fn validate_destination(root: &str, name: &str) -> Result<Destination> {
    safe_fs::component(name)?;
    let directory = Directory::open(Path::new(root))?;
    directory.owned()?;
    directory.absent(name)?;
    Ok(Destination {
        root: crate::core::path_string(&directory.path)?,
        destination: crate::core::path_string(&directory.path.join(name))?,
    })
}
fn error(code: ErrorCode, message: &str) -> AppError {
    AppError::new(code, message)
}
fn journal(db: &Registry, job: &mut CloneJob, phase: &str) -> Result<()> {
    job.phase = phase.into();
    job.updated_at = now();
    db.save_clone(job)
}
fn failed(db: &Registry, job: &mut CloneJob, e: &AppError) {
    job.state = if e.code == ErrorCode::Cancelled {
        "cancelled"
    } else {
        "failed"
    }
    .into();
    job.error = Some(e.clone());
    job.updated_at = now();
    let _ = db.save_clone(job);
}
trait Source {
    fn verify(&self, repo: &Repository, cancel: &AtomicBool) -> Result<Repository>;
    fn populate(
        &self,
        repo: &Repository,
        directory: &Directory,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(&str, Option<u8>),
    ) -> Result<()>;
}
struct GithubSource<'a> {
    git: &'a Git,
    github: Github,
}
impl Source for GithubSource<'_> {
    fn verify(&self, repo: &Repository, cancel: &AtomicBool) -> Result<Repository> {
        self.github.verify_repository(repo, cancel)
    }
    fn populate(
        &self,
        repo: &Repository,
        directory: &Directory,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(&str, Option<u8>),
    ) -> Result<()> {
        populate(
            &CloneRunner::new(self.git, &self.github),
            repo,
            directory,
            cancel,
            progress,
        )
    }
}
pub fn execute(
    db: &Registry,
    git: &Git,
    repo: &Repository,
    root: &str,
    name: &str,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, Option<u8>),
) -> Result<CloneJob> {
    execute_with(
        db,
        git,
        &GithubSource {
            git,
            github: Github::default(),
        },
        repo,
        root,
        name,
        cancel,
        progress,
    )
}
#[allow(clippy::too_many_arguments)]
fn execute_with(
    db: &Registry,
    git: &Git,
    source: &dyn Source,
    repo: &Repository,
    root: &str,
    name: &str,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, Option<u8>),
) -> Result<CloneJob> {
    let preview = validate_destination(root, name)?;
    let root = Directory::open(Path::new(&preview.root))?;
    root.owned()?;
    root.lock()?;
    root.absent(name)?;
    check_cancel(cancel)?;
    if db.list()?.iter().any(|p| p.path == preview.destination) {
        return Err(error(ErrorCode::Collision,"This destination already has a registration. Review that registration or choose another name."));
    }
    progress("Checking live GitHub repository identity", None);
    let repository = source.verify(repo, cancel)?;
    root.validate_path()?;
    root.absent(name)?;
    check_cancel(cancel)?;
    let id = safe_fs::random_id()?;
    let staging = format!(".stow-clone-{id}");
    let mut job = CloneJob {
        id,
        repository,
        root: preview.root,
        root_identity: root.identity()?,
        staging,
        stage_identity: None,
        repo_identity: None,
        name: name.into(),
        destination: preview.destination,
        phase: "reserved".into(),
        state: "running".into(),
        updated_at: now(),
        verified: None,
        project_id: None,
        error: None,
        editor_error: None,
    };
    // Persist the intended location BEFORE creating any data. If later DB writes
    // fail, this record still identifies retained staging data for manual review.
    db.save_clone(&job)?;
    let outcome = (|| {
        let stage = root.create(&job.staging)?;
        job.stage_identity = Some(stage.identity()?);
        journal(db, &mut job, "staging")?;
        let repo_dir = stage.create("workspace")?;
        job.repo_identity = Some(repo_dir.identity()?);
        journal(db, &mut job, "creating")?;
        progress("Creating workspace in private staging directory", None);
        source.populate(&job.repository, &repo_dir, cancel, progress)?;
        progress("Verifying cloned workspace", None);
        let current = source.verify(&job.repository, cancel)?;
        if current.default_branch != job.repository.default_branch {
            return Err(error(ErrorCode::ProviderData,"GitHub's default branch changed during cloning. Staging is retained; refresh and retry."));
        }
        root.validate_path()?;
        stage.validate_path()?;
        repo_dir.validate_path()?;
        let inspection = verify(git, &repo_dir, &job, cancel)?;
        job.verified = Some(inspection);
        journal(db, &mut job, "verified")?;
        finish(db, git, &root, &stage, &mut job, cancel, progress)
    })();
    if let Err(e) = outcome {
        failed(db, &mut job, &e);
        return Err(e);
    }
    Ok(job)
}
fn verify(
    git: &Git,
    directory: &Directory,
    job: &CloneJob,
    cancel: &AtomicBool,
) -> Result<Inspection> {
    directory.validate_path()?;
    if Some(directory.identity()?) != job.repo_identity {
        return Err(safe_fs::changed());
    }
    // No linked-worktree or redirected metadata is accepted for a clone.
    directory.child(".git")?;
    let (_, inspection) = git.inspect(&directory.path, cancel)?;
    directory.validate_path()?;
    if !inspection.working_tree.is_clean()
        || inspection.detached
        || inspection.remotes.len() != 1
        || inspection.remotes[0].name != "origin"
        || inspection.remotes[0].url != job.repository.https_url
    {
        return Err(error(ErrorCode::RecoveryRequired,"The created workspace failed verification. It was not registered. Review the retained staging data."));
    }
    if let Some(previous) = &job.verified {
        if previous.commit != inspection.commit || previous.branch != inspection.branch {
            return Err(error(ErrorCode::RecoveryRequired,"The verified workspace changed. Recovery will not reset or overwrite it; review it manually."));
        }
    }
    Ok(inspection)
}
fn finish(
    db: &Registry,
    git: &Git,
    root: &Directory,
    stage: &Directory,
    job: &mut CloneJob,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, Option<u8>),
) -> Result<()> {
    check_cancel(cancel)?;
    let expected = job.repo_identity.ok_or_else(safe_fs::changed)?;
    if job.phase != "promoted" {
        let repo_dir = stage.child("workspace")?;
        verify(git, &repo_dir, job, cancel)?;
        root.validate_path()?;
        root.absent(&job.name)?;
        journal(db, job, "promoting")?;
        check_cancel(cancel)?;
        progress("Promoting verified workspace without replacing files", None);
        check_cancel(cancel)?;
        root.owned()?;
        stage.owned()?;
        stage.promote("workspace", expected, root, &job.name)?;
        // If this journal write fails, 'promoting' + inode identity resolves the
        // filesystem/SQLite crash gap during explicit recovery.
        journal(db, job, "promoted")?;
    }
    progress("Registering promoted workspace", None);
    let promoted = root.child(&job.name)?;
    let inspection = verify(git, &promoted, job, cancel)?;
    root.validate_path()?;
    check_cancel(cancel)?;
    db.register_clone(job, &inspection)?;
    progress("Clone registered; staging journal retained", Some(100));
    Ok(())
}
pub fn recover(
    db: &Registry,
    git: &Git,
    id: &str,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, Option<u8>),
) -> Result<CloneJob> {
    let mut job = db.clone_job(id)?;
    if job.state == "completed" {
        return Ok(job);
    }
    if job.verified.is_none()
        || !["verified", "promoting", "promoted"].contains(&job.phase.as_str())
    {
        return Err(error(ErrorCode::RecoveryRequired,"This clone did not finish verification. Keep its staging data for review and start a new clone; incomplete data is never promoted."));
    }
    let root = Directory::open(Path::new(&job.root))?;
    root.owned()?;
    root.lock()?;
    if root.identity()? != job.root_identity {
        return Err(safe_fs::changed());
    }
    let stage = root.child(&job.staging)?;
    stage.owned()?;
    if Some(stage.identity()?) != job.stage_identity {
        return Err(safe_fs::changed());
    }
    // A crash can occur after exclusive rename but before persisting 'promoted'.
    match root.absent(&job.name) {
        Ok(()) if job.phase != "promoted" => {}
        Ok(()) => return Err(safe_fs::changed()),
        Err(e) if e.code == ErrorCode::Collision => {
            if Some(root.child(&job.name)?.identity()?) != job.repo_identity {
                return Err(e);
            }
            job.phase = "promoted".into();
        }
        Err(e) => return Err(e),
    }
    job.state = "running".into();
    job.error = None;
    db.save_clone(&job)?;
    if let Err(e) = finish(db, git, &root, &stage, &mut job, cancel, progress) {
        failed(db, &mut job, &e);
        return Err(e);
    }
    Ok(job)
}
pub fn review_path(job: &CloneJob) -> Result<PathBuf> {
    let root = Directory::open(Path::new(&job.root))?;
    if root.identity()? != job.root_identity {
        return Err(safe_fs::changed());
    }
    if let Ok(destination) = root.child(&job.name) {
        if Some(destination.identity()?) == job.repo_identity {
            return Ok(destination.path);
        }
    }
    let stage = root.child(&job.staging)?;
    if Some(stage.identity()?) != job.stage_identity {
        return Err(error(ErrorCode::RecoveryRequired,"Staging ownership could not be confirmed. Review the recorded path manually in Finder."));
    }
    Ok(stage.path)
}

struct CloneRunner {
    git: PathBuf,
    gh: PathBuf,
    timeout: Duration,
    #[cfg(test)]
    local_source: Option<PathBuf>,
}
impl CloneRunner {
    fn new(git: &Git, gh: &Github) -> Self {
        Self {
            git: git.executable().into(),
            gh: gh.executable().into(),
            timeout: Duration::from_secs(900),
            #[cfg(test)]
            local_source: None,
        }
    }
    fn command(&self, directory: &Directory) -> Result<Command> {
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
            .env("GIT_LFS_SKIP_SMUDGE", "1")
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("GH_HOST", "github.com")
            .env("GIT_ASKPASS", "/usr/bin/false")
            .env("SSH_ASKPASS", "/usr/bin/false");
        for key in ["HOME", "GH_CONFIG_DIR", "XDG_CONFIG_HOME"] {
            if let Some(value) = std::env::var_os(key).filter(|v| PathBuf::from(v).is_absolute()) {
                cmd.env(key, value);
            }
        }
        for option in [
            "core.hooksPath=/dev/null",
            "core.fsmonitor=false",
            "core.attributesFile=/dev/null",
            "core.excludesFile=/dev/null",
            "core.protectHFS=true",
            "core.protectNTFS=true",
            "core.pager=cat",
            "submodule.recurse=false",
            "fetch.recurseSubmodules=false",
            "protocol.allow=never",
            "protocol.https.allow=always",
            "credential.helper=",
            "credential.useHttpPath=true",
            "http.followRedirects=false",
            "http.sslVerify=true",
            "fetch.fsckObjects=true",
            "transfer.fsckObjects=true",
            "maintenance.auto=false",
            "gc.auto=0",
        ] {
            cmd.args(["-c", option]);
        }
        let gh = self.gh.to_str().ok_or_else(|| {
            error(
                ErrorCode::GithubUnavailable,
                "Unsupported GitHub CLI installation path.",
            )
        })?;
        // Git invokes this fixed trusted credential helper; Stow never handles its
        // stdin/stdout credentials. There is no user-controlled shell fragment.
        if ![
            "/opt/homebrew/bin/gh",
            "/usr/local/bin/gh",
            "/opt/local/bin/gh",
            "/usr/bin/gh",
        ]
        .contains(&gh)
        {
            return Err(error(
                ErrorCode::GithubUnavailable,
                "Unsupported GitHub CLI installation path.",
            ));
        }
        cmd.args([
            "-c",
            &format!("credential.https://github.com.helper={gh} auth git-credential"),
        ]);
        #[cfg(test)]
        if self.local_source.is_some() {
            cmd.args(["-c", "protocol.file.allow=always"]);
        }
        let fd = directory.file.as_raw_fd();
        // SAFETY: directory remains alive through spawn; fchdir is async-signal-safe.
        // CLOEXEC closes the inherited descriptor only after the child has chdir'd.
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
        Ok(cmd)
    }
    fn run(
        &self,
        dir: &Directory,
        args: &[&str],
        cancel: &AtomicBool,
        deadline: Instant,
        allow_missing: bool,
        progress: &mut dyn FnMut(&str, Option<u8>),
    ) -> Result<(bool, Vec<u8>)> {
        check_cancel(cancel)?;
        dir.validate_path()?;
        let mut cmd = self.command(dir)?;
        cmd.args(args);
        crate::git_process::supervise(
            cmd,
            cancel,
            deadline,
            allow_missing,
            progress,
            None,
            ErrorCode::CloneFailed,
        )
    }
}

fn populate(
    runner: &CloneRunner,
    repo: &Repository,
    dir: &Directory,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, Option<u8>),
) -> Result<()> {
    let deadline = Instant::now() + runner.timeout;
    let branch = repo.default_branch.as_deref().unwrap_or("main");
    let head = format!("refs/heads/{branch}");
    let remote = format!("refs/remotes/origin/{branch}");
    let mut run =
        |args: &[&str], missing: bool| runner.run(dir, args, cancel, deadline, missing, progress);
    run(&["check-ref-format", &head], false)?;
    run(
        &["init", "--template=", "--initial-branch=main", "."],
        false,
    )?;
    run(
        &["config", "--local", "remote.origin.url", &repo.https_url],
        false,
    )?;
    run(
        &[
            "config",
            "--local",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
        false,
    )?;
    let source = repo.https_url.clone();
    #[cfg(test)]
    let source = runner
        .local_source
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or(source);
    // Creation-only fetch into our exclusively created repository. Unlike git clone,
    // a failed fetch does not remove the repository and its partially received data.
    run(
        &[
            "fetch",
            "--atomic",
            "--no-recurse-submodules",
            "--no-auto-maintenance",
            "--progress",
            "--",
            &source,
            "+refs/heads/*:refs/remotes/origin/*",
        ],
        false,
    )?;
    let (exists, commit) = run(
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{remote}^{{commit}}"),
        ],
        true,
    )?;
    if !exists {
        let (_, refs) = run(&["for-each-ref", "--format=%(objectname)"], false)?;
        if !refs.is_empty() {
            return Err(error(ErrorCode::CloneFailed,"The remote default branch was unavailable. Refresh GitHub metadata and retry; staging is retained."));
        }
    }
    run(&["symbolic-ref", "HEAD", &head], false)?;
    if exists {
        let hash = std::str::from_utf8(&commit)
            .map_err(|_| error(ErrorCode::CloneFailed, "Invalid cloned commit identity."))?
            .trim();
        if hash.len() != 40 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(error(
                ErrorCode::CloneFailed,
                "Unsupported cloned object identity.",
            ));
        }
        run(&["update-ref", &head, hash], false)?;
        run(&["read-tree", "--reset", "-u", "HEAD"], false)?;
        run(
            &["symbolic-ref", "refs/remotes/origin/HEAD", &remote],
            false,
        )?;
        run(
            &[
                "config",
                "--local",
                &format!("branch.{branch}.remote"),
                "origin",
            ],
            false,
        )?;
        run(
            &[
                "config",
                "--local",
                &format!("branch.{branch}.merge"),
                &head,
            ],
            false,
        )?;
    }
    dir.file.sync_all().map_err(AppError::io)?;
    Ok(())
}

#[cfg(test)]
#[path = "clone_tests.rs"]
mod tests;

#[cfg(test)]
use crate::git_process::drain;
