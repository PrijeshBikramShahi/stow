//! File-level authorization, isolated index edits, and compare-and-swap commits.
//! Diff bytes and commit messages are ephemeral; journals contain summaries only.
use crate::{
    core::{now, AppError, ErrorCode, Result},
    database::Registry,
    git::{self, check_cancel, Git},
    safe_fs::{self, Directory},
    sync,
    workspace::{reject_hooks, LockedWorkspace},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{MetadataExt, OpenOptionsExt},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{atomic::AtomicBool, Arc},
    thread,
};

const FILE_LIMIT: u64 = 8 * 1024 * 1024;
const TOTAL_LIMIT: usize = 32 * 1024 * 1024;
fn blocked(message: &str) -> AppError {
    AppError::new(ErrorCode::Unsupported, message)
}
fn stale() -> AppError {
    AppError::new(ErrorCode::RecoveryRequired,"Changes, index, branch, or configuration changed since review. Refresh changes and select again.")
}
fn uncertain() -> AppError {
    AppError::new(ErrorCode::RecoveryRequired,"Local mutation requires inspection. Refresh HEAD and changes, and inspect Git locks before retrying. Stow never resets files or retries automatically.")
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    pub original: Option<String>,
    pub staged: Option<String>,
    pub unstaged: Option<String>,
    pub untracked: bool,
    pub binary: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Author {
    pub name: String,
    pub email: String,
}
#[derive(Clone, Serialize)]
pub struct Review {
    pub token: String,
    pub project_id: i64,
    pub repository: String,
    pub branch: String,
    pub head: String,
    pub entries: Vec<Change>,
    pub author: Option<Author>,
    pub author_error: Option<AppError>,
}
#[derive(Clone, PartialEq, Eq)]
struct FileState {
    bytes: Vec<u8>,
    device: u64,
    inode: u64,
    mode: u32,
}
#[derive(Clone)]
pub struct Snapshot {
    pub view: Review,
    config: Vec<(String, String)>,
    index: FileState,
    files: BTreeMap<String, Option<FileState>>,
}
#[derive(Serialize)]
pub struct Diff {
    pub binary: bool,
    pub text: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ChangeJob {
    pub id: String,
    pub project_id: i64,
    pub kind: String,
    pub state: String,
    pub phase: String,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    pub branch: String,
    pub before_head: String,
    pub after_head: Option<String>,
    pub tree: Option<String>,
    pub paths: Vec<String>,
    pub error: Option<AppError>,
}
#[derive(Serialize)]
pub struct CommitPreview {
    pub job: ChangeJob,
    pub repository: String,
    pub author: Author,
    pub message: String,
    pub entries: Vec<Change>,
}
pub struct PreparedCommit {
    workspace: LockedWorkspace,
    snapshot: Snapshot,
    stage: Directory,
    message: String,
    author: Author,
    pub job: ChangeJob,
}

fn valid_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > 4096
        || path.contains('\0')
        || path.split('/').any(|p| {
            p.is_empty()
                || p == "."
                || p == ".."
                || [".git", ".gitattributes", ".gitmodules"]
                    .iter()
                    .any(|x| p.eq_ignore_ascii_case(x))
        })
    {
        return Err(blocked("Only ordinary repository-relative files are supported. Attributes, submodules, and metadata paths require terminal review."));
    }
    Ok(())
}
fn read_file(path: &Path, limit: u64) -> Result<FileState> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(AppError::io)?;
    let m = file.metadata().map_err(AppError::io)?;
    if !m.is_file()
        || m.nlink() != 1
        || m.uid() != unsafe { libc::geteuid() }
        || m.mode() & 0o022 != 0
    {
        return Err(blocked("Review requires private regular files without symlinks, hardlinks, or shared write access."));
    }
    if m.len() > limit {
        return Err(AppError::new(ErrorCode::LimitReached,"File or index exceeds review limits (8 MiB/file, 32 MiB/index). Review larger changes in your terminal."));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(AppError::io)?;
    if bytes.len() as u64 > limit {
        return Err(stale());
    }
    Ok(FileState {
        bytes,
        device: m.dev(),
        inode: m.ino(),
        mode: m.mode(),
    })
}
fn work_file(root: &Directory, path: &str) -> Result<Option<FileState>> {
    valid_path(path)?;
    let mut parent = root.path.clone();
    let mut parts = path.split('/').peekable();
    while let Some(part) = parts.next() {
        if fs::symlink_metadata(parent.join(".gitattributes")).is_ok() {
            return Err(blocked(
                "Working-tree attributes are unsupported. Review staging in your terminal.",
            ));
        }
        let child = parent.join(part);
        match fs::symlink_metadata(&child) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(AppError::io(e)),
            Ok(_) if parts.peek().is_some() => {
                Directory::open(&child)?.owned()?;
                parent = child;
            }
            Ok(_) => return read_file(&child, FILE_LIMIT).map(Some),
        }
    }
    Err(stale())
}
fn index_policy(git: &Git, w: &LockedWorkspace, cancel: &AtomicBool) -> Result<()> {
    for name in [
        "info/attributes",
        "info/sparse-checkout",
        "modules",
        "refs/replace",
    ] {
        if fs::symlink_metadata(w.meta.path.join(name)).is_ok() {
            return Err(blocked(
                "Attributes, sparse checkout, submodules, or replacement history are unsupported.",
            ));
        }
    }
    let flags = git
        .run(&w.root.path, &["ls-files", "-v", "-z"], cancel, &[])?
        .1;
    if flags
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .any(|p| !p.starts_with(b"H "))
    {
        return Err(blocked(
            "Unmerged, assume-unchanged, and skip-worktree entries are unsupported.",
        ));
    }
    let entries = git
        .run(&w.root.path, &["ls-files", "--stage", "-z"], cancel, &[])?
        .1;
    for entry in entries.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let entry = std::str::from_utf8(entry)
            .map_err(|_| blocked("Non-UTF-8 filenames require terminal review."))?;
        let (mode, path) = entry.split_once('\t').ok_or_else(stale)?;
        if !(mode.starts_with("100644 ") || mode.starts_with("100755 ")) || !mode.ends_with(" 0") {
            return Err(blocked(
                "Only ordinary files in a resolved index are supported.",
            ));
        }
        valid_path(path)?;
    }
    Ok(())
}
fn author(git: &Git, local: &[(String, String)], cancel: &AtomicBool) -> Result<Author> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".config")));
    let paths = [
        xdg.map(|p| p.join("git/config")),
        home.map(|p| p.join(".gitconfig")),
    ];
    author_from_files(git, local, paths.into_iter().flatten(), cancel)
}
fn author_from_files(
    git: &Git,
    local: &[(String, String)],
    paths: impl IntoIterator<Item = PathBuf>,
    cancel: &AtomicBool,
) -> Result<Author> {
    let mut config = Vec::new();
    for path in paths {
        if !path.is_absolute() {
            return Err(blocked(
                "Git author config must use an absolute home/config path.",
            ));
        }
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(AppError::io(e)),
            Ok(_) => {
                read_file(&path, 1024 * 1024)?;
            }
        }
        let bytes = git
            .run(
                Path::new("/"),
                &[
                    "config",
                    "--file",
                    &crate::core::path_string(&path)?,
                    "--no-includes",
                    "--null",
                    "--list",
                ],
                cancel,
                &[],
            )?
            .1;
        config.extend(git::parse_config(&bytes)?);
    }
    config.extend_from_slice(local);
    if config.iter().any(|(k, _)| {
        k.starts_with("include.")
            || k.starts_with("includeif.")
            || k == "core.hookspath"
            || k.starts_with("gpg.")
            || k == "user.signingkey"
    }) || config
        .iter()
        .rev()
        .find(|(k, _)| k == "commit.gpgsign")
        .is_some_and(|(_, v)| {
            !["false", "no", "off", "0"].contains(&v.to_ascii_lowercase().as_str())
        })
    {
        return Err(blocked("Commit requires directly configured author identity without includes, hook overrides, or signing requirements. Use your terminal for those workflows; Stow does not modify settings."));
    }
    let value = |key: &str| {
        config
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.trim().to_owned())
            .filter(|v| !v.is_empty() && !v.chars().any(|c| c.is_control() || c == '<' || c == '>'))
    };
    let name = value("user.name").ok_or_else(|| {
        blocked("Configure a valid Git user.name in your terminal before committing.")
    })?;
    let email = value("user.email")
        .filter(|e| e.contains('@') && !e.contains(char::is_whitespace))
        .ok_or_else(|| {
            blocked("Configure a valid Git user.email in your terminal before committing.")
        })?;
    Ok(Author { name, email })
}
fn status(git: &Git, w: &LockedWorkspace, cancel: &AtomicBool) -> Result<Vec<Change>> {
    let bytes = git
        .run(
            &w.root.path,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignore-submodules=none",
                "--renames",
            ],
            cancel,
            &[],
        )?
        .1;
    let mut records = bytes.split(|b| *b == 0).filter(|p| !p.is_empty());
    let mut entries = Vec::new();
    while let Some(r) = records.next() {
        if r.len() < 4 || r[2] != b' ' {
            return Err(stale());
        }
        let path = std::str::from_utf8(&r[3..])
            .map_err(|_| blocked("Non-UTF-8 filenames require terminal review."))?
            .to_owned();
        valid_path(&path)?;
        let original = if r[..2].contains(&b'R') || r[..2].contains(&b'C') {
            Some(
                std::str::from_utf8(records.next().ok_or_else(stale)?)
                    .map_err(|_| blocked("Non-UTF-8 rename paths are unsupported."))?
                    .to_owned(),
            )
        } else {
            None
        };
        if let Some(ref p) = original {
            valid_path(p)?;
        }
        let name = |code| match code {
            b' ' => Ok(None),
            b'M' => Ok(Some("modified".into())),
            b'A' => Ok(Some("added".into())),
            b'D' => Ok(Some("deleted".into())),
            b'R' => Ok(Some("renamed".into())),
            _ => Err(blocked(
                "Conflicted, copied, or special file changes require terminal review.",
            )),
        };
        let untracked = &r[..2] == b"??";
        entries.push(Change {
            path,
            original,
            staged: if untracked { None } else { name(r[0])? },
            unstaged: if untracked { None } else { name(r[1])? },
            untracked,
            binary: false,
        });
        if entries.len() > 10_000 {
            return Err(blocked(
                "Change list exceeds 10,000 paths. Review in your terminal.",
            ));
        }
    }
    for staged in [false, true] {
        let mut args = vec![
            "diff",
            "--numstat",
            "-z",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
        ];
        if staged {
            args.push("--cached");
        }
        args.push("--");
        let stats = git.run(&w.root.path, &args, cancel, &[])?.1;
        for r in stats
            .split(|b| *b == 0)
            .filter(|r| r.starts_with(b"-\t-\t"))
        {
            let p = std::str::from_utf8(&r[4..]).map_err(|_| stale())?;
            for entry in &mut entries {
                if entry.path == p || entry.original.as_deref() == Some(p) {
                    entry.binary = true;
                }
            }
        }
    }
    Ok(entries)
}
fn snapshot(
    git: &Git,
    w: &LockedWorkspace,
    id: i64,
    lock: Option<&File>,
    cancel: &AtomicBool,
) -> Result<Snapshot> {
    check_cancel(cancel)?;
    sync::checked_layout_index_locked(&w.root, &w.meta, lock)?;
    let config = sync::config(git, &w.root.path, cancel)?;
    sync::ordinary_config(&config)?;
    index_policy(git, w, cancel)?;
    let branch = sync::output(
        git,
        &w.root.path,
        &["symbolic-ref", "--no-recurse", "HEAD"],
        cancel,
    )?
    .strip_prefix("refs/heads/")
    .ok_or_else(|| blocked("Changes require an ordinary checked-out local branch."))?
    .to_owned();
    let head = sync::output(
        git,
        &w.root.path,
        &["rev-parse", "--verify", "HEAD^{commit}"],
        cancel,
    )?;
    if !sync::oid(&head) {
        return Err(blocked(
            "A branch with an existing SHA-1 commit is required.",
        ));
    }
    if git
        .run(
            &w.root.path,
            &["check-ref-format", &format!("refs/heads/{branch}")],
            cancel,
            &[1],
        )?
        .0
        != 0
    {
        return Err(stale());
    }
    let index = read_file(&w.meta.path.join("index"), TOTAL_LIMIT as u64)?;
    let mut entries = status(git, w, cancel)?;
    let mut files = BTreeMap::new();
    let mut total = 0;
    for e in &mut entries {
        for p in std::iter::once(&e.path).chain(e.original.iter()) {
            let file = work_file(&w.root, p)?;
            if let Some(f) = &file {
                total += f.bytes.len();
                if e.untracked && f.bytes.iter().take(8000).any(|b| *b == 0) {
                    e.binary = true;
                }
            }
            if total > TOTAL_LIMIT {
                return Err(blocked("Changed working files exceed the 32 MiB review snapshot limit. Use your terminal for large changes."));
            }
            files.insert(p.clone(), file);
        }
    }
    let author = author(git, &config, cancel);
    if read_file(&w.meta.path.join("index"), TOTAL_LIMIT as u64)? != index
        || sync::config(git, &w.root.path, cancel)? != config
        || sync::output(git, &w.root.path, &["rev-parse", "HEAD"], cancel)? != head
        || sync::output(
            git,
            &w.root.path,
            &["symbolic-ref", "--no-recurse", "HEAD"],
            cancel,
        )? != format!("refs/heads/{branch}")
        || git
            .run(
                &w.root.path,
                &[
                    "symbolic-ref",
                    "--quiet",
                    "--no-recurse",
                    &format!("refs/heads/{branch}"),
                ],
                cancel,
                &[1],
            )?
            .0
            != 1
    {
        return Err(stale());
    }
    sync::checked_layout_index_locked(&w.root, &w.meta, lock)?;
    Ok(Snapshot {
        view: Review {
            token: safe_fs::random_id()?,
            project_id: id,
            repository: crate::core::path_string(&w.root.path)?,
            branch,
            head,
            entries,
            author: author.as_ref().ok().cloned(),
            author_error: author.err(),
        },
        config,
        index,
        files,
    })
}
fn same(a: &Snapshot, b: &Snapshot, working_files: bool) -> bool {
    a.config == b.config
        && a.index == b.index
        && a.view.branch == b.view.branch
        && a.view.head == b.view.head
        && (!working_files || a.files == b.files)
}
pub fn review(db: &Registry, git: &Git, id: i64, cancel: &AtomicBool) -> Result<Snapshot> {
    sync::inspect(db, git, id, cancel)?;
    let w = LockedWorkspace::open(db, id)?;
    snapshot(git, &w, id, None, cancel)
}
pub fn diff(
    db: &Registry,
    git: &Git,
    saved: &Snapshot,
    path: &str,
    staged: bool,
    cancel: &AtomicBool,
) -> Result<Diff> {
    let w = LockedWorkspace::open(db, saved.view.project_id)?;
    if !same(
        saved,
        &snapshot(git, &w, saved.view.project_id, None, cancel)?,
        true,
    ) {
        return Err(stale());
    }
    let e = saved
        .view
        .entries
        .iter()
        .find(|e| {
            e.path == path
                && if staged {
                    e.staged.is_some()
                } else {
                    e.unstaged.is_some() || e.untracked
                }
        })
        .ok_or_else(stale)?;
    let bytes = if e.untracked {
        saved
            .files
            .get(path)
            .and_then(|f| f.as_ref())
            .ok_or_else(stale)?
            .bytes
            .clone()
    } else {
        let mut args = vec![
            "--literal-pathspecs",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--find-renames=50%",
        ];
        if staged {
            args.push("--cached");
        }
        args.push("--");
        args.push(path);
        if let Some(p) = &e.original {
            args.push(p);
        }
        git.run(&w.root.path, &args, cancel, &[])?.1
    };
    if e.binary {
        return Ok(Diff {
            binary: true,
            text: "Binary content is not rendered. Review this file with a trusted local tool."
                .into(),
        });
    }
    if bytes.len() > 512 * 1024 {
        return Err(AppError::new(
            ErrorCode::LimitReached,
            "Diff exceeds the 512 KiB preview limit. Review this file in your terminal.",
        ));
    }
    Ok(Diff {
        binary: false,
        text: String::from_utf8_lossy(&bytes).into(),
    })
}
fn job(saved: &Snapshot, kind: &str, paths: Vec<String>) -> Result<ChangeJob> {
    Ok(ChangeJob {
        id: safe_fs::random_id()?,
        project_id: saved.view.project_id,
        kind: kind.into(),
        state: "running".into(),
        phase: "preflight".into(),
        started_at: now(),
        completed_at: None,
        branch: saved.view.branch.clone(),
        before_head: saved.view.head.clone(),
        after_head: None,
        tree: None,
        paths,
        error: None,
    })
}
fn finish(db: &Registry, j: &mut ChangeJob, outcome: Result<()>) -> Result<()> {
    j.completed_at = Some(now());
    match outcome {
        Ok(()) => {
            j.state = "completed".into();
            j.phase = "verified".into();
            if db.save_change(j).is_err() {
                let e = uncertain();
                j.state = "requires_inspection".into();
                j.error = Some(e.clone());
                let _ = db.save_change(j);
                return Err(e);
            }
            Ok(())
        }
        Err(e) => {
            let e = if matches!(
                j.phase.as_str(),
                "installing_index" | "updating_branch" | "verifying"
            ) {
                uncertain()
            } else {
                e
            };
            j.state = if e.code == ErrorCode::Cancelled {
                "cancelled"
            } else if e.code == ErrorCode::RecoveryRequired {
                "requires_inspection"
            } else {
                "blocked"
            }
            .into();
            j.error = Some(e.clone());
            db.save_change(j)?;
            Err(e)
        }
    }
}
// This file is owned by us only while its descriptor identity still matches.
// A crash intentionally leaves the native index lock for manual inspection.
struct IndexLock {
    file: File,
    path: PathBuf,
}
impl IndexLock {
    fn acquire(w: &LockedWorkspace) -> Result<Self> {
        w.validate()?;
        Self::create(w, "index.lock")
    }
    // Caller has already checked the workspace under its own index reservation.
    fn create(w: &LockedWorkspace, name: &str) -> Result<Self> {
        w.root.validate_path()?;
        w.meta.validate_path()?;
        let path = w.meta.path.join(name);
        let file=OpenOptions::new().read(true).write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(&path).map_err(|e|if e.kind()==std::io::ErrorKind::AlreadyExists{AppError::new(ErrorCode::Busy,"Git index is locked. Wait for the other operation; Stow will not remove its lock.")}else{AppError::io(e)})?;
        Ok(Self { file, path })
    }
    fn valid(&self) -> Result<()> {
        let a = self.file.metadata().map_err(AppError::io)?;
        let b = fs::symlink_metadata(&self.path).map_err(AppError::io)?;
        if a.dev() != b.dev() || a.ino() != b.ino() || !b.is_file() {
            return Err(stale());
        }
        Ok(())
    }
    fn install(&mut self, w: &LockedWorkspace, bytes: &[u8]) -> Result<()> {
        self.valid()?;
        w.root.validate_path()?;
        w.meta.validate_path()?;
        self.file.write_all(bytes).map_err(AppError::io)?;
        self.file.sync_all().map_err(AppError::io)?;
        fs::rename(&self.path, w.meta.path.join("index")).map_err(AppError::io)?;
        w.meta.file.sync_all().map_err(AppError::io)
    }
}
impl Drop for IndexLock {
    fn drop(&mut self) {
        if self.valid().is_ok() {
            let _ = fs::remove_file(&self.path);
        }
    }
}
fn private_index(w: &LockedWorkspace, saved: &Snapshot) -> Result<Directory> {
    let stage = w
        .meta
        .create(&format!(".stow-changes-{}", safe_fs::random_id()?))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(stage.path.join("index"))
        .map_err(AppError::io)?;
    file.write_all(&saved.index.bytes).map_err(AppError::io)?;
    file.sync_all().map_err(AppError::io)?;
    let mut head = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(stage.path.join("HEAD"))
        .map_err(AppError::io)?;
    head.write_all(format!("{}\n", saved.view.head).as_bytes())
        .map_err(AppError::io)?;
    let mut common = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(stage.path.join("commondir"))
        .map_err(AppError::io)?;
    common
        .write_all(format!("{}\n", crate::core::path_string(&w.meta.path)?).as_bytes())
        .map_err(AppError::io)?;
    Ok(stage)
}
pub fn mutate_index(
    db: &Registry,
    git: &Git,
    saved: &Snapshot,
    selected: &[String],
    unstage: bool,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&str, Option<u8>),
) -> Result<()> {
    if selected.is_empty() || selected.len() > 1000 {
        return Err(blocked("Select between 1 and 1,000 files."));
    }
    let w = LockedWorkspace::open(db, saved.view.project_id)?;
    if !same(
        saved,
        &snapshot(git, &w, saved.view.project_id, None, cancel)?,
        true,
    ) {
        return Err(stale());
    }
    let mut paths = BTreeSet::new();
    for p in selected {
        valid_path(p)?;
        let e = saved
            .view
            .entries
            .iter()
            .find(|e| {
                e.path == *p
                    && if unstage {
                        e.staged.is_some()
                    } else {
                        e.unstaged.is_some() || e.untracked
                    }
            })
            .ok_or_else(stale)?;
        paths.insert(p.clone());
        if let Some(old) = &e.original {
            paths.insert(old.clone());
        }
    }
    let mut j = job(
        saved,
        if unstage { "unstage" } else { "stage" },
        paths.into_iter().collect(),
    )?;
    db.save_change(&j)?;
    let result = (|| {
        progress("Checking selected files and reserving index", None);
        check_cancel(cancel)?;
        let mut lock = IndexLock::acquire(&w)?;
        if !same(
            saved,
            &snapshot(git, &w, saved.view.project_id, Some(&lock.file), cancel)?,
            true,
        ) {
            return Err(stale());
        }
        let stage = private_index(&w, saved)?;
        let runner = LocalRunner {
            git,
            w: &w,
            stage: &stage,
        };
        let input = j
            .paths
            .iter()
            .flat_map(|p| p.as_bytes().iter().copied().chain(std::iter::once(0)))
            .collect();
        j.phase = "editing_private_index".into();
        db.save_change(&j)?;
        runner.index_edit(unstage, &saved.view.head, input)?;
        progress(
            "Revalidating selected files before index installation",
            None,
        );
        check_cancel(cancel)?;
        if !same(
            saved,
            &snapshot(git, &w, saved.view.project_id, Some(&lock.file), cancel)?,
            true,
        ) {
            return Err(stale());
        }
        let bytes = read_file(&stage.path.join("index"), TOTAL_LIMIT as u64)?.bytes;
        j.phase = "installing_index".into();
        db.save_change(&j)?;
        lock.install(&w, &bytes)?;
        drop(lock);
        j.phase = "verifying".into();
        db.save_change(&j)?;
        if read_file(&w.meta.path.join("index"), TOTAL_LIMIT as u64)?.bytes != bytes {
            return Err(uncertain());
        }
        for (path, state) in &saved.files {
            if work_file(&w.root, path)? != *state {
                return Err(uncertain());
            }
        }
        sync::inspect(db, git, saved.view.project_id, &AtomicBool::new(false))?;
        let fresh = db
            .sync_record(saved.view.project_id)?
            .assessment
            .ok_or_else(uncertain)?;
        if fresh.inspection.commit.as_ref() != Some(&saved.view.head)
            || fresh.inspection.branch.as_ref() != Some(&saved.view.branch)
        {
            return Err(uncertain());
        }
        j.after_head = Some(saved.view.head.clone());
        Ok(())
    })();
    finish(db, &mut j, result)?;
    progress("Index update verified; working files preserved", Some(100));
    Ok(())
}
pub fn prepare_commit(
    db: &Registry,
    git: &Git,
    saved: &Snapshot,
    message: String,
    cancel: &AtomicBool,
) -> Result<PreparedCommit> {
    if message.trim().is_empty() || message.len() > 65536 || message.contains('\0') {
        return Err(blocked(
            "Enter a nonempty commit message of at most 64 KiB without NUL characters.",
        ));
    }
    let workspace = LockedWorkspace::open(db, saved.view.project_id)?;
    let current = snapshot(git, &workspace, saved.view.project_id, None, cancel)?;
    if !same(saved, &current, false) {
        return Err(stale());
    }
    reject_hooks(&workspace.meta.path)?;
    let author = current
        .view
        .author
        .clone()
        .ok_or_else(|| current.view.author_error.clone().unwrap_or_else(stale))?;
    if current.view.entries.iter().all(|e| e.staged.is_none()) {
        return Err(blocked(
            "There are no staged changes. Empty commits are unsupported.",
        ));
    }
    let stage = private_index(&workspace, &current)?;
    let runner = LocalRunner {
        git,
        w: &workspace,
        stage: &stage,
    };
    let tree = runner.write_tree()?;
    if tree
        == sync::output(
            git,
            &workspace.root.path,
            &["rev-parse", "HEAD^{tree}"],
            cancel,
        )?
    {
        return Err(blocked(
            "The staged tree equals HEAD. Empty commits are unsupported.",
        ));
    }
    let mut job = job(
        &current,
        "commit",
        current
            .view
            .entries
            .iter()
            .filter(|e| e.staged.is_some())
            .map(|e| e.path.clone())
            .collect(),
    )?;
    job.tree = Some(tree);
    job.phase = "awaiting_confirmation".into();
    db.save_change(&job)?;
    Ok(PreparedCommit {
        workspace,
        snapshot: current,
        stage,
        message,
        author,
        job,
    })
}
impl PreparedCommit {
    pub fn preview(&self) -> CommitPreview {
        CommitPreview {
            job: self.job.clone(),
            repository: self.snapshot.view.repository.clone(),
            author: self.author.clone(),
            message: self.message.clone(),
            entries: self
                .snapshot
                .view
                .entries
                .iter()
                .filter(|e| e.staged.is_some())
                .cloned()
                .collect(),
        }
    }
    pub fn stop(&mut self, db: &Registry, e: AppError) -> Result<()> {
        finish(db, &mut self.job, Err(e))
    }
    pub fn execute(
        &mut self,
        db: &Registry,
        git: &Git,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(&str, Option<u8>),
    ) -> Result<()> {
        let outcome = (|| {
            check_cancel(cancel)?;
            let w = &self.workspace;
            let current = snapshot(git, w, self.job.project_id, None, cancel)?;
            if !same(&self.snapshot, &current, false)
                || current.view.author.as_ref() != Some(&self.author)
            {
                return Err(stale());
            }
            reject_hooks(&w.meta.path)?;
            let lock = IndexLock::acquire(w)?;
            let current = snapshot(git, w, self.job.project_id, Some(&lock.file), cancel)?;
            if !same(&self.snapshot, &current, false)
                || current.view.author.as_ref() != Some(&self.author)
            {
                return Err(stale());
            }
            let runner = LocalRunner {
                git,
                w,
                stage: &self.stage,
            };
            let tree = self.job.tree.as_ref().ok_or_else(stale)?;
            if runner.write_tree()? != *tree {
                return Err(stale());
            }
            self.job.phase = "creating_object".into();
            db.save_change(&self.job)?;
            let target =
                runner.commit_tree(tree, &self.job.before_head, &self.author, &self.message)?;
            progress(
                "Commit object ready; revalidating confirmation before branch update",
                None,
            );
            check_cancel(cancel)?;
            let current = snapshot(git, w, self.job.project_id, Some(&lock.file), cancel)?;
            if !same(&self.snapshot, &current, false)
                || current.view.author.as_ref() != Some(&self.author)
            {
                return Err(stale());
            }
            reject_hooks(&w.meta.path)?;
            lock.valid()?;
            // Git cannot symref-verify HEAD and update its referent in one
            // transaction (implicit HEAD reflog update duplicates that ref).
            // Reserve the real HEAD natively; update the named branch through
            // our detached metadata view sharing the same common ref store.
            let head_lock = IndexLock::create(w, "HEAD.lock")?;
            self.job.after_head = Some(target.clone());
            self.job.phase = "updating_branch".into();
            db.save_change(&self.job)?;
            progress(
                "Updating branch atomically; cancellation deferred until verification",
                None,
            );
            head_lock.valid()?;
            lock.valid()?;
            if read_file(&w.meta.path.join("HEAD"), 4096)?.bytes
                != format!("ref: refs/heads/{}\n", self.job.branch).as_bytes()
                || read_file(&w.meta.path.join("index"), TOTAL_LIMIT as u64)? != self.snapshot.index
                || sync::config(git, &w.root.path, &AtomicBool::new(false))? != self.snapshot.config
            {
                return Err(stale());
            }
            runner.update_branch(
                &self.job.branch,
                &self.job.before_head,
                &target,
                &self.author,
            )?;
            self.job.phase = "verifying".into();
            db.save_change(&self.job)?;
            drop(head_lock);
            drop(lock);
            let fresh = AtomicBool::new(false);
            if sync::output(git, &w.root.path, &["rev-parse", "HEAD"], &fresh)? != target
                || read_file(&w.meta.path.join("index"), TOTAL_LIMIT as u64)? != self.snapshot.index
                || sync::output(
                    git,
                    &w.root.path,
                    &["rev-parse", &format!("{target}^{{tree}}")],
                    &fresh,
                )? != *tree
                || sync::output(
                    git,
                    &w.root.path,
                    &["rev-parse", &format!("{target}^1")],
                    &fresh,
                )? != self.job.before_head
            {
                return Err(uncertain());
            }
            sync::inspect(db, git, self.job.project_id, &fresh)?;
            Ok(())
        })();
        finish(db, &mut self.job, outcome)?;
        progress(
            "Commit verified. Hooks and project tests were not run; nothing was pushed.",
            Some(100),
        );
        Ok(())
    }
}
// No arbitrary argument API is exposed beyond this private policy. All writes
// except compare-and-swap refs target the private index or append-only objects.
struct LocalRunner<'a> {
    git: &'a Git,
    w: &'a LockedWorkspace,
    stage: &'a Directory,
}
impl LocalRunner<'_> {
    fn command(&self) -> Result<Command> {
        self.w.root.validate_path()?;
        self.w.meta.validate_path()?;
        self.stage.validate_path()?;
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
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_INDEX_FILE", self.stage.path.join("index"));
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
            "commit.gpgSign=false",
            "gc.auto=0",
            "maintenance.auto=false",
            "core.pager=cat",
        ] {
            c.args(["-c", setting]);
        }
        c.arg("--literal-pathspecs");
        let fd = self.w.root.file.as_raw_fd();
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
    fn run(&self, mut c: Command, input: Option<Vec<u8>>) -> Result<Vec<u8>> {
        // Native atomic writes are never killed for UI cancellation or timeout.
        let mut child = c
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| {
                AppError::new(
                    ErrorCode::GitFailed,
                    "Local Git process could not start. Check the Git installation.",
                )
            })?;
        let writer = input.map(|b| {
            let mut pipe = child.stdin.take().expect("piped input");
            thread::spawn(move || pipe.write_all(&b))
        });
        let output = child.stdout.take().expect("piped stdout");
        let overflow = Arc::new(AtomicBool::new(false));
        let flag = overflow.clone();
        let reader = thread::spawn(move || crate::git_process::drain(output, flag, None));
        let status = child.wait().map_err(|_| uncertain());
        let bytes = reader.join().map_err(|_| uncertain())??;
        let written = writer.map(|w| w.join());
        if !status?.success() || overflow.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(AppError::new(ErrorCode::GitFailed,"Constrained local Git step failed. Inspect permissions, index locks, and repository state. Raw output is withheld."));
        }
        if let Some(w) = written {
            w.map_err(|_| uncertain())?.map_err(|_| uncertain())?;
        }
        Ok(bytes)
    }
    fn index_edit(&self, unstage: bool, head: &str, input: Vec<u8>) -> Result<()> {
        let mut c = self.command()?;
        if unstage {
            c.args([
                "restore",
                "--staged",
                &format!("--source={head}"),
                "--pathspec-from-file=-",
                "--pathspec-file-nul",
            ]);
        } else {
            c.args(["add", "--pathspec-from-file=-", "--pathspec-file-nul"]);
        }
        self.run(c, Some(input)).map(|_| ())
    }
    fn write_tree(&self) -> Result<String> {
        let mut c = self.command()?;
        c.arg("write-tree");
        parse_oid(self.run(c, None)?)
    }
    fn commit_tree(
        &self,
        tree: &str,
        parent: &str,
        author: &Author,
        message: &str,
    ) -> Result<String> {
        let mut c = self.command()?;
        c.env("GIT_AUTHOR_NAME", &author.name)
            .env("GIT_AUTHOR_EMAIL", &author.email)
            .env("GIT_COMMITTER_NAME", &author.name)
            .env("GIT_COMMITTER_EMAIL", &author.email)
            .args([
                "commit-tree",
                tree,
                "-p",
                parent,
                "--no-gpg-sign",
                "-F",
                "-",
            ]);
        parse_oid(self.run(c, Some(message.as_bytes().to_vec()))?)
    }
    fn update_branch(
        &self,
        branch: &str,
        before: &str,
        after: &str,
        author: &Author,
    ) -> Result<()> {
        let mut c = self.command()?;
        c.env("GIT_DIR", &self.stage.path)
            .env("GIT_COMMON_DIR", &self.w.meta.path)
            .env("GIT_WORK_TREE", &self.w.root.path)
            .env("GIT_COMMITTER_NAME", &author.name)
            .env("GIT_COMMITTER_EMAIL", &author.email)
            .args([
                "update-ref",
                "--no-deref",
                "--create-reflog",
                "-m",
                "commit (Stow)",
                "--stdin",
            ]);
        self.run(
            c,
            Some(
                format!("start\nupdate refs/heads/{branch} {after} {before}\nprepare\ncommit\n")
                    .into_bytes(),
            ),
        )
        .map(|_| ())
    }
}
fn parse_oid(bytes: Vec<u8>) -> Result<String> {
    let value = std::str::from_utf8(&bytes).map_err(|_| uncertain())?.trim();
    if !sync::oid(value) {
        return Err(uncertain());
    }
    Ok(value.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn direct_global_author_precedence_includes_and_signing_are_checked_without_execution() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().canonicalize().unwrap().join("config");
        let git = Git::default();
        let cancel = AtomicBool::new(false);
        fs::write(&file,"[user]\n name = Global Fixture\n email = global@example.invalid\n[credential]\n helper = !touch should-not-run\n").unwrap();
        let global = author_from_files(&git, &[], vec![file.clone()], &cancel).unwrap();
        assert_eq!(global.email, "global@example.invalid");
        let local = vec![
            ("user.name".into(), "Local Fixture".into()),
            ("user.email".into(), "local@example.invalid".into()),
        ];
        assert_eq!(
            author_from_files(&git, &local, vec![file.clone()], &cancel)
                .unwrap()
                .email,
            "local@example.invalid"
        );
        for extra in [
            "[include]\n path = other\n",
            "[commit]\n gpgSign = true\n",
            "[core]\n hooksPath = arbitrary\n",
        ] {
            fs::write(
                &file,
                format!("[user]\n name = Fixture\n email = fixture@example.invalid\n{extra}"),
            )
            .unwrap();
            assert!(author_from_files(&git, &local, vec![file.clone()], &cancel).is_err());
        }
        assert!(!temp.path().join("should-not-run").exists());
        assert!(author_from_files(&git, &[], Vec::new(), &cancel).is_err());
    }
}
