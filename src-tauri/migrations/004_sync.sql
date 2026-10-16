CREATE TABLE sync_states (
 project_id INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
 record_json TEXT NOT NULL
);
CREATE TABLE fetch_jobs (
 id TEXT PRIMARY KEY,
 project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
 started_at INTEGER NOT NULL,
 record_json TEXT NOT NULL
);
CREATE INDEX fetch_jobs_project ON fetch_jobs(project_id,started_at);
PRAGMA user_version=4;
