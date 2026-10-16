use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
};
use stow_core::{
    core::ErrorCode,
    database::Registry,
    git::{redact_remote, Git},
    operations::{OperationState, Operations},
    projects::{inspect_registered, scan, ScanLimits},
};
use tempfile::TempDir;

fn git(path: &Path, args: &[&str]) {
    let result = Command::new("/usr/bin/git")
        .current_dir(path)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args([
            "-c",
            "user.name=Stow Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgSign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "fixture Git {:?}: {}",
        args,
        String::from_utf8_lossy(&result.stderr)
    );
}
fn repo(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    fs::create_dir_all(&path).unwrap();
    git(&path, &["init", "-b", "main"]);
    fs::write(path.join("tracked.txt"), "original\n").unwrap();
    git(&path, &["add", "tracked.txt"]);
    git(&path, &["commit", "-m", "fixture"]);
    path.canonicalize().unwrap()
}
fn inspect(path: &Path) -> stow_core::core::Inspection {
    Git::default()
        .inspect(path, &AtomicBool::new(false))
        .unwrap()
        .1
}
fn database(root: &Path) -> Registry {
    Registry::open(&root.join("registry.sqlite3")).unwrap()
}

#[test]
fn clean_repository_without_remote_or_upstream() {
    let temp = TempDir::new().unwrap();
    let path = repo(temp.path(), "clean");
    let state = inspect(&path);
    assert_eq!(state.branch.as_deref(), Some("main"));
    assert!(state.commit.is_some());
    assert!(!state.detached);
    assert!(state.working_tree.is_clean());
    assert!(state.remotes.is_empty());
    assert!(state.upstream.is_none());
}

#[test]
fn modified_staged_untracked_and_unusual_filenames() {
    let temp = TempDir::new().unwrap();
    let path = repo(temp.path(), "space ' $(touch NEVER) ; 日本語\nproject");
    fs::write(path.join("tracked.txt"), "changed and longer\n").unwrap();
    assert_eq!(inspect(&path).working_tree.unstaged, 1);
    git(&path, &["add", "tracked.txt"]);
    fs::write(path.join("a\nb file"), "untracked").unwrap();
    let state = inspect(&path);
    assert_eq!(state.working_tree.staged, 1);
    assert_eq!(state.working_tree.unstaged, 0);
    assert_eq!(state.working_tree.untracked, 1);
    assert!(!path.join("NEVER").exists());
}

#[test]
fn detached_and_unborn_heads() {
    let temp = TempDir::new().unwrap();
    let path = repo(temp.path(), "detached");
    git(&path, &["checkout", "--detach"]);
    let state = inspect(&path);
    assert!(state.detached);
    assert!(state.branch.is_none());
    assert!(state.commit.is_some());
    let unborn = temp.path().join("unborn");
    fs::create_dir(&unborn).unwrap();
    git(&unborn, &["init", "-b", "main"]);
    let state = inspect(&unborn);
    assert_eq!(state.branch.as_deref(), Some("main"));
    assert!(state.commit.is_none());
    assert!(!state.detached);
}

#[test]
fn remote_credentials_are_redacted_before_persistence() {
    let temp = TempDir::new().unwrap();
    let path = repo(temp.path(), "remote");
    git(
        &path,
        &[
            "remote",
            "add",
            "origin",
            "https://user:TOP_SECRET@example.com/team/repo.git?token=TOP_SECRET#TOP_SECRET",
        ],
    );
    let state = inspect(&path);
    assert_eq!(state.remotes[0].url, "https://example.com/team/repo.git");
    let db = database(temp.path());
    db.register(&path, &state).unwrap();
    assert!(!serde_json::to_string(&db.list().unwrap())
        .unwrap()
        .contains("TOP_SECRET"));
    drop(db);
    assert!(
        !String::from_utf8_lossy(&fs::read(temp.path().join("registry.sqlite3")).unwrap())
            .contains("TOP_SECRET")
    );
    assert_eq!(
        redact_remote("git@github.com:team/repo.git"),
        "github.com:team/repo.git"
    );
    assert_eq!(
        redact_remote("ext::danger"),
        "[local or nonstandard remote configured; URL withheld]"
    );
}

#[test]
fn reads_upstream_without_network_access() {
    let temp = TempDir::new().unwrap();
    let path = repo(temp.path(), "upstream");
    git(
        &path,
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/repo.git",
        ],
    );
    git(&path, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(&path, &["branch", "--set-upstream-to=origin/main"]);
    assert_eq!(inspect(&path).upstream.as_deref(), Some("origin/main"));
}

#[test]
fn registration_deduplicates_persists_and_removal_preserves_files() {
    let temp = TempDir::new().unwrap();
    let path = repo(temp.path(), "project");
    let db = database(temp.path());
    db.register(&path, &inspect(&path)).unwrap();
    let first = db.list().unwrap()[0].clone();
    db.register(&path.join("."), &inspect(&path)).unwrap();
    assert_eq!(db.list().unwrap().len(), 1);
    assert_eq!(db.list().unwrap()[0].registered_at, first.registered_at);
    drop(db);
    let db = database(temp.path());
    assert_eq!(db.list().unwrap()[0].id, first.id);
    db.remove(first.id).unwrap();
    assert!(db.list().unwrap().is_empty());
    assert!(path.join("tracked.txt").is_file());
    assert!(path.join(".git").is_dir());
}

#[test]
fn missing_workspace_keeps_registration_and_last_success() {
    let temp = TempDir::new().unwrap();
    let path = repo(temp.path(), "project");
    let db = database(temp.path());
    db.register(&path, &inspect(&path)).unwrap();
    let original = db.list().unwrap()[0].clone();
    fs::rename(&path, temp.path().join("moved")).unwrap();
    let error =
        inspect_registered(&db, &Git::default(), original.id, &AtomicBool::new(false)).unwrap_err();
    assert_eq!(error.code, ErrorCode::Missing);
    let project = &db.list().unwrap()[0];
    assert_eq!(project.availability, "missing");
    assert_eq!(project.inspected_at, original.inspected_at);
    assert!(project.inspection.is_some());
}

#[test]
fn database_failure_and_future_migration_are_reported() {
    let temp = TempDir::new().unwrap();
    assert!(Registry::open(temp.path()).is_err());
    let file = temp.path().join("future.sqlite3");
    let db = rusqlite::Connection::open(&file).unwrap();
    db.execute_batch("PRAGMA user_version=99").unwrap();
    drop(db);
    assert!(matches!(Registry::open(&file), Err(e) if e.code == ErrorCode::Database));
}

#[test]
fn discovery_deduplicates_and_stops_at_repository_boundaries() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("scan");
    fs::create_dir(&root).unwrap();
    let first = repo(&root, "first");
    repo(&first, "nested");
    repo(&root, "second");
    repo(&root, "node_modules/excluded");
    repo(&root, "target/excluded");
    let db = database(temp.path());
    let result = scan(
        &[root.clone(), root],
        &db,
        &Git::default(),
        &AtomicBool::new(false),
        &ScanLimits::default(),
        |_| {},
    )
    .unwrap();
    assert_eq!(result.registered, 2);
    assert_eq!(db.list().unwrap().len(), 2);
    assert!(!db
        .list()
        .unwrap()
        .iter()
        .any(|p| p.name == "nested" || p.name == "excluded"));
}

#[test]
fn discovery_empty_invalid_and_inaccessible_roots_do_not_block_others() {
    let temp = TempDir::new().unwrap();
    let empty = temp.path().join("empty");
    fs::create_dir(&empty).unwrap();
    let db = database(temp.path());
    let result = scan(
        std::slice::from_ref(&empty),
        &db,
        &Git::default(),
        &AtomicBool::new(false),
        &ScanLimits::default(),
        |_| {},
    )
    .unwrap();
    assert_eq!(result.registered, 0);
    assert!(result.issues.is_empty());
    let valid = repo(temp.path(), "valid");
    let result = scan(
        &[
            temp.path().join("missing"),
            PathBuf::from("relative"),
            valid,
        ],
        &db,
        &Git::default(),
        &AtomicBool::new(false),
        &ScanLimits::default(),
        |_| {},
    )
    .unwrap();
    assert_eq!(result.registered, 1);
    assert_eq!(result.issues.len(), 2);
}

#[cfg(unix)]
#[test]
fn permissions_and_symlinks_are_not_followed() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let temp = TempDir::new().unwrap();
    let outside = repo(temp.path(), "outside");
    let root = temp.path().join("scan");
    fs::create_dir(&root).unwrap();
    symlink(&outside, root.join("link")).unwrap();
    let blocked = root.join("blocked");
    fs::create_dir(&blocked).unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
    let db = database(temp.path());
    let result = scan(
        std::slice::from_ref(&root),
        &db,
        &Git::default(),
        &AtomicBool::new(false),
        &ScanLimits::default(),
        |_| {},
    )
    .unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(result.registered, 0);
    assert!(
        result
            .issues
            .iter()
            .any(|x| x.error.code == ErrorCode::PermissionDenied),
        "Run these tests as an ordinary user, not root"
    );
    let result = scan(
        &[root.join("link")],
        &db,
        &Git::default(),
        &AtomicBool::new(false),
        &ScanLimits::default(),
        |_| {},
    )
    .unwrap();
    assert_eq!(result.issues[0].error.code, ErrorCode::Unsupported);
}

#[test]
fn linked_worktree_is_supported_but_submodule_is_not() {
    let temp = TempDir::new().unwrap();
    let main = repo(temp.path(), "main");
    let linked = temp.path().join("linked");
    git(
        &main,
        &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
    );
    assert_eq!(inspect(&linked).branch.as_deref(), Some("feature"));
    let child = repo(temp.path(), "child");
    git(
        &main,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            child.to_str().unwrap(),
            "sub",
        ],
    );
    assert_eq!(
        Git::default()
            .inspect(&main.join("sub"), &AtomicBool::new(false))
            .unwrap_err()
            .code,
        ErrorCode::Unsupported
    );
}

#[test]
fn malicious_config_helpers_are_never_executed() {
    let temp = TempDir::new().unwrap();
    let path = repo(temp.path(), "untrusted");
    let marker = temp.path().join("HELPER_EXECUTED");
    let command = format!("touch '{}'", marker.display());
    for key in [
        "core.fsmonitor",
        "diff.external",
        "core.sshCommand",
        "credential.helper",
    ] {
        git(&path, &["config", key, &command]);
    }
    fs::write(
        path.join(".git/hooks/post-index-change"),
        format!("#!/bin/sh\n{command}\n"),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            path.join(".git/hooks/post-index-change"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    fs::write(path.join("tracked.txt"), "modified\n").unwrap();
    assert_eq!(inspect(&path).working_tree.unstaged, 1);
    assert!(!marker.exists());
    git(&path, &["config", "filter.evil.clean", &command]);
    fs::write(path.join(".gitattributes"), "* filter=evil\n").unwrap();
    assert_eq!(
        Git::default()
            .inspect(&path, &AtomicBool::new(false))
            .unwrap_err()
            .code,
        ErrorCode::Unsupported
    );
    assert!(!marker.exists());
}

#[test]
fn includes_and_extended_repository_layouts_are_rejected() {
    let temp = TempDir::new().unwrap();
    let path = repo(temp.path(), "include");
    let config = temp.path().join("external-config");
    fs::write(&config, "this is deliberately invalid configuration").unwrap();
    git(&path, &["config", "include.path", config.to_str().unwrap()]);
    assert_eq!(
        Git::default()
            .inspect(&path, &AtomicBool::new(false))
            .unwrap_err()
            .code,
        ErrorCode::Unsupported
    );
    let other = repo(temp.path(), "extension");
    git(&other, &["config", "extensions.worktreeConfig", "true"]);
    assert_eq!(
        Git::default()
            .inspect(&other, &AtomicBool::new(false))
            .unwrap_err()
            .code,
        ErrorCode::Unsupported
    );
}

#[test]
fn missing_git_unexpected_errors_invalid_paths_and_cancellation() {
    let temp = TempDir::new().unwrap();
    let missing = Git::new(temp.path().join("missing-git"));
    assert_eq!(
        missing.version().unwrap_err().code,
        ErrorCode::GitUnavailable
    );
    let malformed = temp.path().join("malformed");
    fs::create_dir_all(malformed.join(".git")).unwrap();
    fs::write(malformed.join(".git/config"), "invalid configuration\n").unwrap();
    assert_eq!(
        Git::default()
            .inspect(&malformed, &AtomicBool::new(false))
            .unwrap_err()
            .code,
        ErrorCode::GitFailed
    );
    assert_eq!(
        Git::default()
            .inspect(Path::new("/bad\0path"), &AtomicBool::new(false))
            .unwrap_err()
            .code,
        ErrorCode::InvalidPath
    );
    let path = repo(temp.path(), "cancel");
    assert_eq!(
        Git::default()
            .inspect(&path, &AtomicBool::new(true))
            .unwrap_err()
            .code,
        ErrorCode::Cancelled
    );
    let db = database(temp.path());
    let cancel = AtomicBool::new(false);
    let result = scan(
        &[temp.path().to_owned()],
        &db,
        &Git::default(),
        &cancel,
        &ScanLimits::default(),
        |_| cancel.store(true, Ordering::Relaxed),
    );
    assert_eq!(result.unwrap_err().code, ErrorCode::Cancelled);
}

#[test]
fn scan_limits_and_error_propagation() {
    let temp = TempDir::new().unwrap();
    repo(temp.path(), "deep/a/b/project");
    let db = database(temp.path());
    let result = scan(
        &[temp.path().to_owned()],
        &db,
        &Git::default(),
        &AtomicBool::new(false),
        &ScanLimits {
            max_depth: 1,
            max_entries: 100,
        },
        |_| {},
    )
    .unwrap();
    assert_eq!(result.registered, 0);
    assert!(result
        .issues
        .iter()
        .any(|i| i.error.code == ErrorCode::LimitReached));
    let result = scan(
        &[temp.path().to_owned()],
        &db,
        &Git::default(),
        &AtomicBool::new(false),
        &ScanLimits {
            max_depth: 6,
            max_entries: 1,
        },
        |_| {},
    )
    .unwrap();
    assert!(result
        .issues
        .iter()
        .any(|i| i.error.code == ErrorCode::LimitReached));
    assert_eq!(
        scan(
            &[temp.path().to_owned()],
            &db,
            &Git::new(temp.path().join("absent")),
            &AtomicBool::new(false),
            &ScanLimits::default(),
            |_| {}
        )
        .unwrap_err()
        .code,
        ErrorCode::GitUnavailable
    );
}

#[test]
fn operation_lifecycle_cancellation_and_history() {
    let operations = Operations::default();
    let (operation, flag) = operations.start("scan").unwrap();
    assert_eq!(operations.start("scan").unwrap_err().code, ErrorCode::Busy);
    operations.cancel(&operation.id).unwrap();
    assert!(flag.load(Ordering::Relaxed));
    operations
        .finish(
            &operation.id,
            Err(stow_core::core::AppError::new(
                ErrorCode::Cancelled,
                "cancelled",
            )),
        )
        .unwrap();
    assert_eq!(
        operations.list().unwrap()[0].state,
        OperationState::Cancelled
    );
    assert!(operations.list().unwrap()[0].completed_at.is_some());
    for _ in 0..105 {
        let (op, _) = operations.start("inspection").unwrap();
        operations.finish(&op.id, Ok(())).unwrap();
    }
    assert_eq!(operations.list().unwrap().len(), 100);
    assert_eq!(
        operations.list().unwrap()[0].state,
        OperationState::Completed
    );
}

#[cfg(unix)]
#[test]
fn symlinked_metadata_and_replaced_registration_fail_closed() {
    use std::os::unix::fs::symlink;
    let temp = TempDir::new().unwrap();
    let path = repo(temp.path(), "original");
    let db = database(temp.path());
    db.register(&path, &inspect(&path)).unwrap();
    let id = db.list().unwrap()[0].id;
    let moved = temp.path().join("moved");
    fs::rename(&path, &moved).unwrap();
    symlink(&moved, &path).unwrap();
    assert_eq!(
        inspect_registered(&db, &Git::default(), id, &AtomicBool::new(false))
            .unwrap_err()
            .code,
        ErrorCode::Unsupported
    );
    let marker_path = temp.path().join("marker");
    fs::create_dir(&marker_path).unwrap();
    symlink(moved.join(".git"), marker_path.join(".git")).unwrap();
    assert_eq!(
        Git::default()
            .inspect(&marker_path, &AtomicBool::new(false))
            .unwrap_err()
            .code,
        ErrorCode::Unsupported
    );
    let config = moved.join(".git/config");
    let original_config = moved.join(".git/original-config");
    fs::rename(&config, &original_config).unwrap();
    symlink(&original_config, &config).unwrap();
    assert_eq!(
        Git::default()
            .inspect(&moved, &AtomicBool::new(false))
            .unwrap_err()
            .code,
        ErrorCode::Unsupported
    );
}

#[test]
fn bare_and_unsupported_repositories_report_issues_without_stopping_scan() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("scan");
    fs::create_dir(&root).unwrap();
    let bare = root.join("bare.git");
    git(&root, &["init", "--bare", bare.to_str().unwrap()]);
    let filtered = repo(&root, "filtered");
    git(&filtered, &["config", "filter.custom.clean", "false"]);
    repo(&root, "supported");
    let db = database(temp.path());
    let result = scan(
        &[root],
        &db,
        &Git::default(),
        &AtomicBool::new(false),
        &ScanLimits::default(),
        |_| {},
    )
    .unwrap();
    assert_eq!(result.registered, 1);
    assert_eq!(result.issues.len(), 2);
    assert!(result
        .issues
        .iter()
        .all(|i| i.error.code == ErrorCode::Unsupported));
}
