use super::*;
use crate::pull::{self, PreparedPull};
fn prepare(f: &Fixture) -> Result<PreparedPull> {
    let a = f.preflight();
    pull::prepare(
        &f.db,
        &Git::default(),
        f.id,
        &a.token,
        &AtomicBool::new(false),
    )
}
fn execute(f: &Fixture, p: &mut PreparedPull) -> Result<()> {
    p.execute(
        &f.db,
        &Git::default(),
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
}
#[test]
fn fast_forward_fetches_new_target_and_verifies_files_index_refs_and_persistence() {
    let f = Fixture::new();
    let old = command(&f.work, &["rev-parse", "HEAD"]);
    command(&f.work, &["branch", "unrelated"]);
    let mut p = prepare(&f).unwrap();
    f.advance(); // Remote advance after confirmation is intentionally fetched/verified.
    execute(&f, &mut p).unwrap();
    let target = command(&f.remote, &["rev-parse", "main"]);
    assert_eq!(command(&f.work, &["rev-parse", "HEAD"]), target);
    assert_eq!(command(&f.work, &["rev-parse", "unrelated"]), old);
    assert!(command(&f.work, &["status", "--porcelain"]).is_empty());
    assert_eq!(
        command(&f.work, &["write-tree"]),
        command(&f.work, &["rev-parse", "HEAD^{tree}"])
    );
    assert_eq!(
        fs::read_to_string(f.work.join("remote-change")).unwrap(),
        "remote-change"
    );
    assert_eq!(
        f.db.sync_record(f.id).unwrap().assessment.unwrap().relation,
        "up_to_date"
    );
    assert_eq!(
        f.db.get(f.id).unwrap().inspection.unwrap().commit,
        Some(target.clone())
    );
    let reopened = Registry::open(&f.root.join("registry.sqlite")).unwrap();
    let job = reopened.pull_history(f.id).unwrap().remove(0);
    assert_eq!(job.state, "completed");
    assert_eq!(job.after_head, Some(target));
}
#[test]
fn up_to_date_is_successful_noop_without_index_or_worktree_write() {
    let f = Fixture::new();
    let index = fs::read(f.work.join(".git/index")).unwrap();
    let file = fs::metadata(f.work.join("initial"))
        .unwrap()
        .modified()
        .unwrap();
    let mut p = prepare(&f).unwrap();
    execute(&f, &mut p).unwrap();
    assert_eq!(fs::read(f.work.join(".git/index")).unwrap(), index);
    assert_eq!(
        fs::metadata(f.work.join("initial"))
            .unwrap()
            .modified()
            .unwrap(),
        file
    );
    assert_eq!(p.job.before_head, p.job.after_head.unwrap());
    assert_eq!(f.db.fetch_history(f.id).unwrap()[0].state, "completed");
}
#[test]
fn dirty_staged_untracked_and_ignored_files_are_preserved() {
    for mode in ["dirty", "staged", "untracked", "ignored"] {
        let f = Fixture::new();
        match mode {
            "dirty" => fs::write(f.work.join("initial"), "changed").unwrap(),
            "staged" => {
                fs::write(f.work.join("initial"), "staged").unwrap();
                command(&f.work, &["add", "initial"]);
            }
            "untracked" => fs::write(f.work.join("new-file"), "local").unwrap(),
            _ => {
                fs::write(f.work.join(".git/info/exclude"), "ignored\n").unwrap();
                fs::write(f.work.join("ignored"), "local").unwrap();
            }
        }
        let index = fs::read(f.work.join(".git/index")).unwrap();
        assert!(prepare(&f).is_err(), "{mode}");
        assert_eq!(fs::read(f.work.join(".git/index")).unwrap(), index);
    }
}
#[test]
fn ahead_diverged_detached_missing_ambiguous_and_locks_block() {
    for mode in [
        "ahead",
        "diverged",
        "detached",
        "missing",
        "ambiguous",
        "lock",
    ] {
        let f = Fixture::new();
        match mode {
            "ahead" => commit(&f.work, "local"),
            "diverged" => {
                commit(&f.work, "local");
                f.advance();
                f.fetch().unwrap();
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
            _ => fs::write(f.work.join(".git/index.lock"), "lock").unwrap(),
        }
        assert!(prepare(&f).is_err(), "{mode}");
    }
}
#[test]
fn exclusive_lock_and_operation_queue_reject_concurrent_work() {
    let f = Fixture::new();
    let _p = prepare(&f).unwrap();
    assert_eq!(prepare(&f).err().unwrap().code, ErrorCode::Busy);
    let operations = crate::operations::Operations::default();
    operations.start("pull").unwrap();
    assert_eq!(operations.start("fetch").unwrap_err().code, ErrorCode::Busy);
}
#[test]
fn source_changes_after_confirmation_abort_before_fetch() {
    for mode in [
        "head",
        "config",
        "index",
        "dirty",
        "tracking",
        "substitution",
    ] {
        let f = Fixture::new();
        let mut p = prepare(&f).unwrap();
        match mode {
            "head" => commit(&f.work, "local"),
            "config" => {
                command(&f.work, &["config", "user.name", "Changed"]);
            }
            "index" => {
                command(&f.work, &["update-index", "--assume-unchanged", "initial"]);
            }
            "dirty" => fs::write(f.work.join("initial"), "changed").unwrap(),
            "tracking" => {
                command(
                    &f.work,
                    &[
                        "update-ref",
                        "refs/remotes/origin/other",
                        &p.job.before_head,
                    ],
                );
            }
            _ => {
                fs::rename(&f.work, f.root.join("moved")).unwrap();
                fs::create_dir(&f.work).unwrap();
            }
        }
        assert!(execute(&f, &mut p).is_err(), "{mode}");
        assert!(f.db.fetch_history(f.id).unwrap().is_empty());
    }
}
#[test]
fn changes_after_fetch_and_immediately_before_integration_are_rechecked() {
    for phase in [
        "Fetch finished",
        "Ready to integrate",
        "Updating working tree",
    ] {
        let f = Fixture::new();
        let mut p = prepare(&f).unwrap();
        f.advance();
        let old = p.job.before_head.clone();
        let result = p.execute(
            &f.db,
            &Git::default(),
            &AtomicBool::new(false),
            &mut |message, _| {
                if message.starts_with(phase) {
                    fs::write(f.work.join("initial"), "external edit").unwrap();
                }
            },
        );
        assert!(result.is_err());
        assert_eq!(command(&f.work, &["rev-parse", "HEAD"]), old);
        assert_eq!(
            fs::read_to_string(f.work.join("initial")).unwrap(),
            "external edit"
        );
    }
}
#[test]
fn cancellation_boundaries_and_critical_section() {
    for phase in [
        "before",
        "Fetching selected",
        "Fetch finished",
        "Ready to integrate",
        "Updating working tree",
    ] {
        let f = Fixture::new();
        let mut p = prepare(&f).unwrap();
        f.advance();
        let cancel = AtomicBool::new(phase == "before");
        let result = p.execute(&f.db, &Git::default(), &cancel, &mut |message, _| {
            if message.starts_with(phase) {
                cancel.store(true, Ordering::Relaxed);
            }
        });
        if phase == "Updating working tree" {
            result.unwrap();
            assert_eq!(p.job.state, "completed");
            assert_eq!(
                command(&f.work, &["rev-parse", "HEAD"]),
                command(&f.remote, &["rev-parse", "main"])
            );
        } else {
            assert_eq!(result.unwrap_err().code, ErrorCode::Cancelled);
            assert_eq!(p.job.state, "cancelled");
            assert_eq!(command(&f.work, &["rev-parse", "HEAD"]), p.job.before_head);
        }
    }
}
#[test]
fn remote_unavailable_or_deleted_upstream_never_integrates_cached_target() {
    for deleted in [false, true] {
        let f = Fixture::new();
        f.advance();
        f.fetch().unwrap();
        let mut p = prepare(&f).unwrap();
        if deleted {
            command(&f.remote, &["update-ref", "-d", "refs/heads/main"]);
        } else {
            fs::rename(&f.remote, f.root.join("unavailable")).unwrap();
        }
        assert!(execute(&f, &mut p).is_err());
        assert_eq!(command(&f.work, &["rev-parse", "HEAD"]), p.job.before_head);
    }
}
#[test]
fn hooks_filters_attributes_symlinks_and_hidden_index_changes_block() {
    for mode in [
        "hook",
        "hook_config",
        "filter",
        "attributes",
        "sparse",
        "assume",
        "skip",
        "symlink",
    ] {
        let f = Fixture::new();
        match mode {
            "hook" => {
                fs::write(
                    f.work.join(".git/hooks/post-merge"),
                    "#!/bin/sh\ntouch should-not-run\n",
                )
                .unwrap();
            }
            "hook_config" => {
                command(&f.work, &["config", "core.hooksPath", "/tmp/unsafe-hooks"]);
            }
            "filter" => {
                command(
                    &f.work,
                    &["config", "filter.evil.clean", "touch should-not-run"],
                );
            }
            "attributes" => {
                fs::write(f.work.join(".git/info/attributes"), "* filter=evil").unwrap()
            }
            "sparse" => fs::write(f.work.join(".git/info/sparse-checkout"), "initial").unwrap(),
            "assume" => {
                command(&f.work, &["update-index", "--assume-unchanged", "initial"]);
                fs::write(f.work.join("initial"), "hidden").unwrap();
            }
            "skip" => {
                command(&f.work, &["update-index", "--skip-worktree", "initial"]);
            }
            _ => symlink("initial", f.work.join("link")).unwrap(),
        }
        let a = assess(&Git::default(), &f.work, &AtomicBool::new(false));
        if a.is_ok() {
            assert!(prepare(&f).is_err(), "{mode}");
        }
        assert!(!f.work.join("should-not-run").exists());
    }
    let f = Fixture::new();
    let mut p = prepare(&f).unwrap();
    commit(&f.source, ".gitattributes");
    command(&f.source, &["push", f.remote.to_str().unwrap(), "main"]);
    assert!(execute(&f, &mut p).is_err());
    assert!(!f.work.join(".gitattributes").exists());
}
#[test]
fn restart_classifies_all_unfinished_phases_without_reset() {
    let f = Fixture::new();
    let mut p = prepare(&f).unwrap();
    for phase in [
        "awaiting_confirmation",
        "fetching",
        "integrating",
        "verifying",
    ] {
        p.job.state = "running".into();
        p.job.phase = phase.into();
        f.db.save_pull(&p.job).unwrap();
        let reopened = Registry::open(&f.root.join("registry.sqlite")).unwrap();
        reopened.reconcile_pulls().unwrap();
        let job = reopened.pull_history(f.id).unwrap().remove(0);
        assert_eq!(job.state, "requires_inspection");
        assert_eq!(job.phase, phase);
        assert_eq!(command(&f.work, &["rev-parse", "HEAD"]), job.before_head);
    }
}
#[test]
fn persistence_failure_after_integration_requires_inspection() {
    let f = Fixture::new();
    let mut p = prepare(&f).unwrap();
    f.advance();
    let conn = rusqlite::Connection::open(f.root.join("registry.sqlite")).unwrap();
    conn.execute_batch("CREATE TRIGGER reject_pull_success BEFORE UPDATE ON pull_jobs WHEN json_extract(NEW.record_json,'$.state')='completed' BEGIN SELECT RAISE(FAIL,'fixture'); END;").unwrap();
    assert_eq!(
        execute(&f, &mut p).unwrap_err().code,
        ErrorCode::RecoveryRequired
    );
    assert_eq!(
        f.db.pull_history(f.id).unwrap()[0].state,
        "requires_inspection"
    );
    assert_eq!(
        command(&f.work, &["rev-parse", "HEAD"]),
        command(&f.remote, &["rev-parse", "main"])
    );
}

#[test]
fn cached_behind_fast_forward_updates_deletes_and_renames_tracked_files() {
    let f = Fixture::new();
    commit(&f.source, "remove-me");
    command(&f.source, &["push", f.remote.to_str().unwrap(), "main"]);
    let mut first = prepare(&f).unwrap();
    execute(&f, &mut first).unwrap();
    drop(first);
    fs::write(f.source.join("initial"), "updated contents").unwrap();
    command(&f.source, &["mv", "remove-me", "renamed"]);
    command(&f.source, &["add", "initial"]);
    command(
        &f.source,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "modify and rename",
        ],
    );
    command(&f.source, &["push", f.remote.to_str().unwrap(), "main"]);
    f.fetch().unwrap();
    assert_eq!(f.preflight().relation, "behind");
    let mut p = prepare(&f).unwrap();
    execute(&f, &mut p).unwrap();
    assert_eq!(
        fs::read_to_string(f.work.join("initial")).unwrap(),
        "updated contents"
    );
    assert!(!f.work.join("remove-me").exists());
    assert_eq!(
        fs::read_to_string(f.work.join("renamed")).unwrap(),
        "remove-me"
    );
    assert!(command(&f.work, &["status", "--porcelain"]).is_empty());
    assert_eq!(
        command(&f.work, &["write-tree"]),
        command(&f.work, &["rev-parse", "HEAD^{tree}"])
    );
}
