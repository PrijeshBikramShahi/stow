use crate::core::{path_string, AppError, ErrorCode, Inspection, Remote, Result, WorkingTree};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

const OUTPUT_LIMIT: usize = 4 * 1024 * 1024;

#[derive(Clone)]
pub struct Git {
    executable: PathBuf,
    timeout: Duration,
}

impl Default for Git {
    fn default() -> Self {
        // Do not resolve executables relative to a repository or inherited PATH.
        let executable = [
            "/opt/homebrew/bin/git",
            "/usr/local/bin/git",
            "/usr/bin/git",
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("/usr/bin/git"));
        Self::new(executable)
    }
}

impl Git {
    pub fn new(executable: PathBuf) -> Self {
        Self {
            executable,
            timeout: Duration::from_secs(15),
        }
    }

    pub(crate) fn executable(&self) -> &Path {
        &self.executable
    }

    pub fn version(&self) -> Result<String> {
        let (_, bytes) = self.run(Path::new("/"), &["--version"], &AtomicBool::new(false), &[])?;
        Ok(String::from_utf8_lossy(&bytes).trim().to_owned())
    }

    // Private: IPC cannot pass executable names, commands, environment, or Git arguments.
    pub(crate) fn run(
        &self,
        directory: &Path,
        args: &[&str],
        cancel: &AtomicBool,
        allowed_codes: &[i32],
    ) -> Result<(i32, Vec<u8>)> {
        check_cancel(cancel)?;
        let mut command = Command::new(&self.executable);
        command
            .current_dir(directory)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_NO_LAZY_FETCH", "1")
            .args(["--no-pager", "--no-optional-locks"]);
        for setting in [
            "core.fsmonitor=false",
            "core.hooksPath=/dev/null",
            "core.attributesFile=/dev/null",
            "core.excludesFile=/dev/null",
            "core.untrackedCache=false",
            "core.preloadIndex=false",
            "diff.external=",
            "diff.renames=false",
            "submodule.recurse=false",
            "protocol.allow=never",
            "credential.helper=",
            "core.pager=cat",
        ] {
            command.args(["-c", setting]);
        }
        let mut child = command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound && !self.executable.is_file() {
                    AppError::new(
                        ErrorCode::GitUnavailable,
                        "Git is unavailable. Install system Git and restart Stow.",
                    )
                } else {
                    AppError::io(e)
                }
            })?;
        let overflow = Arc::new(AtomicBool::new(false));
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AppError::new(ErrorCode::Internal, "Git output unavailable."))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| AppError::new(ErrorCode::Internal, "Git error output unavailable."))?;
        let flag = overflow.clone();
        let reader = thread::spawn(move || read_bounded(stdout, flag));
        let flag = overflow.clone();
        // Drain errors to avoid deadlocks; never expose raw stderr (it can contain secrets).
        let error_reader = thread::spawn(move || read_bounded(stderr, flag));
        let start = Instant::now();
        let status = loop {
            let reason = if cancel.load(Ordering::Relaxed) {
                Some(AppError::new(ErrorCode::Cancelled, "Operation cancelled."))
            } else if overflow.load(Ordering::Relaxed) {
                Some(AppError::new(
                    ErrorCode::LimitReached,
                    "Git output exceeded the 4 MiB inspection limit.",
                ))
            } else if start.elapsed() > self.timeout {
                Some(AppError::new(
                    ErrorCode::Timeout,
                    "Git inspection exceeded its 15-second command limit.",
                ))
            } else {
                None
            };
            if let Some(error) = reason {
                let _ = child.kill();
                let _ = child.wait();
                break Err(error);
            }
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) => thread::sleep(Duration::from_millis(10)),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(AppError::io(error));
                }
            }
        };
        let output = reader
            .join()
            .map_err(|_| AppError::new(ErrorCode::Internal, "Git output reader failed."))??;
        error_reader
            .join()
            .map_err(|_| AppError::new(ErrorCode::Internal, "Git error reader failed."))??;
        let status = status?;
        check_cancel(cancel)?;
        if overflow.load(Ordering::Relaxed) {
            return Err(AppError::new(
                ErrorCode::LimitReached,
                "Git output exceeded the 4 MiB inspection limit.",
            ));
        }
        let code = status.code().unwrap_or(-1);
        if !status.success() && !allowed_codes.contains(&code) {
            return Err(AppError::new(ErrorCode::GitFailed, format!("Read-only Git inspection failed (exit {code}). Check ownership, permissions, and repository integrity in your terminal. Raw Git errors are withheld to protect credentials.")));
        }
        Ok((code, output))
    }

    pub fn inspect(&self, path: &Path, cancel: &AtomicBool) -> Result<(PathBuf, Inspection)> {
        path_string(path)?;
        if !path.is_absolute() {
            return Err(AppError::new(
                ErrorCode::InvalidPath,
                "Choose an absolute directory path.",
            ));
        }
        if std::fs::symlink_metadata(path)
            .map_err(AppError::io)?
            .file_type()
            .is_symlink()
        {
            return Err(AppError::new(
                ErrorCode::Unsupported,
                "Repository paths must not be symbolic links. Choose the actual directory.",
            ));
        }
        let path = path.canonicalize().map_err(AppError::io)?;
        if !path.is_dir() {
            return Err(AppError::new(
                ErrorCode::InvalidPath,
                "The selected path is not a directory.",
            ));
        }
        let marker = path.join(".git");
        let metadata = std::fs::symlink_metadata(&marker).map_err(AppError::io)?;
        if metadata.file_type().is_symlink() || !(metadata.is_dir() || metadata.is_file()) {
            return Err(AppError::new(
                ErrorCode::Unsupported,
                "Symlinked or special .git markers are unsupported.",
            ));
        }
        // Parse the explicit file OUTSIDE the repository: repository setup itself can
        // process includes before `git config --local --no-includes` sees its flags.
        let config_path = repository_config(&path, &marker, metadata.is_file())?;
        let config_path = path_string(&config_path)?;
        let (_, config) = self.run(
            Path::new("/"),
            &[
                "config",
                "--file",
                &config_path,
                "--no-includes",
                "--null",
                "--list",
            ],
            cancel,
            &[],
        )?;
        let config = parse_config(&config)?;
        validate_config(&config)?;
        let (_, root) = self.run(&path, &["rev-parse", "--show-toplevel"], cancel, &[])?;
        let root = String::from_utf8(root).map_err(|_| {
            AppError::new(
                ErrorCode::Unsupported,
                "Non-UTF-8 repository roots are unsupported.",
            )
        })?;
        // Remove exactly Git's terminating newline; preserve whitespace within paths.
        let root = PathBuf::from(root.strip_suffix('\n').unwrap_or(&root))
            .canonicalize()
            .map_err(AppError::io)?;
        if root != path {
            return Err(AppError::new(
                ErrorCode::Unsupported,
                "Git redirects this directory to a different worktree root.",
            ));
        }
        let (_, superproject) = self.run(
            &path,
            &["rev-parse", "--show-superproject-working-tree"],
            cancel,
            &[],
        )?;
        if superproject != b"\n" && !superproject.is_empty() {
            return Err(AppError::new(
                ErrorCode::Unsupported,
                "Submodules are not registered as independent projects.",
            ));
        }
        let (branch_code, branch) = self.run(
            &path,
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
            cancel,
            &[1],
        )?;
        let branch = if branch_code == 0 {
            Some(text_line(branch)?)
        } else {
            None
        };
        let (commit_code, commit) = self.run(
            &path,
            &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
            cancel,
            &[1],
        )?;
        let commit = if commit_code == 0 {
            Some(text_line(commit)?)
        } else {
            None
        };
        if branch.is_none() && commit.is_none() {
            return Err(AppError::new(
                ErrorCode::GitFailed,
                "Repository HEAD is invalid.",
            ));
        }
        let upstream = if let Some(branch) = &branch {
            let reference = format!("refs/heads/{branch}");
            let (_, result) = self.run(
                &path,
                &[
                    "for-each-ref",
                    "--format=%(upstream:short)",
                    "--",
                    &reference,
                ],
                cancel,
                &[],
            )?;
            let value = text_line(result)?;
            if value.is_empty() {
                None
            } else {
                Some(value)
            }
        } else {
            None
        };
        let remotes = config
            .iter()
            .filter_map(|(key, value)| {
                key.strip_prefix("remote.")
                    .and_then(|k| k.strip_suffix(".url"))
                    .map(|name| Remote {
                        name: name.to_owned(),
                        url: redact_remote(value),
                    })
            })
            .collect();
        let (_, status) = self.run(
            &path,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--ignore-submodules=all",
                "--no-renames",
            ],
            cancel,
            &[],
        )?;
        let working_tree = parse_status(&status)?;
        let mut warnings = vec!["Submodule working trees are excluded from status. A clean result is not a removal-safety assessment.".to_owned()];
        if commit.is_none() {
            warnings.push("This repository has no commits yet.".to_owned());
        }
        Ok((
            root,
            Inspection {
                detached: branch.is_none(),
                branch,
                commit,
                upstream,
                remotes,
                working_tree,
                warnings,
            },
        ))
    }
}

pub(crate) fn repository_config(root: &Path, marker: &Path, is_file: bool) -> Result<PathBuf> {
    let git_directory = if is_file {
        let pointer = read_metadata_file(marker, 4096)?;
        let pointer = pointer
            .strip_prefix("gitdir: ")
            .ok_or_else(|| AppError::new(ErrorCode::Unsupported, "Invalid .git pointer file."))?;
        root.join(pointer.trim_end())
            .canonicalize()
            .map_err(AppError::io)?
    } else {
        marker.to_owned()
    };
    let common_file = git_directory.join("commondir");
    let common = match std::fs::symlink_metadata(&common_file) {
        Ok(_) => {
            let pointer = read_metadata_file(&common_file, 4096)?;
            git_directory
                .join(pointer.trim_end())
                .canonicalize()
                .map_err(AppError::io)?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => git_directory,
        Err(error) => return Err(AppError::io(error)),
    };
    let config = common.join("config");
    // Check the file type and size before asking Git to parse it. Reject devices,
    // FIFOs, and symlinks; config contents are never returned to the frontend.
    let meta = std::fs::symlink_metadata(&config).map_err(AppError::io)?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 1024 * 1024 {
        return Err(AppError::new(
            ErrorCode::Unsupported,
            "Git config must be a regular file no larger than 1 MiB.",
        ));
    }
    Ok(config)
}

fn read_metadata_file(path: &Path, limit: u64) -> Result<String> {
    let meta = std::fs::symlink_metadata(path).map_err(AppError::io)?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > limit {
        return Err(AppError::new(
            ErrorCode::Unsupported,
            "Unsupported Git metadata pointer.",
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(AppError::io)?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(AppError::io)?;
    if bytes.len() as u64 > limit {
        return Err(AppError::new(
            ErrorCode::LimitReached,
            "Git metadata pointer exceeded its size limit.",
        ));
    }
    String::from_utf8(bytes)
        .map_err(|_| AppError::new(ErrorCode::Unsupported, "Non-UTF-8 Git metadata pointer."))
}

fn read_bounded(mut stream: impl Read, overflow: Arc<AtomicBool>) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let length = stream.read(&mut buffer).map_err(AppError::io)?;
        if length == 0 {
            break;
        }
        let remaining = OUTPUT_LIMIT.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..length.min(remaining)]);
        if length > remaining {
            overflow.store(true, Ordering::Relaxed);
        }
    }
    Ok(output)
}

pub fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(AppError::new(ErrorCode::Cancelled, "Operation cancelled."))
    } else {
        Ok(())
    }
}

pub(crate) fn text_line(bytes: Vec<u8>) -> Result<String> {
    let text = String::from_utf8(bytes).map_err(|_| {
        AppError::new(
            ErrorCode::Unsupported,
            "Non-UTF-8 Git metadata is unsupported.",
        )
    })?;
    Ok(text.strip_suffix('\n').unwrap_or(&text).to_owned())
}

pub(crate) fn parse_config(bytes: &[u8]) -> Result<Vec<(String, String)>> {
    let text = std::str::from_utf8(bytes).map_err(|_| {
        AppError::new(
            ErrorCode::Unsupported,
            "Non-UTF-8 Git configuration is unsupported.",
        )
    })?;
    Ok(text
        .split('\0')
        .filter(|x| !x.is_empty())
        .map(|record| {
            let (key, value) = record.split_once('\n').unwrap_or((record, ""));
            (key.to_owned(), value.to_owned())
        })
        .collect())
}

pub(crate) fn validate_config(config: &[(String, String)]) -> Result<()> {
    for (key, value) in config {
        let key = key.to_ascii_lowercase();
        if key == "include.path"
            || key.starts_with("includeif.")
            || key.starts_with("filter.")
            || key == "core.worktree"
            || key == "core.bare" && value != "false"
            || key.starts_with("extensions.")
                && !(key == "extensions.objectformat" && value == "sha1")
            || key.ends_with(".promisor")
            || key.ends_with(".partialclonefilter")
        {
            return Err(AppError::new(ErrorCode::Unsupported, "Inspection disabled: this repository uses config includes, filters, redirected worktrees, partial-clone storage, or unsupported extensions. Stow has not modified its configuration."));
        }
    }
    Ok(())
}

// Keep host/path useful while never persisting URL userinfo, query, or fragment.
// Unknown remote syntaxes are intentionally withheld.
pub fn redact_remote(raw: &str) -> String {
    if raw.chars().any(char::is_control) {
        return "[remote configured; URL withheld]".to_owned();
    }
    let clean = raw.split(['?', '#']).next().unwrap_or("");
    if let Some((scheme, rest)) = clean.split_once("://") {
        if !["https", "http", "ssh", "git", "file"].contains(&scheme) {
            return "[remote configured; URL withheld]".to_owned();
        }
        let (authority, tail) = rest
            .split_once('/')
            .map(|(a, b)| (a, format!("/{b}")))
            .unwrap_or((rest, String::new()));
        return format!(
            "{scheme}://{}{tail}",
            authority.rsplit('@').next().unwrap_or("")
        );
    }
    if clean.contains(':') && !clean.starts_with('/') {
        let host_path = clean.rsplit('@').next().unwrap_or(clean);
        if !host_path.contains("::") {
            return host_path.to_owned();
        }
    }
    "[local or nonstandard remote configured; URL withheld]".to_owned()
}

fn parse_status(bytes: &[u8]) -> Result<WorkingTree> {
    let mut state = WorkingTree::default();
    let mut records = bytes.split(|b| *b == 0).filter(|x| !x.is_empty());
    while let Some(record) = records.next() {
        if record.len() < 4 || record[2] != b' ' {
            return Err(AppError::new(
                ErrorCode::GitFailed,
                "Git returned an invalid status record.",
            ));
        }
        let (x, y) = (record[0], record[1]);
        if x == b'?' && y == b'?' {
            state.untracked += 1;
            continue;
        }
        if x == b'U' || y == b'U' || (x == b'A' && y == b'A') || (x == b'D' && y == b'D') {
            state.conflicted += 1;
        }
        if x != b' ' {
            state.staged += 1;
        }
        if y != b' ' {
            state.unstaged += 1;
        }
        if x == b'R' || x == b'C' || y == b'R' || y == b'C' {
            records.next();
        }
    }
    Ok(state)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fake_git(directory: &Path, body: &str) -> Git {
        let executable = directory.join("fake-git");
        std::fs::write(&executable, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        Git::new(executable)
    }

    #[test]
    fn runner_times_out_and_reaps_process() {
        let temp = tempfile::tempdir().unwrap();
        let mut git = fake_git(temp.path(), "exec /bin/sleep 10");
        git.timeout = Duration::from_millis(50);
        let start = Instant::now();
        assert_eq!(git.version().unwrap_err().code, ErrorCode::Timeout);
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn runner_cancels_in_flight_process() {
        let temp = tempfile::tempdir().unwrap();
        let git = fake_git(temp.path(), "exec /bin/sleep 10");
        let flag = Arc::new(AtomicBool::new(false));
        let cancel = flag.clone();
        let thread = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            cancel.store(true, Ordering::Relaxed);
        });
        let result = git.run(temp.path(), &["--version"], &flag, &[]);
        thread.join().unwrap();
        assert_eq!(result.unwrap_err().code, ErrorCode::Cancelled);
    }

    #[test]
    fn runner_limits_output_and_withholds_stderr() {
        let temp = tempfile::tempdir().unwrap();
        let git = fake_git(temp.path(), "exec /usr/bin/yes x");
        assert_eq!(git.version().unwrap_err().code, ErrorCode::LimitReached);
        let git = fake_git(temp.path(), "echo 'SENSITIVE CONFIG' >&2; exit 22");
        let error = git.version().unwrap_err();
        assert_eq!(error.code, ErrorCode::GitFailed);
        assert!(!error.message.contains("SENSITIVE"));
    }

    #[test]
    fn porcelain_counts_conflicts_and_rename_records() {
        let state = parse_status(b"UU conflict\0R  new name\0old name\0?? a\nb\0").unwrap();
        assert_eq!(state.conflicted, 1);
        assert_eq!(state.staged, 2);
        assert_eq!(state.unstaged, 1);
        assert_eq!(state.untracked, 1);
        assert!(parse_status(b"invalid\0").is_err());
    }
}
