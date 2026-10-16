use crate::{
    core::{path_string, AppError, ErrorCode, Result},
    database::Registry,
    git::{check_cancel, Git},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

const EXCLUDED: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    ".venv",
    "venv",
    ".next",
    ".cache",
    "Pods",
    "DerivedData",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanIssue {
    pub path: String,
    pub error: AppError,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanProgress {
    pub message: Option<String>,
    pub percent: Option<u8>,
    pub visited: usize,
    pub registered: usize,
    pub skipped: usize,
    pub issues: Vec<ScanIssue>,
}
impl ScanProgress {
    fn issue(&mut self, path: &Path, error: AppError) {
        // Bound reporting memory even on hostile directory trees.
        if self.issues.len() < 100 {
            self.issues.push(ScanIssue {
                path: path.to_string_lossy().into_owned(),
                error,
            });
        }
        self.skipped += 1;
    }
}

pub struct ScanLimits {
    pub max_depth: usize,
    pub max_entries: usize,
}
impl Default for ScanLimits {
    fn default() -> Self {
        Self {
            max_depth: 6,
            max_entries: 20_000,
        }
    }
}

pub fn scan(
    roots: &[PathBuf],
    registry: &Registry,
    git: &Git,
    cancel: &AtomicBool,
    limits: &ScanLimits,
    mut progress: impl FnMut(&ScanProgress),
) -> Result<ScanProgress> {
    if roots.is_empty() || roots.len() > 16 {
        return Err(AppError::new(
            ErrorCode::InvalidPath,
            "Choose between 1 and 16 scan directories.",
        ));
    }
    check_cancel(cancel)?;
    git.version()?;
    let mut result = ScanProgress::default();
    let mut visited = HashSet::new();
    let mut stack = Vec::new();
    for root in roots {
        let validated = validate_root(root);
        match validated {
            Ok(root) => stack.push((root, 0)),
            Err(error) => result.issue(root, error),
        }
    }
    while let Some((path, depth)) = stack.pop() {
        check_cancel(cancel)?;
        if result.visited >= limits.max_entries {
            result.issue(
                &path,
                AppError::new(
                    ErrorCode::LimitReached,
                    "Scan entry limit reached; select a narrower directory.",
                ),
            );
            break;
        }
        if !visited.insert(path.clone()) {
            continue;
        }
        if path
            .components()
            .any(|c| c.as_os_str().to_string_lossy().starts_with(".stow-clone-"))
        {
            result.issue(&path, AppError::new(ErrorCode::RecoveryRequired, "Stow staging data must be reviewed through clone recovery; it cannot be scanned into the registry."));
            continue;
        }
        result.visited += 1;
        // Re-check at point of traversal: do not follow ordinary directory symlinks.
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                result.skipped += 1;
                continue;
            }
            Ok(meta) if !meta.is_dir() => continue,
            Err(error) => {
                result.issue(&path, AppError::io(error));
                continue;
            }
            _ => {}
        }
        match fs::symlink_metadata(path.join(".git")) {
            Ok(_) => {
                match git.inspect(&path, cancel) {
                    Ok((canonical, inspection)) => {
                        registry.register(&canonical, &inspection)?;
                        result.registered += 1;
                    }
                    Err(error) if error.code == ErrorCode::Cancelled => return Err(error),
                    Err(error) => result.issue(&path, error),
                }
                // Repositories are traversal boundaries, including unsupported ones.
                progress(&result);
                continue;
            }
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                result.issue(&path, AppError::io(error));
                continue;
            }
            _ => {}
        }
        if path.join("HEAD").is_file()
            && path.join("objects").is_dir()
            && path.join("refs").is_dir()
        {
            result.issue(
                &path,
                AppError::new(
                    ErrorCode::Unsupported,
                    "Bare repositories have no supported worktree.",
                ),
            );
            continue;
        }
        if depth >= limits.max_depth {
            result.issue(
                &path,
                AppError::new(
                    ErrorCode::LimitReached,
                    "Scan depth limit reached; select this directory directly to scan further.",
                ),
            );
            continue;
        }
        match fs::read_dir(&path) {
            Ok(entries) => {
                for entry in entries {
                    check_cancel(cancel)?;
                    // Count every directory entry, not just directories, to bound wide scans.
                    if result.visited + stack.len() >= limits.max_entries {
                        result.issue(
                            &path,
                            AppError::new(
                                ErrorCode::LimitReached,
                                "Scan entry limit reached; remaining entries were skipped.",
                            ),
                        );
                        break;
                    }
                    result.visited += 1;
                    match entry {
                        Ok(entry) => match entry.file_type() {
                            Ok(kind)
                                if kind.is_dir()
                                    && !EXCLUDED.contains(
                                        &entry.file_name().to_string_lossy().as_ref(),
                                    ) =>
                            {
                                stack.push((entry.path(), depth + 1))
                            }
                            Ok(_) => {
                                result.skipped += 1;
                            }
                            Err(error) => result.issue(&entry.path(), AppError::io(error)),
                        },
                        Err(error) => result.issue(&path, AppError::io(error)),
                    }
                }
            }
            Err(error) => result.issue(&path, AppError::io(error)),
        }
        progress(&result);
    }
    progress(&result);
    check_cancel(cancel)?;
    Ok(result)
}

fn validate_root(path: &Path) -> Result<PathBuf> {
    path_string(path)?;
    if !path.is_absolute() {
        return Err(AppError::new(
            ErrorCode::InvalidPath,
            "Scan directories must be absolute paths.",
        ));
    }
    let meta = fs::symlink_metadata(path).map_err(AppError::io)?;
    if meta.file_type().is_symlink() {
        return Err(AppError::new(
            ErrorCode::Unsupported,
            "Choose the actual directory, not a symbolic link.",
        ));
    }
    if !meta.is_dir() {
        return Err(AppError::new(
            ErrorCode::InvalidPath,
            "Scan roots must be directories.",
        ));
    }
    path.canonicalize().map_err(AppError::io)
}

pub fn inspect_registered(
    registry: &Registry,
    git: &Git,
    id: i64,
    cancel: &AtomicBool,
) -> Result<()> {
    let project = registry.get(id)?;
    match git.inspect(Path::new(&project.path), cancel) {
        Ok((path, inspection)) => {
            if path_string(&path)? != project.path {
                let error = AppError::new(ErrorCode::Unsupported, "Registered path now resolves to a different directory; remove and rescan it explicitly.");
                registry.mark_error(id, &error)?;
                return Err(error);
            }
            registry.register(&path, &inspection)
        }
        Err(error) => {
            if error.code != ErrorCode::Cancelled {
                registry.mark_error(id, &error)?;
            }
            Err(error)
        }
    }
}
