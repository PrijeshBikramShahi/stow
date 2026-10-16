use super::*;
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
};
use std::{
    process::Stdio,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
    thread,
};
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    source: PathBuf,
    db_path: PathBuf,
    repo: Repository,
}
impl Fixture {
    fn new() -> Self {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().canonicalize().unwrap();
        let root = base.join("workspaces");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let source = base.join("source");
        fs::create_dir(&source).unwrap();
        for args in [
            vec!["init", "-q", "--initial-branch=main"],
            vec!["config", "user.name", "Fixture"],
            vec!["config", "user.email", "fixture@example.invalid"],
        ] {
            git(&source, &args);
        }
        fs::write(source.join("hello.txt"), "isolated fixture\n").unwrap();
        git(&source, &["add", "hello.txt"]);
        git(
            &source,
            &["-c", "core.hooksPath=/dev/null", "commit", "-qm", "fixture"],
        );
        let repo=serde_json::from_value(serde_json::json!({"id":42,"owner":"alice","name":"fixture","full_name":"alice/fixture","description":null,"web_url":"https://github.com/alice/fixture","https_url":"https://github.com/alice/fixture.git","ssh_url":"git@github.com:alice/fixture.git","visibility":"public","fork":false,"archived":false,"default_branch":"main","pushed_at":null,"updated_at":"2026-10-16T00:00:00Z"})).unwrap();
        Self {
            _temp: t,
            root,
            source,
            db_path: base.join("registry.sqlite"),
            repo,
        }
    }
    fn db(&self) -> Registry {
        Registry::open(&self.db_path).unwrap()
    }
    fn run(&self, db: &Registry, source: &dyn Source, cancel: &AtomicBool) -> Result<CloneJob> {
        execute_with(
            db,
            &Git::default(),
            source,
            &self.repo,
            self.root.to_str().unwrap(),
            "fixture",
            cancel,
            &mut |_, _| {},
        )
    }
}
fn git(path: &Path, args: &[&str]) {
    let status = Command::new(Git::default().executable())
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .current_dir(path)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "{args:?}");
}
struct Local(PathBuf);
impl Source for Local {
    fn verify(&self, r: &Repository, c: &AtomicBool) -> Result<Repository> {
        check_cancel(c)?;
        Ok(r.clone())
    }
    fn populate(
        &self,
        r: &Repository,
        d: &Directory,
        c: &AtomicBool,
        p: &mut dyn FnMut(&str, Option<u8>),
    ) -> Result<()> {
        let mut runner = CloneRunner::new(&Git::default(), &Github::default());
        runner.local_source = Some(self.0.clone());
        populate(&runner, r, d, c, p)
    }
}
#[test]
fn clone_register_restart_preferences_and_library() {
    let f = Fixture::new();
    let db = f.db();
    db.save_preferences(&Preferences {
        root: Some(f.root.to_str().unwrap().into()),
        editor: "cursor".into(),
    })
    .unwrap();
    db.apply_github(
        &crate::github::Snapshot {
            account: crate::github::Account {
                id: 7,
                login: "alice".into(),
            },
            repositories: vec![f.repo.clone()],
        },
        &AtomicBool::new(false),
    )
    .unwrap();
    let job = f
        .run(&db, &Local(f.source.clone()), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(job.state, "completed");
    assert_eq!(
        fs::read_to_string(f.root.join("fixture/hello.txt")).unwrap(),
        "isolated fixture\n"
    );
    assert!(f.root.join(&job.staging).is_dir());
    assert_eq!(db.library(None).unwrap().links[0].repository_id, Some(42));
    drop(db);
    let db = f.db();
    assert_eq!(db.list().unwrap().len(), 1);
    assert_eq!(db.clone_job(&job.id).unwrap().state, "completed");
    assert_eq!(db.preferences().unwrap().editor, "cursor");
    assert_eq!(db.preferences().unwrap().root.as_deref(), f.root.to_str());
    assert!(crate::editor::open_workspace(&f.root.join("fixture"), "untrusted-command").is_err());
    assert_eq!(db.clone_job(&job.id).unwrap().state, "completed");
}
#[test]
fn traversal_collisions_symlinks_and_permissions() {
    let f = Fixture::new();
    let root = f.root.to_str().unwrap();
    for name in [
        "..",
        "../outside",
        "a/b",
        "/absolute",
        ".git",
        "-option",
        "",
    ] {
        assert!(validate_destination(root, name).is_err());
    }
    fs::write(f.root.join("existing"), "keep").unwrap();
    assert_eq!(
        validate_destination(root, "existing").err().unwrap().code,
        ErrorCode::Collision
    );
    symlink("/nonexistent", f.root.join("dangling")).unwrap();
    assert_eq!(
        validate_destination(root, "dangling").err().unwrap().code,
        ErrorCode::Collision
    );
    let alias = f.root.with_file_name("alias");
    symlink(&f.root, &alias).unwrap();
    assert!(validate_destination(alias.to_str().unwrap(), "new").is_err());
    fs::create_dir(f.root.join("nested")).unwrap();
    assert!(Directory::open(&alias.join("nested")).is_err());
    fs::set_permissions(&f.root, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(validate_destination(root, "new").is_err());
    assert_eq!(fs::read_to_string(f.root.join("existing")).unwrap(), "keep");
}
#[test]
fn root_lock_and_exclusive_promotion() {
    let f = Fixture::new();
    let a = Directory::open(&f.root).unwrap();
    let b = Directory::open(&f.root).unwrap();
    a.lock().unwrap();
    assert!(b.lock().is_err());
    let stage = a.create("stage").unwrap();
    let repo = stage.create("workspace").unwrap();
    fs::write(f.root.join("collision"), "keep").unwrap();
    assert_eq!(
        stage
            .promote("workspace", repo.identity().unwrap(), &a, "collision")
            .unwrap_err()
            .code,
        ErrorCode::Collision
    );
    assert!(stage.child("workspace").is_ok());
    assert_eq!(
        fs::read_to_string(f.root.join("collision")).unwrap(),
        "keep"
    );
    drop(a);
    b.lock().unwrap();
}
#[test]
fn replaced_root_and_staging_rejected() {
    let f = Fixture::new();
    let root = Directory::open(&f.root).unwrap();
    fs::rename(&f.root, f.root.with_file_name("old")).unwrap();
    fs::create_dir(&f.root).unwrap();
    assert!(root.validate_path().is_err());
    let stage = root.create("stage");
    assert!(stage.is_err());
}
struct Fail(bool);
impl Source for Fail {
    fn verify(&self, r: &Repository, _: &AtomicBool) -> Result<Repository> {
        Ok(r.clone())
    }
    fn populate(
        &self,
        _: &Repository,
        d: &Directory,
        c: &AtomicBool,
        _: &mut dyn FnMut(&str, Option<u8>),
    ) -> Result<()> {
        fs::write(d.path.join("partial"), "retained").unwrap();
        if self.0 {
            c.store(true, Ordering::Relaxed);
            check_cancel(c)
        } else {
            Err(error(ErrorCode::CloneFailed, "fixture failure"))
        }
    }
}
#[test]
fn failure_and_cancellation_retain_journal_and_data() {
    for cancelled in [false, true] {
        let f = Fixture::new();
        let db = f.db();
        let e = f
            .run(&db, &Fail(cancelled), &AtomicBool::new(false))
            .unwrap_err();
        assert_eq!(
            e.code,
            if cancelled {
                ErrorCode::Cancelled
            } else {
                ErrorCode::CloneFailed
            }
        );
        let job = db.clone_jobs().unwrap().remove(0);
        assert!(db.list().unwrap().is_empty());
        assert!(!f.root.join("fixture").exists());
        assert_eq!(
            fs::read_to_string(f.root.join(&job.staging).join("workspace/partial")).unwrap(),
            "retained"
        );
        drop(db);
        let db = f.db();
        assert!(recover(
            &db,
            &Git::default(),
            &job.id,
            &AtomicBool::new(false),
            &mut |_, _| {}
        )
        .is_err());
        assert_eq!(db.clone_job(&job.id).unwrap().phase, "creating");
    }
}
#[test]
fn registration_failure_and_rename_gap_recover_without_losing_workspace() {
    for crash_gap in [false, true] {
        let f = Fixture::new();
        let db = f.db();
        let connection = rusqlite::Connection::open(&f.db_path).unwrap();
        connection.execute_batch("CREATE TRIGGER refuse_registration BEFORE INSERT ON projects BEGIN SELECT RAISE(FAIL,'fixture'); END;").unwrap();
        assert_eq!(
            f.run(&db, &Local(f.source.clone()), &AtomicBool::new(false))
                .unwrap_err()
                .code,
            ErrorCode::Database
        );
        assert!(db.list().unwrap().is_empty());
        let mut job = db.clone_jobs().unwrap().remove(0);
        assert_eq!(job.phase, "promoted");
        assert!(f.root.join("fixture/hello.txt").is_file());
        if crash_gap {
            job.phase = "promoting".into();
            job.state = "running".into();
            db.save_clone(&job).unwrap();
        }
        connection
            .execute_batch("DROP TRIGGER refuse_registration")
            .unwrap();
        drop(db);
        let db = f.db();
        let recovered = recover(
            &db,
            &Git::default(),
            &job.id,
            &AtomicBool::new(false),
            &mut |_, _| {},
        )
        .unwrap();
        assert_eq!(recovered.state, "completed");
        assert_eq!(db.list().unwrap().len(), 1);
        assert_eq!(
            recover(
                &db,
                &Git::default(),
                &job.id,
                &AtomicBool::new(false),
                &mut |_, _| {}
            )
            .unwrap()
            .project_id,
            recovered.project_id
        );
    }
}
#[test]
fn changed_promoted_workspace_is_not_registered() {
    let f = Fixture::new();
    let db = f.db();
    let c = rusqlite::Connection::open(&f.db_path).unwrap();
    c.execute_batch(
        "CREATE TRIGGER fail BEFORE INSERT ON projects BEGIN SELECT RAISE(FAIL,'fixture'); END",
    )
    .unwrap();
    assert!(f
        .run(&db, &Local(f.source.clone()), &AtomicBool::new(false))
        .is_err());
    let job = db.clone_jobs().unwrap().remove(0);
    c.execute_batch("DROP TRIGGER fail").unwrap();
    fs::write(f.root.join("fixture/hello.txt"), "user changes").unwrap();
    assert!(recover(
        &db,
        &Git::default(),
        &job.id,
        &AtomicBool::new(false),
        &mut |_, _| {}
    )
    .is_err());
    assert!(db.list().unwrap().is_empty());
    assert_eq!(
        fs::read_to_string(f.root.join("fixture/hello.txt")).unwrap(),
        "user changes"
    );
}
#[test]
fn scan_cannot_register_staging_even_when_selected_directly() {
    let f = Fixture::new();
    let db = f.db();
    let stage = f.root.join(".stow-clone-fixture");
    fs::create_dir(&stage).unwrap();
    let path = stage.join("workspace");
    fs::create_dir(&path).unwrap();
    git(&path, &["init", "-q"]);
    for root in [&f.root, &path] {
        crate::projects::scan(
            std::slice::from_ref(root),
            &db,
            &Git::default(),
            &AtomicBool::new(false),
            &crate::projects::ScanLimits::default(),
            |_| {},
        )
        .unwrap();
    }
    assert!(db.list().unwrap().is_empty());
}
#[test]
fn timeout_and_cancellation_kill_process_group() {
    let f = Fixture::new();
    let dir = Directory::open(&f.root).unwrap();
    let script = f.root.join("fake-git");
    fs::write(&script, "#!/bin/sh\n/bin/sleep 30 &\nwait\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let runner = CloneRunner {
        git: script,
        gh: "/opt/homebrew/bin/gh".into(),
        timeout: Duration::from_millis(60),
        local_source: None,
    };
    let start = Instant::now();
    assert_eq!(
        runner
            .run(
                &dir,
                &[],
                &AtomicBool::new(false),
                Instant::now() + runner.timeout,
                false,
                &mut |_, _| {}
            )
            .unwrap_err()
            .code,
        ErrorCode::Timeout
    );
    assert!(start.elapsed() < Duration::from_secs(3));
    let cancel = AtomicBool::new(false);
    std::thread::scope(|s| {
        s.spawn(|| {
            thread::sleep(Duration::from_millis(60));
            cancel.store(true, Ordering::Relaxed);
        });
        assert_eq!(
            runner
                .run(
                    &dir,
                    &[],
                    &cancel,
                    Instant::now() + Duration::from_secs(10),
                    false,
                    &mut |_, _| {}
                )
                .unwrap_err()
                .code,
            ErrorCode::Cancelled
        );
    });
}
#[test]
fn bounded_progress_never_exposes_process_text() {
    let overflow = Arc::new(AtomicBool::new(false));
    let percent = Arc::new(AtomicU8::new(255));
    drain(
        &b"secret-token\nReceiving objects: 42% (4/10)\r"[..],
        overflow.clone(),
        Some(percent.clone()),
    )
    .unwrap();
    assert_eq!(percent.load(Ordering::Relaxed), 42);
    let bytes = vec![b'x'; 8 * 1024 * 1024 + 1];
    assert_eq!(
        drain(bytes.as_slice(), overflow.clone(), None)
            .unwrap()
            .len(),
        8 * 1024 * 1024
    );
    assert!(overflow.load(Ordering::Relaxed));
}

struct Collide(Local, PathBuf);
impl Source for Collide {
    fn verify(&self, r: &Repository, c: &AtomicBool) -> Result<Repository> {
        self.0.verify(r, c)
    }
    fn populate(
        &self,
        r: &Repository,
        d: &Directory,
        c: &AtomicBool,
        p: &mut dyn FnMut(&str, Option<u8>),
    ) -> Result<()> {
        self.0.populate(r, d, c, p)?;
        fs::write(&self.1, "unrelated concurrent data").unwrap();
        Ok(())
    }
}
#[test]
fn collision_after_clone_retains_verified_stage_then_explicit_recovery() {
    let f = Fixture::new();
    let db = f.db();
    assert_eq!(
        f.run(
            &db,
            &Collide(Local(f.source.clone()), f.root.join("fixture")),
            &AtomicBool::new(false)
        )
        .unwrap_err()
        .code,
        ErrorCode::Collision
    );
    let job = db.clone_jobs().unwrap().remove(0);
    assert_eq!(job.phase, "verified");
    assert!(db.list().unwrap().is_empty());
    assert!(f
        .root
        .join(&job.staging)
        .join("workspace/hello.txt")
        .is_file());
    assert!(recover(
        &db,
        &Git::default(),
        &job.id,
        &AtomicBool::new(false),
        &mut |_, _| {}
    )
    .is_err());
    fs::rename(f.root.join("fixture"), f.root.join("preserved-unrelated")).unwrap();
    let recovered = recover(
        &db,
        &Git::default(),
        &job.id,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .unwrap();
    assert_eq!(recovered.state, "completed");
    assert_eq!(
        fs::read_to_string(f.root.join("preserved-unrelated")).unwrap(),
        "unrelated concurrent data"
    );
}
#[test]
fn cancellation_after_verification_is_recoverable() {
    let f = Fixture::new();
    let db = f.db();
    let cancel = AtomicBool::new(false);
    let outcome = execute_with(
        &db,
        &Git::default(),
        &Local(f.source.clone()),
        &f.repo,
        f.root.to_str().unwrap(),
        "fixture",
        &cancel,
        &mut |message, _| {
            if message.starts_with("Promoting") {
                cancel.store(true, Ordering::Relaxed);
            }
        },
    );
    assert_eq!(outcome.unwrap_err().code, ErrorCode::Cancelled);
    let job = db.clone_jobs().unwrap().remove(0);
    assert!(job.verified.is_some());
    assert!(db.list().unwrap().is_empty());
    assert_eq!(
        recover(
            &db,
            &Git::default(),
            &job.id,
            &AtomicBool::new(false),
            &mut |_, _| {}
        )
        .unwrap()
        .state,
        "completed"
    );
}
#[test]
fn empty_repository_clone_is_registered_without_a_commit() {
    let f = Fixture::new();
    let empty = f.source.with_file_name("empty");
    fs::create_dir(&empty).unwrap();
    git(&empty, &["init", "-q", "--initial-branch=main"]);
    let db = f.db();
    let job = f.run(&db, &Local(empty), &AtomicBool::new(false)).unwrap();
    assert!(job.verified.unwrap().commit.is_none());
    assert_eq!(db.list().unwrap().len(), 1);
}
#[test]
fn failed_fetch_keeps_initialized_repository() {
    let f = Fixture::new();
    let db = f.db();
    assert!(f
        .run(
            &db,
            &Local(f.source.join("missing")),
            &AtomicBool::new(false)
        )
        .is_err());
    let job = db.clone_jobs().unwrap().remove(0);
    assert!(f.root.join(&job.staging).join("workspace/.git").is_dir());
    assert!(db.list().unwrap().is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn extended_acl_root_is_rejected_without_changing_permissions() {
    let f = Fixture::new();
    let result = Command::new("/bin/chmod")
        .args(["+a", "everyone allow add_file,add_subdirectory"])
        .arg(&f.root)
        .status()
        .unwrap();
    assert!(result.success());
    assert_eq!(
        Directory::open(&f.root).unwrap().owned().unwrap_err().code,
        ErrorCode::PermissionDenied
    );
}
#[test]
fn replaced_stage_identity_blocks_recovery() {
    let f = Fixture::new();
    let db = f.db();
    assert!(f
        .run(
            &db,
            &Collide(Local(f.source.clone()), f.root.join("fixture")),
            &AtomicBool::new(false)
        )
        .is_err());
    let job = db.clone_jobs().unwrap().remove(0);
    fs::rename(f.root.join(&job.staging), f.root.join("original-stage")).unwrap();
    fs::create_dir(f.root.join(&job.staging)).unwrap();
    assert!(recover(
        &db,
        &Git::default(),
        &job.id,
        &AtomicBool::new(false),
        &mut |_, _| {}
    )
    .is_err());
    assert!(f.root.join("original-stage/workspace/hello.txt").is_file());
    assert!(db.list().unwrap().is_empty());
}

#[test]
fn exited_leader_does_not_leave_pipe_holding_descendants() {
    let f = Fixture::new();
    let dir = Directory::open(&f.root).unwrap();
    let script = f.root.join("fake-git");
    fs::write(&script, "#!/bin/sh\n/bin/sleep 30 &\nexit 0\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let runner = CloneRunner {
        git: script,
        gh: "/opt/homebrew/bin/gh".into(),
        timeout: Duration::from_secs(2),
        local_source: None,
    };
    let start = Instant::now();
    assert!(
        runner
            .run(
                &dir,
                &[],
                &AtomicBool::new(false),
                Instant::now() + runner.timeout,
                false,
                &mut |_, _| {}
            )
            .unwrap()
            .0
    );
    assert!(start.elapsed() < Duration::from_secs(2));
}

/// Opt-in real GitHub backend smoke. Caller supplies a new private temporary root;
/// cloned files and the independent smoke registry are intentionally retained.
#[test]
#[ignore = "requires saved gh authentication, network, and STOW_LIVE_CLONE_ROOT"]
fn live_github_backend_smoke() {
    let root = std::env::var("STOW_LIVE_CLONE_ROOT").expect("explicit temporary root required");
    assert!(root.starts_with("/private/tmp/stow-phase2b-smoke-"));
    let path = Path::new(&root);
    assert_eq!(fs::read_dir(path).unwrap().count(), 0);
    let repository:Repository=serde_json::from_value(serde_json::json!({"id":518885440,"owner":"PrijeshBikramShahi","name":"PrijeshBikramShahi","full_name":"PrijeshBikramShahi/PrijeshBikramShahi","description":null,"web_url":"https://github.com/PrijeshBikramShahi/PrijeshBikramShahi","https_url":"https://github.com/PrijeshBikramShahi/PrijeshBikramShahi.git","ssh_url":"git@github.com:PrijeshBikramShahi/PrijeshBikramShahi.git","visibility":"public","fork":false,"archived":false,"default_branch":"main","pushed_at":null,"updated_at":"2026-10-16T00:00:00Z"})).unwrap();
    let db = Registry::open(&path.join("smoke.sqlite3")).unwrap();
    let job = execute(
        &db,
        &Git::default(),
        &repository,
        &root,
        "profile-smoke",
        &AtomicBool::new(false),
        &mut |message, percent| println!("{message} {percent:?}"),
    )
    .unwrap();
    assert_eq!(job.state, "completed");
    assert_eq!(db.list().unwrap().len(), 1);
    println!("Verified live clone retained at {}", job.destination);
}
