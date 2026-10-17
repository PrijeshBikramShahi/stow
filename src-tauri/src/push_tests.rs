use super::*;
use crate::push::{self, PreparedPush};
fn prepare(f: &Fixture) -> Result<PreparedPush> {
    f.preflight();
    push::prepare(
        &f.db,
        &Git::default(),
        f.id,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
}
fn execute(f: &Fixture, p: &mut PreparedPush) -> Result<()> {
    p.execute(
        &f.db,
        &Git::default(),
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
}
#[test]
fn ahead_push_verifies_exact_ref_preserves_dirty_files_index_and_unrelated_refs() {
    let f = Fixture::new();
    commit(&f.work, "local");
    let old = command(&f.remote, &["rev-parse", "main"]);
    command(&f.remote, &["update-ref", "refs/heads/unrelated", &old]);
    command(&f.work, &["tag", "not-pushed"]);
    fs::write(f.work.join("initial"), "dirty").unwrap();
    fs::write(f.work.join("untracked"), "untracked").unwrap();
    fs::write(f.work.join("staged"), "staged").unwrap();
    command(&f.work, &["add", "staged"]);
    fs::write(f.work.join(".git/info/exclude"), "ignored\n").unwrap();
    fs::write(f.work.join("ignored"), "ignored").unwrap();
    let index = fs::read(f.work.join(".git/index")).unwrap();
    let mut p = prepare(&f).unwrap();
    assert_eq!(p.job.ahead, 1);
    assert_eq!(p.job.dirty.staged, 1);
    execute(&f, &mut p).unwrap();
    assert_eq!(p.job.state, "published_verified");
    assert_eq!(command(&f.remote, &["rev-parse", "main"]), p.job.head);
    assert_eq!(command(&f.remote, &["rev-parse", "unrelated"]), old);
    assert!(command(&f.remote, &["tag", "--list"]).is_empty());
    assert_eq!(fs::read(f.work.join(".git/index")).unwrap(), index);
    for (name, value) in [
        ("initial", "dirty"),
        ("untracked", "untracked"),
        ("staged", "staged"),
        ("ignored", "ignored"),
    ] {
        assert_eq!(fs::read_to_string(f.work.join(name)).unwrap(), value);
    }
    assert_eq!(
        f.db.sync_record(f.id).unwrap().assessment.unwrap().relation,
        "up_to_date"
    );
    let db = Registry::open(&f.root.join("registry.sqlite")).unwrap();
    assert_eq!(
        db.push_history(f.id).unwrap()[0].state,
        "published_verified"
    );
}
#[test]
fn synchronized_is_verified_noop() {
    let f = Fixture::new();
    let mut p = prepare(&f).unwrap();
    assert_eq!(p.job.ahead, 0);
    execute(&f, &mut p).unwrap();
    assert_eq!(p.job.state, "already_synchronized");
}
#[test]
fn blockers_prevent_publication() {
    for mode in [
        "behind",
        "diverged",
        "detached",
        "missing",
        "ambiguous",
        "deleted",
        "pushurl",
        "mirror",
        "refspec",
        "hook",
        "lock",
    ] {
        let f = Fixture::new();
        let before = command(&f.remote, &["rev-parse", "main"]);
        match mode {
            "behind" => f.advance(),
            "diverged" => {
                f.advance();
                commit(&f.work, "local");
            }
            "detached" => {
                command(&f.work, &["checkout", "--detach"]);
            }
            "missing" => {
                command(&f.work, &["branch", "--unset-upstream"]);
            }
            "ambiguous" => {
                command(
                    &f.work,
                    &["config", "--add", "branch.main.merge", "refs/heads/other"],
                );
            }
            "deleted" => {
                command(&f.remote, &["update-ref", "-d", "refs/heads/main"]);
            }
            "pushurl" => {
                command(
                    &f.work,
                    &[
                        "config",
                        "remote.origin.pushurl",
                        f.remote.to_str().unwrap(),
                    ],
                );
            }
            "mirror" => {
                command(&f.work, &["config", "remote.origin.mirror", "true"]);
            }
            "refspec" => {
                command(
                    &f.work,
                    &["config", "remote.origin.push", "refs/heads/*:refs/heads/*"],
                );
            }
            "hook" => fs::write(f.work.join(".git/hooks/pre-push"), "exit 0").unwrap(),
            _ => fs::write(f.work.join(".git/index.lock"), "lock").unwrap(),
        }
        assert!(prepare(&f).is_err(), "{mode}");
        if !["behind", "diverged", "deleted"].contains(&mode) {
            assert_eq!(command(&f.remote, &["rev-parse", "main"]), before);
        }
    }
}
#[test]
fn stale_head_destination_identity_and_remote_are_rejected() {
    for mode in ["head", "destination", "identity", "remote"] {
        let f = Fixture::new();
        commit(&f.work, "local");
        let mut p = prepare(&f).unwrap();
        match mode {
            "head" => commit(&f.work, "second"),
            "destination" => {
                command(
                    &f.work,
                    &["config", "branch.main.merge", "refs/heads/other"],
                );
            }
            "identity" => {
                fs::rename(&f.work, f.root.join("moved")).unwrap();
                fs::create_dir(&f.work).unwrap();
            }
            _ => f.advance(),
        }
        assert!(execute(&f, &mut p).is_err(), "{mode}");
        assert_ne!(command(&f.remote, &["rev-parse", "main"]), p.job.head);
    }
}
#[test]
fn exclusive_lock_and_queue() {
    let f = Fixture::new();
    let _p = prepare(&f).unwrap();
    assert_eq!(prepare(&f).err().unwrap().code, ErrorCode::Busy);
    let q = crate::operations::Operations::default();
    q.start("push").unwrap();
    assert_eq!(q.start("pull").unwrap_err().code, ErrorCode::Busy);
}
#[test]
fn cancellation_before_dispatch_and_uncertain_after_dispatch() {
    for phase in ["Push ready", "Publishing one"] {
        let f = Fixture::new();
        commit(&f.work, "local");
        let mut p = prepare(&f).unwrap();
        let cancel = AtomicBool::new(false);
        let e = p
            .execute(&f.db, &Git::default(), &cancel, &mut |m, _| {
                if m.starts_with(phase) {
                    cancel.store(true, Ordering::Relaxed);
                }
            })
            .unwrap_err();
        assert_eq!(
            p.job.state,
            if phase == "Push ready" {
                "cancelled"
            } else {
                "uncertain"
            }
        );
        assert_eq!(
            e.code,
            if phase == "Push ready" {
                ErrorCode::Cancelled
            } else {
                ErrorCode::RecoveryRequired
            }
        );
        assert_ne!(command(&f.remote, &["rev-parse", "main"]), p.job.head);
    }
}
#[test]
fn race_remote_rejection_and_verification_failure_are_distinct() {
    for reject in [true, false] {
        let f = Fixture::new();
        commit(&f.work, "local");
        let mut p = prepare(&f).unwrap();
        assert!(p
            .execute(
                &f.db,
                &Git::default(),
                &AtomicBool::new(false),
                &mut |m, _| {
                    if reject && m.starts_with("Publishing one") {
                        f.advance();
                    }
                    if !reject && m.starts_with("Independently checking") {
                        fs::rename(&f.remote, f.root.join("unavailable.git")).unwrap();
                    }
                }
            )
            .is_err());
        assert_eq!(p.job.state, if reject { "rejected" } else { "uncertain" });
        if !reject {
            assert_eq!(
                command(&f.root.join("unavailable.git"), &["rev-parse", "main"]),
                p.job.head
            );
        }
    }
}
#[test]
fn interrupted_journal_is_reconciled_without_remote_retry() {
    let f = Fixture::new();
    commit(&f.work, "local");
    let p = prepare(&f).unwrap();
    let old = command(&f.remote, &["rev-parse", "main"]);
    drop(p);
    let db = Registry::open(&f.root.join("registry.sqlite")).unwrap();
    db.reconcile_pushes().unwrap();
    assert_eq!(db.push_history(f.id).unwrap()[0].state, "uncertain");
    assert_eq!(command(&f.remote, &["rev-parse", "main"]), old);
}
#[test]
fn post_dispatch_database_failure_is_uncertain() {
    let f = Fixture::new();
    commit(&f.work, "local");
    let mut p = prepare(&f).unwrap();
    let c = rusqlite::Connection::open(f.root.join("registry.sqlite")).unwrap();
    c.execute_batch("CREATE TRIGGER reject_success BEFORE UPDATE ON push_jobs WHEN json_extract(NEW.record_json,'$.state')='published_verified' BEGIN SELECT RAISE(FAIL,'fixture'); END;").unwrap();
    assert!(execute(&f, &mut p).is_err());
    assert_eq!(p.job.state, "uncertain");
    assert_eq!(command(&f.remote, &["rev-parse", "main"]), p.job.head);
}
