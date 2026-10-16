use super::*;
use std::{
    os::unix::fs::{symlink, PermissionsExt},
    sync::atomic::Ordering,
    thread,
};
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    work: PathBuf,
    remote: PathBuf,
    source: PathBuf,
    db: Registry,
    id: i64,
}
fn command(path: &Path, args: &[&str]) -> String {
    let output = Command::new(Git::default().executable())
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .current_dir(path)
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "fixture command {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}
fn commit(path: &Path, name: &str) {
    fs::write(path.join(name), name).unwrap();
    command(path, &["add", name]);
    command(
        path,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            name,
        ],
    );
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source = root.join("source");
        let remote = root.join("remote.git");
        let work = root.join("work");
        fs::create_dir(&source).unwrap();
        command(&source, &["init", "-q", "--initial-branch=main"]);
        commit(&source, "initial");
        command(
            &root,
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                remote.to_str().unwrap(),
            ],
        );
        command(&source, &["push", remote.to_str().unwrap(), "main"]);
        command(
            &root,
            &[
                "clone",
                "--no-hardlinks",
                remote.to_str().unwrap(),
                work.to_str().unwrap(),
            ],
        );
        let db = Registry::open(&root.join("registry.sqlite")).unwrap();
        let (path, inspection) = Git::default()
            .inspect(&work, &AtomicBool::new(false))
            .unwrap();
        db.register(&path, &inspection).unwrap();
        let id = db.list().unwrap()[0].id;
        Self {
            _temp: temp,
            root,
            source,
            remote,
            work,
            db,
            id,
        }
    }
    fn preflight(&self) -> Assessment {
        inspect(&self.db, &Git::default(), self.id, &AtomicBool::new(false)).unwrap();
        self.db.sync_record(self.id).unwrap().assessment.unwrap()
    }
    fn fetch(&self) -> Result<()> {
        let a = self.preflight();
        fetch(
            &self.db,
            &Git::default(),
            self.id,
            &a.token,
            "origin",
            &AtomicBool::new(false),
            &mut |_, _| {},
        )
    }
    fn advance(&self) {
        commit(&self.source, "remote-change");
        command(
            &self.source,
            &["push", self.remote.to_str().unwrap(), "main"],
        );
    }
}
#[test]
fn relationships_clean_ahead_behind_and_diverged() {
    let f = Fixture::new();
    let a = f.preflight();
    assert_eq!(a.relation, "up_to_date");
    assert_eq!((a.ahead, a.behind), (Some(0), Some(0)));
    commit(&f.work, "local");
    assert_eq!(f.preflight().relation, "ahead");
    f.advance();
    f.fetch().unwrap();
    let a = f.preflight();
    assert_eq!(a.relation, "diverged");
    assert_eq!((a.ahead, a.behind), (Some(1), Some(1)));
}
#[test]
fn fetch_preserves_dirty_worktree_head_index_and_refreshes_status() {
    let f = Fixture::new();
    f.advance();
    fs::write(f.work.join("initial"), "dirty tracked").unwrap();
    fs::write(f.work.join("untracked"), "untouched").unwrap();
    fs::write(f.work.join("staged"), "staged").unwrap();
    command(&f.work, &["add", "staged"]);
    let head = command(&f.work, &["rev-parse", "HEAD"]);
    let index = fs::read(f.work.join(".git/index")).unwrap();
    f.fetch().unwrap();
    assert_eq!(command(&f.work, &["rev-parse", "HEAD"]), head);
    assert_eq!(fs::read(f.work.join(".git/index")).unwrap(), index);
    assert_eq!(
        fs::read_to_string(f.work.join("initial")).unwrap(),
        "dirty tracked"
    );
    assert_eq!(
        fs::read_to_string(f.work.join("untracked")).unwrap(),
        "untouched"
    );
    let a = f.db.sync_record(f.id).unwrap().assessment.unwrap();
    assert_eq!(a.relation, "behind");
    assert_eq!(a.behind, Some(1));
    assert_eq!(a.inspection.working_tree.staged, 1);
    assert_eq!(a.inspection.working_tree.unstaged, 1);
    assert_eq!(a.inspection.working_tree.untracked, 1);
    assert_eq!(f.db.fetch_history(f.id).unwrap()[0].state, "completed");
    let reopened = Registry::open(&f.root.join("registry.sqlite")).unwrap();
    assert_eq!(
        reopened
            .sync_record(f.id)
            .unwrap()
            .assessment
            .unwrap()
            .behind,
        Some(1)
    );
    assert!(reopened.fetch_history(f.id).unwrap()[0]
        .completed_at
        .is_some());
}
#[test]
fn missing_upstream_tracking_detached_and_operations() {
    let f = Fixture::new();
    command(&f.work, &["branch", "--unset-upstream"]);
    assert_eq!(f.preflight().relation, "no_upstream");
    command(&f.work, &["config", "branch.main.remote", "origin"]);
    command(
        &f.work,
        &["config", "branch.main.merge", "refs/heads/missing"],
    );
    assert_eq!(f.preflight().relation, "missing_tracking_ref");
    command(
        &f.work,
        &["config", "--add", "branch.main.merge", "refs/heads/other"],
    );
    assert_eq!(f.preflight().relation, "ambiguous_upstream");
    command(&f.work, &["checkout", "--detach"]);
    assert_eq!(f.preflight().relation, "detached");
    fs::write(f.work.join(".git/MERGE_HEAD"), "fixture").unwrap();
    assert_eq!(f.preflight().operations, vec!["MERGE_HEAD"]);
    assert!(f.fetch().is_err());
}
#[test]
fn non_fast_forward_remote_is_rejected_without_pruning() {
    let f = Fixture::new();
    f.advance();
    f.fetch().unwrap();
    let tracked = command(&f.work, &["rev-parse", "refs/remotes/origin/main"]);
    let initial = command(&f.source, &["rev-parse", "HEAD~1"]);
    command(&f.remote, &["update-ref", "refs/heads/main", &initial]);
    let err = f.fetch().unwrap_err();
    assert_eq!(err.code, ErrorCode::FetchFailed);
    assert_eq!(
        command(&f.work, &["rev-parse", "refs/remotes/origin/main"]),
        tracked
    );
    assert_eq!(f.db.fetch_history(f.id).unwrap()[0].state, "failed");
    assert!(f
        .db
        .fetch_history(f.id)
        .unwrap()
        .iter()
        .any(|j| j.state == "completed"));
}
#[test]
fn remote_deleted_branch_retained_and_tags_not_fetched() {
    let f = Fixture::new();
    command(
        &f.remote,
        &[
            "update-ref",
            "refs/heads/other",
            &command(&f.remote, &["rev-parse", "main"]),
        ],
    );
    f.fetch().unwrap();
    command(&f.remote, &["update-ref", "-d", "refs/heads/other"]);
    command(&f.remote, &["tag", "remote-tag", "main"]);
    f.fetch().unwrap();
    assert!(!command(&f.work, &["rev-parse", "refs/remotes/origin/other"]).is_empty());
    assert!(command(&f.work, &["tag", "--list"]).is_empty());
}
#[test]
fn config_and_transport_restrictions() {
    let f = Fixture::new();
    for (key, value) in [
        ("core.sshCommand", "touch /should-not-run"),
        ("credential.helper", "!echo secret"),
        ("remote.origin.uploadpack", "custom"),
        ("http.sslVerify", "false"),
        ("fetch.prune", "true"),
        ("url.https://evil/.insteadOf", "https://github.com/"),
    ] {
        command(&f.work, &["config", key, value]);
        assert!(f.preflight().remotes[0].reason.is_some());
        assert!(f.fetch().is_err());
        command(&f.work, &["config", "--unset", key]);
    }
    for url in [
        "git@github.com:alice/repo.git",
        "ext::evil",
        "https://secret@github.com/alice/repo.git",
        "https://github.com/alice/repo.git?token=secret",
    ] {
        command(&f.work, &["config", "remote.origin.url", url]);
        assert!(f.preflight().remotes[0].reason.is_some());
        assert!(f.fetch().is_err());
    }
    assert!(f.db.fetch_history(f.id).unwrap().is_empty());
}
#[test]
fn authorization_rejects_path_substitution_and_remote_changes() {
    let f = Fixture::new();
    let a = f.preflight();
    let moved = f.root.join("old-work");
    fs::rename(&f.work, &moved).unwrap();
    command(
        &f.root,
        &[
            "clone",
            "--no-hardlinks",
            f.remote.to_str().unwrap(),
            f.work.to_str().unwrap(),
        ],
    );
    assert!(fetch(
        &f.db,
        &Git::default(),
        f.id,
        &a.token,
        "origin",
        &AtomicBool::new(false),
        &mut |_, _| {}
    )
    .is_err());
    assert!(inspect(&f.db, &Git::default(), f.id, &AtomicBool::new(false)).is_err());
    let f = Fixture::new();
    let a = f.preflight();
    command(
        &f.work,
        &[
            "config",
            "remote.origin.url",
            "https://github.com/alice/changed.git",
        ],
    );
    assert!(fetch(
        &f.db,
        &Git::default(),
        f.id,
        &a.token,
        "origin",
        &AtomicBool::new(false),
        &mut |_, _| {}
    )
    .is_err());
}
#[test]
fn metadata_redirects_and_conflicting_operations_rejected() {
    let f = Fixture::new();
    let a = f.preflight();
    let meta = Directory::open(&f.work.join(".git")).unwrap();
    meta.lock().unwrap();
    assert_eq!(
        fetch(
            &f.db,
            &Git::default(),
            f.id,
            &a.token,
            "origin",
            &AtomicBool::new(false),
            &mut |_, _| {}
        )
        .unwrap_err()
        .code,
        ErrorCode::Busy
    );
    drop(meta);
    symlink(f.root.join("outside"), f.work.join(".git/refs/evil")).unwrap();
    assert!(f.fetch().is_err());
}
fn fake_runner(f: &Fixture, body: &str) -> FetchRunner {
    let script = f.root.join("fake-git");
    fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    FetchRunner {
        git: script,
        timeout: Duration::from_secs(10),
    }
}
#[test]
fn failed_authentication_unavailable_timeout_and_cancellation() {
    for (code, body) in [
        (
            ErrorCode::Authentication,
            "echo 'fatal: Authentication failed secret' >&2; exit 1",
        ),
        (
            ErrorCode::Network,
            "echo 'Could not resolve host secret' >&2; exit 1",
        ),
        (ErrorCode::FetchFailed, "exit 1"),
    ] {
        let f = Fixture::new();
        let a = f.preflight();
        let runner = fake_runner(&f, body);
        let e = fetch_with(
            &f.db,
            &Git::default(),
            &runner,
            f.id,
            &a.token,
            "origin",
            &AtomicBool::new(false),
            &mut |_, _| {},
        )
        .unwrap_err();
        assert_eq!(e.code, code);
        assert!(!e.message.contains("secret"));
        assert_eq!(f.db.fetch_history(f.id).unwrap()[0].state, "failed");
    }
    let f = Fixture::new();
    let a = f.preflight();
    let mut runner = fake_runner(&f, "/bin/sleep 30 &\nwait");
    runner.timeout = Duration::from_millis(80);
    assert_eq!(
        fetch_with(
            &f.db,
            &Git::default(),
            &runner,
            f.id,
            &a.token,
            "origin",
            &AtomicBool::new(false),
            &mut |_, _| {}
        )
        .unwrap_err()
        .code,
        ErrorCode::Timeout
    );
    let a = f.preflight();
    runner.timeout = Duration::from_secs(10);
    let cancel = AtomicBool::new(false);
    thread::scope(|s| {
        s.spawn(|| {
            thread::sleep(Duration::from_millis(350));
            cancel.store(true, Ordering::Relaxed);
        });
        assert_eq!(
            fetch_with(
                &f.db,
                &Git::default(),
                &runner,
                f.id,
                &a.token,
                "origin",
                &cancel,
                &mut |_, _| {}
            )
            .unwrap_err()
            .code,
            ErrorCode::Cancelled
        );
    });
    assert!(f
        .db
        .fetch_history(f.id)
        .unwrap()
        .iter()
        .all(|j| j.state != "completed"));
}
#[test]
fn cancellation_after_transfer_never_claims_success() {
    let f = Fixture::new();
    f.advance();
    let a = f.preflight();
    let old = command(&f.work, &["rev-parse", "origin/main"]);
    let cancel = AtomicBool::new(false);
    assert_eq!(
        fetch(
            &f.db,
            &Git::default(),
            f.id,
            &a.token,
            "origin",
            &cancel,
            &mut |message, _| {
                if message.starts_with("Applying") {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
        )
        .unwrap_err()
        .code,
        ErrorCode::Cancelled
    );
    assert_eq!(command(&f.work, &["rev-parse", "origin/main"]), old);
    let history = f.db.fetch_history(f.id).unwrap();
    assert_eq!(history[0].state, "cancelled");
    assert!(Path::new(history[0].staging.as_ref().unwrap()).is_dir());
}

#[test]
fn concurrent_ref_and_head_changes_abort_without_overwrite() {
    for change_head in [false, true] {
        let f = Fixture::new();
        f.advance();
        let a = f.preflight();
        let old = command(&f.work, &["rev-parse", "origin/main"]);
        let new = command(&f.remote, &["rev-parse", "main"]);
        let result = fetch(
            &f.db,
            &Git::default(),
            f.id,
            &a.token,
            "origin",
            &AtomicBool::new(false),
            &mut |message, _| {
                if message.starts_with("Applying") {
                    if change_head {
                        command(
                            &f.work,
                            &["symbolic-ref", "HEAD", "refs/remotes/origin/main"],
                        );
                    } else {
                        command(&f.work, &["update-ref", "refs/remotes/origin/main", &new]);
                    }
                }
            },
        );
        assert!(result.is_err());
        assert_eq!(
            command(&f.work, &["rev-parse", "origin/main"]),
            if change_head { old } else { new }
        );
        assert_eq!(f.db.fetch_history(f.id).unwrap()[0].state, "failed");
        assert_eq!(
            fs::read_to_string(f.work.join("initial")).unwrap(),
            "initial"
        );
    }
}

#[test]
fn successful_transfer_with_database_failure_is_not_reported_successful() {
    let f = Fixture::new();
    f.advance();
    let connection = rusqlite::Connection::open(f.root.join("registry.sqlite")).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_success BEFORE UPDATE ON fetch_jobs WHEN json_extract(NEW.record_json,'$.state')='completed' BEGIN SELECT RAISE(FAIL,'fixture'); END;").unwrap();
    assert_eq!(f.fetch().unwrap_err().code, ErrorCode::Database);
    assert_eq!(
        command(&f.work, &["rev-parse", "origin/main"]),
        command(&f.remote, &["rev-parse", "main"])
    );
    assert_eq!(
        f.db.sync_record(f.id).unwrap().assessment.unwrap().behind,
        Some(1)
    );
    let reopened = Registry::open(&f.root.join("registry.sqlite")).unwrap();
    let jobs = reopened.fetch_history(f.id).unwrap();
    assert_eq!(jobs[0].state, "failed");
    assert!(Path::new(jobs[0].staging.as_ref().unwrap()).exists());
}

#[test]
fn unavailable_source_and_interrupted_history_remain_reviewable() {
    let f = Fixture::new();
    let a = f.preflight();
    let result = fetch(
        &f.db,
        &Git::default(),
        f.id,
        &a.token,
        "origin",
        &AtomicBool::new(false),
        &mut |message, _| {
            if message.starts_with("Fetching selected") {
                fs::rename(&f.remote, f.root.join("unavailable.git")).unwrap();
            }
        },
    );
    assert!(result.is_err());
    let mut job = f.db.fetch_history(f.id).unwrap().remove(0);
    assert_eq!(job.state, "failed");
    assert!(Path::new(job.staging.as_ref().unwrap()).exists());
    job.state = "running".into();
    job.completed_at = None;
    f.db.save_fetch(&job).unwrap();
    let reopened = Registry::open(&f.root.join("registry.sqlite")).unwrap();
    let job = reopened.fetch_history(f.id).unwrap().remove(0);
    assert_eq!(job.state, "running");
    assert!(job.completed_at.is_none());
}

#[test]
fn unsupported_history_and_custom_refspecs_do_not_fetch() {
    let f = Fixture::new();
    let head = command(&f.work, &["rev-parse", "HEAD"]);
    fs::write(f.work.join(".git/shallow"), format!("{head}\n")).unwrap();
    assert_eq!(f.preflight().relation, "unsupported");
    assert!(f.fetch().is_err());
    let f = Fixture::new();
    command(
        &f.work,
        &["config", "remote.origin.fetch", "refs/heads/*:refs/heads/*"],
    );
    assert!(f.preflight().remotes[0].reason.is_some());
    assert!(f.fetch().is_err());
    assert!(f.db.fetch_history(f.id).unwrap().is_empty());
}

#[path = "pull_tests.rs"]
mod pulls;
