CREATE TABLE push_jobs (
 id TEXT PRIMARY KEY,
 project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
 started_at INTEGER NOT NULL,
 record_json TEXT NOT NULL
);
PRAGMA user_version=6;
