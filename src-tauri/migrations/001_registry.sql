CREATE TABLE projects (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    path TEXT NOT NULL UNIQUE,
    registered_at INTEGER NOT NULL,
    inspected_at INTEGER,
    availability TEXT NOT NULL CHECK (availability IN ('available', 'missing', 'error', 'unsupported')),
    inspection_json TEXT,
    error_json TEXT
);
CREATE INDEX projects_name ON projects(name COLLATE NOCASE);
PRAGMA user_version = 1;
