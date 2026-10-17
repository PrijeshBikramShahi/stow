use super::*;
use crate::changes::{self, PreparedCommit, Snapshot};
fn fixture() -> Fixture {
    let f = Fixture::new();
    command(&f.work, &["config", "user.name", "Fixture"]);
    command(
        &f.work,
        &["config", "user.email", "fixture@example.invalid"],
    );
    f
}
fn review(f: &Fixture) -> Snapshot {
    changes::review(&f.db, &Git::default(), f.id, &AtomicBool::new(false)).unwrap()
}
fn stage(f: &Fixture, s: &Snapshot, paths: &[&str], unstage: bool) -> Result<()> {
    changes::mutate_index(
        &f.db,
        &Git::default(),
        s,
        &paths.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
        unstage,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
}
fn prepare(f: &Fixture, s: &Snapshot, message: &str) -> Result<PreparedCommit> {
    changes::prepare_commit(
        &f.db,
        &Git::default(),
        s,
        message.into(),
        &AtomicBool::new(false),
    )
}
fn execute(f: &Fixture, p: &mut PreparedCommit) -> Result<()> {
    p.execute(
        &f.db,
        &Git::default(),
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
}
#[test]
fn selective_modified_new_deleted_staging_and_index_only_unstaging() {
    let f = fixture();
    commit(&f.work, "delete-me");
    fs::write(f.work.join("initial"), "edited").unwrap();
    fs::write(f.work.join("new"), "new").unwrap();
    fs::write(f.work.join("unselected"), "untouched").unwrap();
    fs::remove_file(f.work.join("delete-me")).unwrap();
    let s = review(&f);
    assert_eq!(s.view.entries.len(), 4);
    stage(&f, &s, &["initial", "new", "delete-me"], false).unwrap();
    let s = review(&f);
    assert_eq!(
        s.view.entries.iter().filter(|e| e.staged.is_some()).count(),
        3
    );
    assert!(
        s.view
            .entries
            .iter()
            .find(|e| e.path == "unselected")
            .unwrap()
            .untracked
    );
    assert_eq!(command(&f.work, &["show", ":initial"]), "edited");
    assert_eq!(command(&f.work, &["show", ":new"]), "new");
    stage(&f, &s, &["initial", "new", "delete-me"], true).unwrap();
    assert!(command(&f.work, &["diff", "--cached", "--name-only"]).is_empty());
    assert_eq!(
        fs::read_to_string(f.work.join("initial")).unwrap(),
        "edited"
    );
    assert_eq!(fs::read_to_string(f.work.join("new")).unwrap(), "new");
    assert!(!f.work.join("delete-me").exists());
    assert_eq!(f.db.change_history(f.id).unwrap().len(), 2);
}
#[test]
fn literal_spaces_unicode_tabs_newlines_and_pathspec_metacharacters() {
    let f = fixture();
    let paths = [
        "with spaces",
        "नमस्ते.txt",
        "tab\tname",
        "line\nname",
        "-leading",
        ":(glob)*.txt",
    ];
    for p in paths {
        fs::write(f.work.join(p), p).unwrap();
    }
    let s = review(&f);
    assert_eq!(s.view.entries.len(), paths.len());
    stage(&f, &s, &paths, false).unwrap();
    let s = review(&f);
    assert_eq!(
        s.view.entries.iter().filter(|e| e.staged.is_some()).count(),
        paths.len()
    );
    let mut p = prepare(&f, &s, "Unusual filenames").unwrap();
    execute(&f, &mut p).unwrap();
    drop(p);
    assert!(review(&f).view.entries.is_empty());
    for p in paths {
        assert!(fs::read(f.work.join(p)).is_ok());
    }
}
#[test]
fn rename_and_binary_reviews_and_separate_diffs() {
    let f = fixture();
    command(&f.work, &["mv", "initial", "renamed"]);
    fs::write(f.work.join("binary"), [0, 1, 2, 3]).unwrap();
    let s = review(&f);
    let r = s.view.entries.iter().find(|e| e.path == "renamed").unwrap();
    assert_eq!(r.original.as_deref(), Some("initial"));
    assert_eq!(r.staged.as_deref(), Some("renamed"));
    assert!(
        s.view
            .entries
            .iter()
            .find(|e| e.path == "binary")
            .unwrap()
            .binary
    );
    assert!(
        changes::diff(
            &f.db,
            &Git::default(),
            &s,
            "binary",
            false,
            &AtomicBool::new(false)
        )
        .unwrap()
        .binary
    );
    stage(&f, &s, &["renamed"], true).unwrap();
    assert!(!f.work.join("initial").exists());
    assert_eq!(
        fs::read_to_string(f.work.join("renamed")).unwrap(),
        "initial"
    );
    let s = review(&f);
    stage(&f, &s, &["renamed", "initial", "binary"], false).unwrap();
    fs::write(f.work.join("renamed"), "unstaged contents").unwrap();
    let s = review(&f);
    let staged = changes::diff(
        &f.db,
        &Git::default(),
        &s,
        "renamed",
        true,
        &AtomicBool::new(false),
    )
    .unwrap();
    let working = changes::diff(
        &f.db,
        &Git::default(),
        &s,
        "renamed",
        false,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(!staged.text.contains("unstaged contents"));
    assert!(working.text.contains("unstaged contents"));
}
#[test]
fn commit_only_confirmed_index_keeps_unstaged_and_refreshes_ahead() {
    let f = fixture();
    fs::write(f.work.join("initial"), "staged content").unwrap();
    let s = review(&f);
    stage(&f, &s, &["initial"], false).unwrap();
    fs::write(f.work.join("initial"), "unstaged content").unwrap();
    fs::write(f.work.join("untracked"), "untracked").unwrap();
    let s = review(&f);
    let index = fs::read(f.work.join(".git/index")).unwrap();
    let before = command(&f.work, &["rev-parse", "HEAD"]);
    let mut p = prepare(&f, &s, "Commit only staged\n\nExact message.").unwrap();
    assert_eq!(p.preview().author.email, "fixture@example.invalid");
    execute(&f, &mut p).unwrap();
    assert_eq!(p.job.state, "completed");
    assert_eq!(
        command(&f.work, &["show", "HEAD:initial"]),
        "staged content"
    );
    assert_eq!(
        fs::read_to_string(f.work.join("initial")).unwrap(),
        "unstaged content"
    );
    assert_eq!(fs::read(f.work.join(".git/index")).unwrap(), index);
    assert!(f.work.join("untracked").exists());
    assert_eq!(command(&f.work, &["rev-parse", "HEAD^"]), before);
    assert_eq!(
        command(&f.work, &["log", "-1", "--format=%ae"]),
        "fixture@example.invalid"
    );
    let a = f.db.sync_record(f.id).unwrap().assessment.unwrap();
    assert_eq!(a.relation, "ahead");
    assert_eq!(a.ahead, Some(1));
    assert_eq!(a.inspection.working_tree.unstaged, 1);
    assert_ne!(
        command(&f.work, &["rev-parse", "HEAD"]),
        command(&f.remote, &["rev-parse", "main"])
    );
    assert!(!serde_json::to_string(&f.db.change_history(f.id).unwrap())
        .unwrap()
        .contains("Exact message"));
    drop(p);
    let mut push = crate::push::prepare(
        &f.db,
        &Git::default(),
        f.id,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .unwrap();
    push.execute(
        &f.db,
        &Git::default(),
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .unwrap();
    assert_eq!(push.job.state, "published_verified");
}
#[test]
fn empty_message_empty_index_missing_author_and_hook_signing_block() {
    let f = fixture();
    let s = review(&f);
    assert!(prepare(&f, &s, "message").is_err());
    fs::write(f.work.join("initial"), "changed").unwrap();
    let s = review(&f);
    stage(&f, &s, &["initial"], false).unwrap();
    let s = review(&f);
    for message in ["", "  \n\t", "nul\0byte"] {
        assert!(prepare(&f, &s, message).is_err());
    }
    command(&f.work, &["config", "user.name", ""]);
    let s = review(&f);
    assert!(s.view.author.is_none());
    assert!(prepare(&f, &s, "message")
        .unwrap_err_message()
        .contains("user.name"));
    command(&f.work, &["config", "user.name", "Fixture"]);
    fs::write(
        f.work.join(".git/hooks/pre-commit"),
        "#!/bin/sh\ntouch should-not-exist\n",
    )
    .unwrap();
    let s = review(&f);
    assert!(prepare(&f, &s, "message").is_err());
    assert!(!f.work.join("should-not-exist").exists());
    let f = fixture();
    command(&f.work, &["config", "commit.gpgSign", "true"]);
    assert!(changes::review(&f.db, &Git::default(), f.id, &AtomicBool::new(false)).is_err());
}
// Avoid requiring Debug on capability-bearing PreparedCommit just for assertions.
trait ErrorMessage {
    fn unwrap_err_message(self) -> String;
}
impl<T> ErrorMessage for Result<T> {
    fn unwrap_err_message(self) -> String {
        match self {
            Err(e) => e.message,
            Ok(_) => panic!("expected blocker"),
        }
    }
}
#[test]
fn stale_files_index_head_and_paths_never_mutate_original_index() {
    for mode in [
        "file",
        "index",
        "head",
        "traversal",
        "directory",
        "unselected",
    ] {
        let f = fixture();
        fs::write(f.work.join("initial"), "reviewed").unwrap();
        let s = review(&f);
        let path = match mode {
            "file" => {
                fs::write(f.work.join("initial"), "changed after review").unwrap();
                "initial"
            }
            "index" => {
                command(&f.work, &["add", "initial"]);
                "initial"
            }
            "head" => {
                commit(&f.work, "head-change");
                "initial"
            }
            "traversal" => "../outside",
            "directory" => ".",
            _ => "other",
        };
        let before = fs::read(f.work.join(".git/index")).unwrap();
        assert!(stage(&f, &s, &[path], false).is_err(), "{mode}");
        assert_eq!(fs::read(f.work.join(".git/index")).unwrap(), before);
    }
}
#[test]
fn changes_during_private_staging_abort_installation() {
    let f = fixture();
    fs::write(f.work.join("initial"), "reviewed").unwrap();
    let s = review(&f);
    let index = fs::read(f.work.join(".git/index")).unwrap();
    assert!(changes::mutate_index(
        &f.db,
        &Git::default(),
        &s,
        &["initial".into()],
        false,
        &AtomicBool::new(false),
        &mut |m, _| {
            if m.starts_with("Revalidating selected") {
                fs::write(f.work.join("initial"), "external edit").unwrap();
            }
        }
    )
    .is_err());
    assert_eq!(fs::read(f.work.join(".git/index")).unwrap(), index);
    assert_eq!(
        fs::read_to_string(f.work.join("initial")).unwrap(),
        "external edit"
    );
    assert!(!f.work.join(".git/index.lock").exists());
}
#[test]
fn stale_commit_approval_rejects_index_head_author_and_native_locks() {
    for mode in ["index", "head", "author", "lock", "identity"] {
        let f = fixture();
        fs::write(f.work.join("initial"), "staged").unwrap();
        let s = review(&f);
        stage(&f, &s, &["initial"], false).unwrap();
        let s = review(&f);
        let mut p = prepare(&f, &s, "reviewed message").unwrap();
        match mode {
            "index" => {
                fs::write(f.work.join("initial"), "different").unwrap();
                command(&f.work, &["add", "initial"]);
            }
            "head" => commit(&f.work, "other"),
            "author" => {
                command(&f.work, &["config", "user.email", "other@example.invalid"]);
            }
            "lock" => fs::write(f.work.join(".git/index.lock"), "external lock").unwrap(),
            _ => {
                fs::rename(&f.work, f.root.join("moved")).unwrap();
                fs::create_dir(&f.work).unwrap();
            }
        }
        assert!(execute(&f, &mut p).is_err(), "{mode}");
        assert_ne!(p.job.state, "completed");
        if mode == "lock" {
            assert_eq!(
                fs::read_to_string(f.work.join(".git/index.lock")).unwrap(),
                "external lock"
            );
        }
    }
}
#[test]
fn cancellation_before_branch_update_and_deferral_after_dispatch() {
    for phase in ["Commit object ready", "Updating branch atomically"] {
        let f = fixture();
        fs::write(f.work.join("initial"), "staged").unwrap();
        let s = review(&f);
        stage(&f, &s, &["initial"], false).unwrap();
        let s = review(&f);
        let mut p = prepare(&f, &s, "reviewed message").unwrap();
        let cancel = AtomicBool::new(false);
        let result = p.execute(&f.db, &Git::default(), &cancel, &mut |m, _| {
            if m.starts_with(phase) {
                cancel.store(true, Ordering::Relaxed);
            }
        });
        if phase == "Commit object ready" {
            assert_eq!(result.unwrap_err().code, ErrorCode::Cancelled);
            assert_eq!(p.job.state, "cancelled");
            assert_eq!(command(&f.work, &["rev-parse", "HEAD"]), p.job.before_head);
        } else {
            result.unwrap();
            assert_eq!(p.job.state, "completed");
        }
    }
}
#[test]
fn branch_compare_and_swap_guards_concurrent_head_switch() {
    let f = fixture();
    command(&f.work, &["branch", "other"]);
    fs::write(f.work.join("initial"), "staged").unwrap();
    let s = review(&f);
    stage(&f, &s, &["initial"], false).unwrap();
    let s = review(&f);
    let mut p = prepare(&f, &s, "message").unwrap();
    assert!(p
        .execute(
            &f.db,
            &Git::default(),
            &AtomicBool::new(false),
            &mut |m, _| {
                if m.starts_with("Updating branch atomically") {
                    // Simulate a writer ignoring Git's native HEAD reservation.
                    fs::write(f.work.join(".git/HEAD"), "ref: refs/heads/other\n").unwrap();
                }
            }
        )
        .is_err());
    assert_eq!(command(&f.work, &["rev-parse", "main"]), p.job.before_head);
    assert_eq!(command(&f.work, &["rev-parse", "other"]), p.job.before_head);
    assert_eq!(p.job.state, "requires_inspection");
}
#[test]
fn journal_failure_and_restart_never_retry_commit() {
    let f = fixture();
    fs::write(f.work.join("initial"), "staged").unwrap();
    let s = review(&f);
    stage(&f, &s, &["initial"], false).unwrap();
    let s = review(&f);
    let mut p = prepare(&f, &s, "private message").unwrap();
    let c = rusqlite::Connection::open(f.root.join("registry.sqlite")).unwrap();
    c.execute_batch("CREATE TRIGGER reject_success BEFORE UPDATE ON change_jobs WHEN json_extract(NEW.record_json,'$.state')='completed' BEGIN SELECT RAISE(FAIL,'fixture'); END;").unwrap();
    assert_eq!(
        execute(&f, &mut p).unwrap_err().code,
        ErrorCode::RecoveryRequired
    );
    let head = command(&f.work, &["rev-parse", "HEAD"]);
    assert_ne!(head, p.job.before_head);
    assert_eq!(p.job.state, "requires_inspection");
    assert_eq!(
        f.db.change_history(f.id).unwrap()[0].state,
        "requires_inspection"
    );
    drop(p);
    let db = Registry::open(&f.root.join("registry.sqlite")).unwrap();
    db.reconcile_changes().unwrap();
    assert_eq!(
        db.change_history(f.id).unwrap()[0].state,
        "requires_inspection"
    );
    assert_eq!(command(&f.work, &["rev-parse", "HEAD"]), head);
    let s = review(&f);
    assert!(s.view.entries.is_empty());
    assert!(prepare(&f, &s, "retry").is_err());
}
#[test]
fn path_symlinks_attributes_hidden_index_and_ignored_candidates() {
    for mode in ["symlink", "hardlink", "attributes", "skip"] {
        let f = fixture();
        match mode {
            "symlink" => symlink(f.root.join("source/initial"), f.work.join("unsafe")).unwrap(),
            "hardlink" => {
                fs::hard_link(f.root.join("source/initial"), f.work.join("unsafe")).unwrap()
            }
            "attributes" => fs::write(f.work.join(".gitattributes"), "* filter=evil").unwrap(),
            _ => {
                command(&f.work, &["update-index", "--skip-worktree", "initial"]);
            }
        }
        assert!(
            changes::review(&f.db, &Git::default(), f.id, &AtomicBool::new(false)).is_err(),
            "{mode}"
        );
    }
    let f = fixture();
    fs::write(f.work.join(".git/info/exclude"), "ignored\n").unwrap();
    fs::write(f.work.join("ignored"), "secret").unwrap();
    assert!(review(&f).view.entries.is_empty());
}
#[test]
fn oversized_diff_is_actionable_and_concurrent_operations_are_blocked() {
    let f = fixture();
    fs::write(f.work.join("large"), "x".repeat(600 * 1024)).unwrap();
    let s = review(&f);
    assert_eq!(
        changes::diff(
            &f.db,
            &Git::default(),
            &s,
            "large",
            false,
            &AtomicBool::new(false)
        )
        .err()
        .unwrap()
        .code,
        ErrorCode::LimitReached
    );
    let w = Directory::open(&f.work).unwrap();
    w.lock().unwrap();
    assert_eq!(
        stage(&f, &s, &["large"], false).unwrap_err().code,
        ErrorCode::Busy
    );
    let q = crate::operations::Operations::default();
    q.start("commit").unwrap();
    assert_eq!(q.start("stage").unwrap_err().code, ErrorCode::Busy);
}

#[test]
fn commit_without_remote_reserves_native_head_and_index_locks() {
    let f = fixture();
    command(&f.work, &["remote", "remove", "origin"]);
    command(&f.work, &["branch", "other"]);
    fs::write(f.work.join("initial"), "local only").unwrap();
    let s = review(&f);
    stage(&f, &s, &["initial"], false).unwrap();
    let s = review(&f);
    let mut p = prepare(&f, &s, "Local-only commit").unwrap();
    p.execute(
        &f.db,
        &Git::default(),
        &AtomicBool::new(false),
        &mut |message, _| {
            if message.starts_with("Updating branch atomically") {
                assert!(f.work.join(".git/index.lock").exists());
                assert!(f.work.join(".git/HEAD.lock").exists());
                let attempted = Command::new(Git::default().executable())
                    .env_clear()
                    .env("PATH", "/usr/bin:/bin")
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .env("GIT_CONFIG_SYSTEM", "/dev/null")
                    .current_dir(&f.work)
                    .args([
                        "-c",
                        "core.hooksPath=/dev/null",
                        "symbolic-ref",
                        "HEAD",
                        "refs/heads/other",
                    ])
                    .output()
                    .unwrap();
                assert!(!attempted.status.success());
            }
        },
    )
    .unwrap();
    assert_eq!(p.job.state, "completed");
    assert_eq!(
        command(&f.work, &["symbolic-ref", "HEAD"]),
        "refs/heads/main"
    );
    assert_eq!(
        f.db.sync_record(f.id).unwrap().assessment.unwrap().relation,
        "no_upstream"
    );
    assert!(!f.work.join(".git/index.lock").exists());
    assert!(!f.work.join(".git/HEAD.lock").exists());
}
