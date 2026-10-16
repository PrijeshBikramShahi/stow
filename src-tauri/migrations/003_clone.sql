CREATE TABLE workspace_preferences (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    root TEXT,
    editor TEXT NOT NULL CHECK(editor IN ('code','cursor','finder'))
);
INSERT INTO workspace_preferences(singleton,editor) VALUES(1,'finder');
-- Durable saga journal; filesystem promotion and database commit are distinct.
CREATE TABLE clone_jobs (
    id TEXT PRIMARY KEY,
    updated_at INTEGER NOT NULL,
    record_json TEXT NOT NULL
);
PRAGMA user_version = 3;
