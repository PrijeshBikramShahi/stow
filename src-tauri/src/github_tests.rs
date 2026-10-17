use super::*;
use crate::{
    database::Registry,
    library::{match_projects, remote_identity},
};
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;

fn fixture(id: i64, owner: &str, name: &str) -> Value {
    json!({"id":id,"owner":{"login":owner},"name":name,"full_name":format!("{owner}/{name}"),"description":"A repository", "html_url":format!("https://github.com/{owner}/{name}"),"clone_url":format!("https://github.com/{owner}/{name}.git"),"ssh_url":format!("git@github.com:{owner}/{name}.git"),"visibility":"public","private":false,"fork":false,"archived":false,"default_branch":"main","pushed_at":"2026-10-15T00:00:00Z","updated_at":"2026-10-15T01:00:00Z"})
}
fn repository(id: i64, owner: &str, name: &str) -> Repository {
    serde_json::from_value::<ApiRepository>(fixture(id, owner, name))
        .unwrap()
        .validate()
        .unwrap()
}
fn auth_json(state: &str, error: &str) -> String {
    json!({"hosts":{"github.com":[{"host":"github.com","login":"alice","active":true,"state":state,"error":error}]}}).to_string()
}
fn parse_auth_fixture(value: Value) -> AuthStatus {
    parse_auth(value.to_string().as_bytes(), "gh version 2.102.0".into()).unwrap()
}
fn fake(directory: &std::path::Path, body: &str) -> Github {
    let executable = directory.join("fake-gh");
    std::fs::write(&executable, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    Github {
        executable,
        timeout: Duration::from_secs(10),
        output_limit: OUTPUT_LIMIT,
    }
}
fn provider_script(repo_body: &str) -> String {
    format!(
        r#"
case "$1" in
--version) echo 'gh version 2.102.0';;
auth) cat <<'AUTH'
{}
AUTH
;;
api)
 for last in "$@"; do :; done
 case "$last" in
 /user) echo '{{"id":7,"login":"alice"}}';;
 /user/repos\?*)
 case "$*" in *'--paginate --slurp'*'visibility=all&affiliation=owner,collaborator,organization_member&per_page=100'*) :;; *) exit 90;; esac
 {}
 ;;
 *) exit 91;;
 esac;;
*) exit 92;;
esac
"#,
        auth_json("success", ""),
        repo_body
    )
}
fn slurp(repos: &str) -> String {
    let pages = serde_json::Deserializer::from_str(repos)
        .into_iter::<Value>()
        .map(|p| p.unwrap())
        .collect::<Vec<_>>();
    serde_json::to_string(&pages).unwrap()
}
fn output_repos(repos: &str) -> String {
    format!("cat <<'REPOS'\n{}\nREPOS", slurp(repos))
}
fn snapshot(repositories: Vec<Repository>) -> Snapshot {
    Snapshot {
        account: Account {
            id: 7,
            login: "alice".into(),
        },
        repositories,
    }
}
fn project(remotes: &[&str]) -> crate::core::Project {
    crate::core::Project {
        id: 1,
        name: "test".into(),
        path: "/not-read".into(),
        registered_at: 0,
        inspected_at: Some(0),
        availability: "available".into(),
        error: None,
        inspection: Some(crate::core::Inspection {
            branch: None,
            detached: false,
            commit: None,
            upstream: None,
            working_tree: Default::default(),
            warnings: vec![],
            remotes: remotes
                .iter()
                .enumerate()
                .map(|(i, url)| crate::core::Remote {
                    name: format!("remote-{i}"),
                    url: (*url).into(),
                })
                .collect(),
        }),
    }
}
fn cancelled() -> AtomicBool {
    AtomicBool::new(false)
}

#[test]
fn auth_missing_available_and_recheck() {
    let dir = tempfile::tempdir().unwrap();
    let mut gh = fake(dir.path(), "echo 'gh version 2.102.0'");
    assert!(gh.version().unwrap().starts_with("gh version"));
    gh.executable = dir.path().join("missing");
    assert_eq!(gh.auth(&cancelled()).state, AuthState::CliUnavailable);
    let gh = fake(dir.path(), &provider_script(&output_repos("[]")));
    assert_eq!(gh.auth(&cancelled()).state, AuthState::Authenticated);
    fake(
        dir.path(),
        &format!(
            "if [ \"$1\" = '--version' ]; then echo 'gh version 2.102.0'; else echo '{}'; fi",
            auth_json("error", "HTTP 401: Bad credentials SENSITIVE")
        ),
    );
    let status = gh.auth(&cancelled());
    assert_eq!(status.state, AuthState::Invalid);
    assert!(!serde_json::to_string(&status)
        .unwrap()
        .contains("SENSITIVE"));
}
#[test]
fn auth_structured_states_and_unknown_shapes() {
    assert_eq!(
        parse_auth_fixture(json!({"hosts":{}})).state,
        AuthState::NotAuthenticated
    );
    for (state, error, expected) in [
        ("success", "", AuthState::Authenticated),
        ("error", "HTTP 401: Bad credentials", AuthState::Invalid),
        ("timeout", "", AuthState::NetworkUnavailable),
        (
            "error",
            "dial tcp: connection refused",
            AuthState::NetworkUnavailable,
        ),
        ("error", "HTTP 500", AuthState::Uncertain),
        ("success", "unexpected error", AuthState::Uncertain),
        ("future", "", AuthState::Uncertain),
    ] {
        assert_eq!(
            parse_auth(auth_json(state, error).as_bytes(), "gh version 2".into())
                .unwrap()
                .state,
            expected
        );
    }
    assert!(parse_auth(b"{}", "gh version 2".into()).is_err());
}
#[test]
fn auth_multiple_accounts_does_not_select_inactive_account() {
    let mut data: Value = serde_json::from_str(&auth_json("error", "HTTP 401")).unwrap();
    data["hosts"]["github.com"]
        .as_array_mut()
        .unwrap()
        .push(json!({"host":"github.com","login":"bob","active":false,"state":"success"}));
    let status = parse_auth_fixture(data.clone());
    assert_eq!(status.state, AuthState::Invalid);
    assert_eq!(status.login.as_deref(), Some("alice"));
    assert_eq!(status.known_accounts, 2);
    data["hosts"]["github.com"][1]["active"] = json!(true);
    assert_eq!(parse_auth_fixture(data).state, AuthState::Unsupported);
    assert_eq!(
        parse_auth_fixture(json!({"hosts":{"enterprise.example":[]}})).state,
        AuthState::Unsupported
    );
}
#[test]
fn failed_login_handoff_is_safe_and_structured() {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "echo 'SENSITIVE LOGIN' >&2; exit 1"]);
    let error = handoff(command).unwrap_err();
    assert_eq!(error.code, ErrorCode::LoginHandoff);
    assert!(!error.message.contains("SENSITIVE"));
    assert_eq!(
        handoff(Command::new("/nonexistent-terminal"))
            .unwrap_err()
            .code,
        ErrorCode::LoginHandoff
    );
}
#[test]
fn empty_single_and_multiple_page_discovery() {
    let dir = tempfile::tempdir().unwrap();
    for (data, count, pages) in [
        ("[]".to_owned(), 0, 1),
        (json!([fixture(1, "alice", "one")]).to_string(), 1, 1),
        (
            format!(
                "{}\n{}",
                json!([fixture(1, "alice", "one")]),
                json!([fixture(2, "org", "two")])
            ),
            2,
            2,
        ),
    ] {
        let gh = fake(dir.path(), &provider_script(&output_repos(&data)));
        let mut seen = (0, 0);
        let result = gh.fetch(&cancelled(), |p, n| seen = (p, n)).unwrap();
        assert_eq!(result.repositories.len(), count);
        assert_eq!(seen, (pages, count));
        assert_eq!(result.account.id, 7);
    }
}
#[test]
fn private_organization_fork_archived_empty_and_no_default_branch() {
    let mut data = fixture(42, "organization", "empty-fork");
    data["private"] = json!(true);
    data["visibility"] = json!("private");
    data["fork"] = json!(true);
    data["archived"] = json!(true);
    data["default_branch"] = Value::Null;
    data["pushed_at"] = Value::Null;
    let repos = parse_pages(
        json!([[data]]).to_string().as_bytes(),
        &cancelled(),
        &mut |_, _| {},
    )
    .unwrap();
    let r = &repos[0];
    assert_eq!(r.visibility, "private");
    assert!(r.fork && r.archived);
    assert!(r.default_branch.is_none() && r.pushed_at.is_none());
}
#[test]
fn malformed_or_untrusted_repository_fields_fail_closed() {
    for (key, value) in [
        ("id", json!(0)),
        ("name", json!("../escape")),
        ("html_url", json!("https://github.com.evil/alice/one")),
        (
            "clone_url",
            json!("https://secret@github.com/alice/one.git"),
        ),
        ("ssh_url", json!("ssh://evil/one")),
        ("description", json!("line\u{001b}escape")),
        ("updated_at", json!("yesterday")),
        ("visibility", json!("private")),
        ("default_branch", json!("bad\nbranch")),
    ] {
        let mut data = fixture(1, "alice", "one");
        data[key] = value;
        assert!(
            parse_pages(
                json!([[data]]).to_string().as_bytes(),
                &cancelled(),
                &mut |_, _| {}
            )
            .is_err(),
            "{key}"
        );
    }
    for bytes in [b"".as_slice(), b"{}", b"[]\n[", b"[] trailing"] {
        assert!(parse_pages(bytes, &cancelled(), &mut |_, _| {}).is_err());
    }
    let duplicate = json!([[fixture(1, "alice", "one"), fixture(1, "alice", "two")]]).to_string();
    assert!(parse_pages(duplicate.as_bytes(), &cancelled(), &mut |_, _| {}).is_err());
}
#[test]
fn page_and_library_limits_fail_without_truncation() {
    let page = (1..=101)
        .map(|id| fixture(id, "alice", &format!("r{id}")))
        .collect::<Vec<_>>();
    assert_eq!(
        parse_pages(
            serde_json::to_vec(&vec![page]).unwrap().as_slice(),
            &cancelled(),
            &mut |_, _| {}
        )
        .unwrap_err()
        .code,
        ErrorCode::LimitReached
    );
    let pages = serde_json::to_string(&vec![Vec::<Value>::new(); 1001]).unwrap();
    assert_eq!(
        parse_pages(pages.as_bytes(), &cancelled(), &mut |_, _| {})
            .unwrap_err()
            .code,
        ErrorCode::LimitReached
    );
}
#[test]
fn partial_pagination_failure_never_replaces_cache() {
    let dir = tempfile::tempdir().unwrap();
    let db = Registry::open(&dir.path().join("registry")).unwrap();
    db.apply_github(
        &snapshot(vec![repository(1, "alice", "before")]),
        &cancelled(),
    )
    .unwrap();
    let body = format!(
        "{}\necho 'HTTP 403: rate limit exceeded SENSITIVE' >&2\nexit 1",
        output_repos(&json!([fixture(2, "alice", "partial")]).to_string())
    );
    let gh = fake(dir.path(), &provider_script(&body));
    let error = gh.fetch(&cancelled(), |_, _| {}).unwrap_err();
    assert_eq!(error.code, ErrorCode::RateLimited);
    db.github_failed(&error).unwrap();
    drop(db);
    let db = Registry::open(&dir.path().join("registry")).unwrap();
    let cache = db.library(None).unwrap().cache;
    assert_eq!(cache.repositories.len(), 1);
    assert_eq!(cache.repositories[0].repository.name, "before");
    assert_eq!(cache.error.unwrap().code, ErrorCode::RateLimited);
}
#[test]
fn runner_timeout_cancel_output_limit_and_safe_errors() {
    let dir = tempfile::tempdir().unwrap();
    let mut gh = fake(dir.path(), "exec /bin/sleep 10");
    gh.timeout = Duration::from_millis(40);
    assert_eq!(gh.version().unwrap_err().code, ErrorCode::Timeout);
    gh.timeout = Duration::from_secs(2);
    let flag = Arc::new(AtomicBool::new(false));
    let other = flag.clone();
    let thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(40));
        other.store(true, Ordering::Relaxed);
    });
    assert_eq!(
        gh.run(&["--version"], &flag, Duration::from_secs(2))
            .err()
            .unwrap()
            .code,
        ErrorCode::Cancelled
    );
    thread.join().unwrap();
    let mut gh = fake(dir.path(), "exec /usr/bin/yes data");
    gh.output_limit = 1024;
    assert_eq!(gh.version().unwrap_err().code, ErrorCode::LimitReached);
    let error = classify(b"HTTP 401 SENSITIVE");
    assert_eq!(error.code, ErrorCode::Authentication);
    assert!(!error.message.contains("SENSITIVE"));
    assert_eq!(
        classify(b"HTTP 403 forbidden").code,
        ErrorCode::ProviderAccess
    );
    assert_eq!(classify(b"HTTP 429").code, ErrorCode::RateLimited);
    assert_eq!(
        classify(b"dial tcp connection refused").code,
        ErrorCode::Network
    );
}
#[test]
fn runner_uses_fixed_environment_and_non_repository_directory() {
    let dir = tempfile::tempdir().unwrap();
    let gh = fake(
        dir.path(),
        r#"[ "$PWD" = / ] || exit 1
[ "$PATH" = /usr/bin:/bin ] || exit 2
[ "$GH_PROMPT_DISABLED" = 1 ] || exit 3
[ "$GH_HOST" = github.com ] || exit 4
[ -z "$GH_TOKEN$GITHUB_TOKEN$GH_DEBUG$GH_REPO$GIT_DIR" ] || exit 5
echo 'gh version 2.102.0'"#,
    );
    assert!(gh.version().is_ok());
}
#[test]
fn account_switch_during_fetch_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let counter = dir.path().join("counter");
    let script=provider_script(&output_repos("[]")).replace("echo '{\"id\":7,\"login\":\"alice\"}'",&format!("if [ -f '{}' ]; then echo '{{\"id\":8,\"login\":\"bob\"}}'; else /usr/bin/touch '{}'; echo '{{\"id\":7,\"login\":\"alice\"}}'; fi",counter.display(),counter.display()));
    let gh = fake(dir.path(), &script);
    assert_eq!(
        gh.fetch(&cancelled(), |_, _| {}).unwrap_err().code,
        ErrorCode::Authentication
    );
}
#[test]
fn migration_from_v1_preserves_local_rows_and_remote_cache_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("registry");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(include_str!("../migrations/001_registry.sql"))
        .unwrap();
    conn.execute("INSERT INTO projects(id,name,path,registered_at,availability) VALUES (1,'local','/kept',1,'missing')",[]).unwrap();
    drop(conn);
    let db = Registry::open(&path).unwrap();
    db.apply_github(
        &snapshot(vec![repository(42, "alice", "one")]),
        &cancelled(),
    )
    .unwrap();
    drop(db);
    let db = Registry::open(&path).unwrap();
    let library = db.library(None).unwrap();
    assert_eq!(library.projects[0].path, "/kept");
    assert_eq!(library.cache.repositories[0].repository.id, 42);
    let conn = rusqlite::Connection::open(&path).unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 7);
}
#[test]
fn stable_ids_rename_transfer_and_absent_records_retained() {
    let dir = tempfile::tempdir().unwrap();
    let db = Registry::open(&dir.path().join("db")).unwrap();
    db.apply_github(
        &snapshot(vec![repository(42, "alice", "before")]),
        &cancelled(),
    )
    .unwrap();
    db.apply_github(
        &snapshot(vec![repository(42, "org", "after")]),
        &cancelled(),
    )
    .unwrap();
    let cache = db.library(None).unwrap().cache;
    assert_eq!(cache.repositories.len(), 1);
    assert_eq!(cache.repositories[0].repository.full_name, "org/after");
    db.apply_github(&snapshot(vec![]), &cancelled()).unwrap();
    assert_eq!(
        db.library(None).unwrap().cache.repositories[0].availability,
        "not_returned"
    );
    db.apply_github(
        &snapshot(vec![repository(42, "org", "after")]),
        &cancelled(),
    )
    .unwrap();
    assert_eq!(
        db.library(None).unwrap().cache.repositories[0].availability,
        "seen"
    );
}
#[test]
fn account_boundaries_preserved_and_cancelled_transaction_rolls_back() {
    let dir = tempfile::tempdir().unwrap();
    let db = Registry::open(&dir.path().join("db")).unwrap();
    db.apply_github(
        &snapshot(vec![repository(42, "alice", "private")]),
        &cancelled(),
    )
    .unwrap();
    let other = Snapshot {
        account: Account {
            id: 8,
            login: "bob".into(),
        },
        repositories: vec![repository(43, "bob", "other")],
    };
    db.apply_github(&other, &cancelled()).unwrap();
    assert_eq!(db.library(None).unwrap().cache.account_id, Some(8));
    assert_eq!(
        db.library(Some(7)).unwrap().cache.repositories[0]
            .repository
            .id,
        42
    );
    assert_eq!(
        db.library(Some(8)).unwrap().cache.repositories[0]
            .repository
            .id,
        43
    );
    assert!(db.library(Some(99)).is_err());
    assert_eq!(
        db.apply_github(&snapshot(vec![]), &AtomicBool::new(true))
            .unwrap_err()
            .code,
        ErrorCode::Cancelled
    );
    assert_eq!(
        db.library(Some(7)).unwrap().cache.repositories[0].availability,
        "seen"
    );
}
#[test]
fn database_error_mid_refresh_rolls_back_all_metadata_and_timestamp() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let db = Registry::open(&path).unwrap();
    db.apply_github(
        &snapshot(vec![repository(1, "alice", "before")]),
        &cancelled(),
    )
    .unwrap();
    let before = db.library(None).unwrap().cache.accounts[0].refreshed_at;
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TRIGGER fail_insert BEFORE INSERT ON remote_repositories WHEN NEW.provider_id=2 BEGIN SELECT RAISE(ABORT, 'test failure'); END;").unwrap();
    assert!(db
        .apply_github(
            &snapshot(vec![
                repository(1, "alice", "after"),
                repository(2, "alice", "new")
            ]),
            &cancelled()
        )
        .is_err());
    let cache = db.library(None).unwrap().cache;
    assert_eq!(cache.repositories.len(), 1);
    assert_eq!(cache.repositories[0].repository.name, "before");
    assert_eq!(cache.accounts[0].refreshed_at, before);
}
#[test]
fn conservative_url_normalization() {
    for url in [
        "https://GitHub.COM/Owner/Repo",
        "https://github.com/owner/repo.git",
        "git@github.com:owner/repo.git",
        "github.com:owner/repo",
        "ssh://git@github.com/owner/repo.git",
        "ssh://github.com/owner/repo",
    ] {
        assert_eq!(remote_identity(url).as_deref(), Some("owner/repo"), "{url}");
    }
    for url in [
        "https://github.com.evil/owner/repo",
        "https://github.com/owner/repo?token=secret",
        "https://github.com/owner/repo#fragment",
        "https://user@github.com/owner/repo",
        "https://github.com:443/owner/repo",
        "https://github.com/owner/%72epo",
        "https://github.com/owner/repo/extra",
        "file:///owner/repo",
        "/local/repo",
        "git@enterprise:owner/repo",
    ] {
        assert!(remote_identity(url).is_none(), "{url}");
    }
}
#[test]
fn matching_non_origin_multiple_no_remote_and_rename_uncertainty() {
    let dir = tempfile::tempdir().unwrap();
    let db = Registry::open(&dir.path().join("db")).unwrap();
    db.apply_github(
        &snapshot(vec![
            repository(1, "alice", "one"),
            repository(2, "org", "two"),
        ]),
        &cancelled(),
    )
    .unwrap();
    let repos = db.library(None).unwrap().cache.repositories;
    let names = vec![("old/one".into(), 1)];
    let cases = [
        (vec![], "local", None),
        (vec!["git@github.com:alice/one.git"], "linked", Some(1)),
        (
            vec![
                "https://github.com/alice/one",
                "ssh://github.com/alice/one.git",
            ],
            "linked",
            Some(1),
        ),
        (
            vec!["https://github.com/alice/one", "https://github.com/org/two"],
            "uncertain",
            None,
        ),
        (
            vec![
                "https://github.com/alice/one",
                "https://github.com/unlisted/repo",
            ],
            "uncertain",
            None,
        ),
        (vec!["https://github.com/old/one"], "uncertain", None),
    ];
    for (urls, state, id) in cases {
        let links = match_projects(&[project(&urls)], &repos, &names);
        assert_eq!(links[0].state, state);
        assert_eq!(links[0].repository_id, id);
    }
    let mut p = project(&["https://github.com/alice/one"]);
    p.availability = "missing".into();
    assert_eq!(match_projects(&[p], &repos, &[])[0].state, "uncertain");
    assert_eq!(
        match_projects(
            &[project(&["https://github.com/alice/one"])],
            &repos,
            &[("alice/one".into(), 999)]
        )[0]
        .state,
        "uncertain"
    );
}
#[test]
fn shared_operations_prevent_duplicate_or_out_of_order_refreshes() {
    let operations = crate::operations::Operations::default();
    let (op, flag) = operations.start("github_refresh").unwrap();
    assert_eq!(
        operations.start("github_refresh").unwrap_err().code,
        ErrorCode::Busy
    );
    assert_eq!(operations.start("scan").unwrap_err().code, ErrorCode::Busy);
    operations.cancel(&op.id).unwrap();
    assert!(flag.load(Ordering::Relaxed));
    operations
        .finish(&op.id, Err(failure(ErrorCode::Cancelled, "cancelled")))
        .unwrap();
    assert_eq!(
        operations.list().unwrap()[0].state,
        crate::operations::OperationState::Cancelled
    );
    assert!(operations.start("github_refresh").is_ok());
}
#[test]
fn external_open_revalidates_cached_url() {
    let mut repo = repository(1, "alice", "one");
    repo.web_url = "https://evil.example".into();
    assert_eq!(
        open_repository(&repo).unwrap_err().code,
        ErrorCode::ProviderData
    );
}

#[test]
fn slurped_pagination_exceeds_one_hundred_total_without_merging_pages() {
    let first = (1..=100)
        .map(|id| fixture(id, "alice", &format!("r{id}")))
        .collect::<Vec<_>>();
    let second = vec![fixture(101, "org", "last")];
    let data = json!([first, second]);
    let mut progress = (0, 0);
    let result = parse_pages(data.to_string().as_bytes(), &cancelled(), &mut |p, n| {
        progress = (p, n)
    })
    .unwrap();
    assert_eq!(result.len(), 101);
    assert_eq!(progress, (2, 101));
    // Piped gh without --slurp merges arrays: reject that shape rather than
    // silently misreporting page completion or accepting a truncated result.
    assert!(parse_pages(
        json!([fixture(1, "alice", "one")]).to_string().as_bytes(),
        &cancelled(),
        &mut |_, _| {}
    )
    .is_err());
}

#[test]
fn clone_identity_revalidates_id_url_and_name() {
    let t = tempfile::tempdir().unwrap();
    let expected = repository(42, "alice", "example");
    let cli = fake(
        t.path(),
        &format!("cat <<'JSON'\n{}\nJSON", fixture(42, "alice", "example")),
    );
    assert_eq!(
        cli.verify_repository(&expected, &AtomicBool::new(false))
            .unwrap()
            .id,
        42
    );
    let cli = fake(
        t.path(),
        &format!("cat <<'JSON'\n{}\nJSON", fixture(43, "alice", "example")),
    );
    assert_eq!(
        cli.verify_repository(&expected, &AtomicBool::new(false))
            .unwrap_err()
            .code,
        ErrorCode::ProviderData
    );
    let mut bad = expected.clone();
    bad.https_url = "https://evil.invalid/alice/example.git".into();
    assert_eq!(
        cli.verify_repository(&bad, &AtomicBool::new(false))
            .unwrap_err()
            .code,
        ErrorCode::ProviderData
    );
    bad = expected;
    bad.name = "../other".into();
    assert!(cli
        .verify_repository(&bad, &AtomicBool::new(false))
        .is_err());
}
