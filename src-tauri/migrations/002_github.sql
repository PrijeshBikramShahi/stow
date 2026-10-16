CREATE TABLE github_accounts (
    id INTEGER PRIMARY KEY,
    login TEXT NOT NULL,
    refreshed_at INTEGER NOT NULL
);
CREATE TABLE remote_repositories (
    provider TEXT NOT NULL CHECK(provider = 'github'),
    host TEXT NOT NULL CHECK(host = 'github.com'),
    provider_id INTEGER NOT NULL CHECK(provider_id > 0),
    metadata_json TEXT NOT NULL,
    fetched_at INTEGER NOT NULL,
    account_id INTEGER NOT NULL REFERENCES github_accounts(id),
    PRIMARY KEY(provider, host, provider_id)
);
CREATE TABLE github_access (
    account_id INTEGER NOT NULL REFERENCES github_accounts(id),
    repository_id INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    availability TEXT NOT NULL CHECK(availability IN ('seen', 'not_returned')),
    PRIMARY KEY(account_id, repository_id)
);
-- Retain historical names as uncertain candidates, never as proof of a link.
CREATE TABLE github_names (
    full_name TEXT NOT NULL COLLATE NOCASE,
    repository_id INTEGER NOT NULL,
    PRIMARY KEY(full_name, repository_id)
);
CREATE TABLE github_refresh (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    account_id INTEGER REFERENCES github_accounts(id),
    attempted_at INTEGER,
    error_json TEXT
);
INSERT INTO github_refresh(singleton) VALUES (1);
PRAGMA user_version = 2;
