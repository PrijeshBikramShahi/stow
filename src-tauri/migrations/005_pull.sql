CREATE TABLE pull_jobs (
 id TEXT PRIMARY KEY,
 project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
 started_at INTEGER NOT NULL,
 record_json TEXT NOT NULL
);
CREATE INDEX pull_jobs_project ON pull_jobs(project_id,started_at);
PRAGMA user_version=5;
