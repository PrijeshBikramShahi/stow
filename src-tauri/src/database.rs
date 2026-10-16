use crate::core::{now, AppError, ErrorCode, Inspection, Project, Result};
use rusqlite::{params, Connection};
use std::{path::Path, sync::Mutex, time::Duration};

pub struct Registry {
    connection: Mutex<Connection>,
}
fn db_error(_: impl std::fmt::Display) -> AppError {
    AppError::new(ErrorCode::Database, "The project registry could not be read or written. Check application-data permissions and available disk space.")
}

impl Registry {
    pub fn open(path: &Path) -> Result<Self> {
        let mut connection = Connection::open(path).map_err(db_error)?;
        connection
            .busy_timeout(Duration::from_secs(3))
            .map_err(db_error)?;
        connection
            .execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;")
            .map_err(db_error)?;
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(db_error)?;
        if version > 5 {
            return Err(AppError::new(
                ErrorCode::Database,
                "This registry was created by a newer Stow version.",
            ));
        }
        if version == 0 {
            let transaction = connection.transaction().map_err(db_error)?;
            transaction
                .execute_batch(include_str!("../migrations/001_registry.sql"))
                .map_err(db_error)?;
            transaction.commit().map_err(db_error)?;
        }
        if version < 2 {
            let transaction = connection.transaction().map_err(db_error)?;
            transaction
                .execute_batch(include_str!("../migrations/002_github.sql"))
                .map_err(db_error)?;
            transaction.commit().map_err(db_error)?;
        }
        if version < 3 {
            let transaction = connection.transaction().map_err(db_error)?;
            transaction
                .execute_batch(include_str!("../migrations/003_clone.sql"))
                .map_err(db_error)?;
            transaction.commit().map_err(db_error)?;
        }
        if version < 4 {
            let transaction = connection.transaction().map_err(db_error)?;
            transaction
                .execute_batch(include_str!("../migrations/004_sync.sql"))
                .map_err(db_error)?;
            transaction.commit().map_err(db_error)?;
        }
        if version < 5 {
            let transaction = connection.transaction().map_err(db_error)?;
            transaction
                .execute_batch(include_str!("../migrations/005_pull.sql"))
                .map_err(db_error)?;
            transaction.commit().map_err(db_error)?;
        }
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn register(&self, path: &Path, inspection: &Inspection) -> Result<()> {
        let canonical = path.canonicalize().map_err(AppError::io)?;
        let path = crate::core::path_string(&canonical)?;
        let name = canonical
            .file_name()
            .and_then(|x| x.to_str())
            .unwrap_or(&path);
        let json = serde_json::to_string(inspection).map_err(db_error)?;
        self.connection.lock().map_err(db_error)?.execute(
            "INSERT INTO projects(name,path,registered_at,inspected_at,availability,inspection_json) VALUES (?1,?2,?3,?3,'available',?4)
             ON CONFLICT(path) DO UPDATE SET inspected_at=excluded.inspected_at, availability='available', inspection_json=excluded.inspection_json,error_json=NULL",
            params![name, path, now(), json],
        ).map_err(db_error)?;
        Ok(())
    }

    pub fn mark_error(&self, id: i64, error: &AppError) -> Result<()> {
        let availability = match error.code {
            ErrorCode::Missing => "missing",
            ErrorCode::Unsupported => "unsupported",
            _ => "error",
        };
        self.connection
            .lock()
            .map_err(db_error)?
            .execute(
                "UPDATE projects SET availability=?1,error_json=?2 WHERE id=?3",
                params![
                    availability,
                    serde_json::to_string(error).map_err(db_error)?,
                    id
                ],
            )
            .map_err(db_error)?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<Project>> {
        let connection = self.connection.lock().map_err(db_error)?;
        let mut stmt = connection.prepare("SELECT id,name,path,registered_at,inspected_at,availability,inspection_json,error_json FROM projects ORDER BY name COLLATE NOCASE,path").map_err(db_error)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    Project {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        path: row.get(2)?,
                        registered_at: row.get(3)?,
                        inspected_at: row.get(4)?,
                        availability: row.get(5)?,
                        inspection: None,
                        error: None,
                    },
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            })
            .map_err(db_error)?;
        rows.map(|row| {
            let (mut project, inspection, error) = row.map_err(db_error)?;
            project.inspection = inspection
                .map(|x| serde_json::from_str(&x))
                .transpose()
                .map_err(db_error)?;
            project.error = error
                .map(|x| serde_json::from_str(&x))
                .transpose()
                .map_err(db_error)?;
            Ok(project)
        })
        .collect()
    }

    pub fn get(&self, id: i64) -> Result<Project> {
        self.list()?
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| {
                AppError::new(ErrorCode::Missing, "Project registration no longer exists.")
            })
    }

    pub fn remove(&self, id: i64) -> Result<()> {
        self.connection
            .lock()
            .map_err(db_error)?
            .execute("DELETE FROM projects WHERE id=?1", [id])
            .map_err(db_error)?;
        Ok(())
    }
}

impl Registry {
    pub fn apply_github(
        &self,
        snapshot: &crate::github::Snapshot,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<()> {
        use crate::git::check_cancel;
        check_cancel(cancel)?;
        let mut connection = self.connection.lock().map_err(db_error)?;
        let transaction = connection.transaction().map_err(db_error)?;
        let fetched_at = now();
        transaction.execute("INSERT INTO github_accounts(id,login,refreshed_at) VALUES (?1,?2,?3) ON CONFLICT(id) DO UPDATE SET login=excluded.login,refreshed_at=excluded.refreshed_at", params![snapshot.account.id, snapshot.account.login, fetched_at]).map_err(db_error)?;
        transaction
            .execute(
                "UPDATE github_access SET availability='not_returned' WHERE account_id=?1",
                [snapshot.account.id],
            )
            .map_err(db_error)?;
        for repo in &snapshot.repositories {
            check_cancel(cancel)?;
            transaction.execute("INSERT INTO remote_repositories(provider,host,provider_id,metadata_json,fetched_at,account_id) VALUES ('github','github.com',?1,?2,?3,?4) ON CONFLICT(provider,host,provider_id) DO UPDATE SET metadata_json=excluded.metadata_json,fetched_at=excluded.fetched_at,account_id=excluded.account_id", params![repo.id, serde_json::to_string(repo).map_err(db_error)?, fetched_at, snapshot.account.id]).map_err(db_error)?;
            transaction.execute("INSERT INTO github_access(account_id,repository_id,last_seen_at,availability) VALUES (?1,?2,?3,'seen') ON CONFLICT(account_id,repository_id) DO UPDATE SET last_seen_at=excluded.last_seen_at,availability='seen'", params![snapshot.account.id, repo.id, fetched_at]).map_err(db_error)?;
            transaction
                .execute(
                    "INSERT OR IGNORE INTO github_names(full_name,repository_id) VALUES (?1,?2)",
                    params![repo.full_name, repo.id],
                )
                .map_err(db_error)?;
        }
        transaction.execute("UPDATE github_refresh SET account_id=?1,attempted_at=?2,error_json=NULL WHERE singleton=1", params![snapshot.account.id, fetched_at]).map_err(db_error)?;
        check_cancel(cancel)?;
        transaction.commit().map_err(db_error)
    }
    pub fn github_failed(&self, error: &AppError) -> Result<()> {
        self.connection
            .lock()
            .map_err(db_error)?
            .execute(
                "UPDATE github_refresh SET attempted_at=?1,error_json=?2 WHERE singleton=1",
                params![now(), serde_json::to_string(error).map_err(db_error)?],
            )
            .map_err(db_error)?;
        Ok(())
    }
    pub fn library(&self, account_id: Option<i64>) -> Result<crate::library::Library> {
        use crate::{
            github::Account,
            library::{match_projects, Cache, CachedAccount, CachedRepository, Library},
        };
        let projects = self.list()?;
        let connection = self.connection.lock().map_err(db_error)?;
        let (last_account, attempted_at, error): (Option<i64>, Option<i64>, Option<String>) = connection.query_row("SELECT account_id,attempted_at,error_json FROM github_refresh WHERE singleton=1", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(db_error)?;
        let mut stmt = connection
            .prepare(
                "SELECT id,login,refreshed_at FROM github_accounts ORDER BY login COLLATE NOCASE",
            )
            .map_err(db_error)?;
        let accounts = stmt
            .query_map([], |r| {
                Ok(CachedAccount {
                    account: Account {
                        id: r.get(0)?,
                        login: r.get(1)?,
                    },
                    refreshed_at: r.get(2)?,
                })
            })
            .map_err(db_error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_error)?;
        let account_id = account_id.or(last_account);
        if account_id.is_some_and(|id| !accounts.iter().any(|a| a.account.id == id)) {
            return Err(AppError::new(
                ErrorCode::Missing,
                "No cached library exists for that account.",
            ));
        }
        let mut stmt = connection.prepare("SELECT r.metadata_json,r.fetched_at,a.id,a.login,g.last_seen_at,g.availability FROM remote_repositories r JOIN github_access g ON g.repository_id=r.provider_id JOIN github_accounts a ON a.id=r.account_id WHERE g.account_id=?1 ORDER BY r.provider_id").map_err(db_error)?;
        let rows = stmt
            .query_map([account_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })
            .map_err(db_error)?;
        let repositories = rows
            .map(|row| {
                let (json, fetched_at, id, login, last_seen_at, availability) =
                    row.map_err(db_error)?;
                Ok(CachedRepository {
                    repository: serde_json::from_str(&json).map_err(db_error)?,
                    fetched_at,
                    last_account: Account { id, login },
                    last_seen_at,
                    availability,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let mut stmt = connection
            .prepare("SELECT full_name,repository_id FROM github_names")
            .map_err(db_error)?;
        let names = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map_err(db_error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_error)?;
        let links = match_projects(&projects, &repositories, &names);
        Ok(Library {
            projects,
            links,
            cache: Cache {
                accounts,
                account_id,
                repositories,
                attempted_at,
                error: error
                    .map(|s| serde_json::from_str(&s))
                    .transpose()
                    .map_err(db_error)?,
            },
        })
    }
}

impl Registry {
    pub fn preferences(&self) -> Result<crate::clone::Preferences> {
        self.connection
            .lock()
            .map_err(db_error)?
            .query_row(
                "SELECT root,editor FROM workspace_preferences WHERE singleton=1",
                [],
                |r| {
                    Ok(crate::clone::Preferences {
                        root: r.get(0)?,
                        editor: r.get(1)?,
                    })
                },
            )
            .map_err(db_error)
    }
    pub fn save_preferences(&self, preferences: &crate::clone::Preferences) -> Result<()> {
        crate::editor::validate_editor(&preferences.editor)?;
        if let Some(root) = &preferences.root {
            crate::safe_fs::Directory::open(Path::new(root))?.owned()?;
        }
        self.connection
            .lock()
            .map_err(db_error)?
            .execute(
                "UPDATE workspace_preferences SET root=?1,editor=?2 WHERE singleton=1",
                params![preferences.root, preferences.editor],
            )
            .map_err(db_error)?;
        Ok(())
    }
    pub fn save_clone(&self, job: &crate::clone::CloneJob) -> Result<()> {
        self.connection.lock().map_err(db_error)?.execute("INSERT INTO clone_jobs(id,updated_at,record_json) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET updated_at=excluded.updated_at,record_json=excluded.record_json",params![job.id,job.updated_at,serde_json::to_string(job).map_err(db_error)?]).map_err(db_error)?;
        Ok(())
    }
    pub fn clone_jobs(&self) -> Result<Vec<crate::clone::CloneJob>> {
        let connection = self.connection.lock().map_err(db_error)?;
        let mut stmt = connection
            .prepare("SELECT record_json FROM clone_jobs ORDER BY updated_at DESC,id DESC")
            .map_err(db_error)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(db_error)?;
        rows.map(|r| serde_json::from_str(&r.map_err(db_error)?).map_err(db_error))
            .collect()
    }
    pub fn clone_job(&self, id: &str) -> Result<crate::clone::CloneJob> {
        let connection = self.connection.lock().map_err(db_error)?;
        let json: String = connection
            .query_row(
                "SELECT record_json FROM clone_jobs WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        serde_json::from_str(&json).map_err(db_error)
    }
    pub fn register_clone(
        &self,
        job: &mut crate::clone::CloneJob,
        inspection: &Inspection,
    ) -> Result<()> {
        let mut connection = self.connection.lock().map_err(db_error)?;
        let transaction = connection.transaction().map_err(db_error)?;
        // Do not upsert into an unrelated/stale registration with the same path.
        transaction.execute("INSERT INTO projects(name,path,registered_at,inspected_at,availability,inspection_json) VALUES(?1,?2,?3,?3,'available',?4)",params![job.name,job.destination,now(),serde_json::to_string(inspection).map_err(db_error)?]).map_err(db_error)?;
        let mut complete = job.clone();
        complete.project_id = Some(transaction.last_insert_rowid());
        complete.phase = "registered".into();
        complete.state = "completed".into();
        complete.error = None;
        complete.updated_at = now();
        transaction
            .execute(
                "UPDATE clone_jobs SET updated_at=?1,record_json=?2 WHERE id=?3",
                params![
                    complete.updated_at,
                    serde_json::to_string(&complete).map_err(db_error)?,
                    complete.id
                ],
            )
            .map_err(db_error)?;
        transaction.commit().map_err(db_error)?;
        *job = complete;
        Ok(())
    }
}

impl Registry {
    pub fn sync_record(&self, id: i64) -> Result<crate::sync::SyncRecord> {
        use rusqlite::OptionalExtension;
        let json: Option<String> = self
            .connection
            .lock()
            .map_err(db_error)?
            .query_row(
                "SELECT record_json FROM sync_states WHERE project_id=?1",
                [id],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?;
        json.map(|j| serde_json::from_str(&j).map_err(db_error))
            .unwrap_or_else(|| {
                Ok(crate::sync::SyncRecord {
                    project_id: id,
                    assessment: None,
                    error: None,
                })
            })
    }
    pub fn save_sync(&self, record: &crate::sync::SyncRecord) -> Result<()> {
        self.connection.lock().map_err(db_error)?.execute("INSERT INTO sync_states(project_id,record_json) VALUES(?1,?2) ON CONFLICT(project_id) DO UPDATE SET record_json=excluded.record_json",params![record.project_id,serde_json::to_string(record).map_err(db_error)?]).map_err(db_error)?;
        Ok(())
    }
    pub fn save_pull(&self, job: &crate::pull::PullJob) -> Result<()> {
        self.connection.lock().map_err(db_error)?.execute("INSERT INTO pull_jobs(id,project_id,started_at,record_json) VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET record_json=excluded.record_json",params![job.id,job.project_id,job.started_at,serde_json::to_string(job).map_err(db_error)?]).map_err(db_error)?;
        Ok(())
    }
    pub fn pull_history(&self, id: i64) -> Result<Vec<crate::pull::PullJob>> {
        let connection = self.connection.lock().map_err(db_error)?;
        let mut stmt = connection.prepare("SELECT record_json FROM pull_jobs WHERE project_id=?1 ORDER BY started_at DESC,rowid DESC LIMIT 50").map_err(db_error)?;
        let rows = stmt
            .query_map([id], |r| r.get::<_, String>(0))
            .map_err(db_error)?;
        rows.map(|row| serde_json::from_str(&row.map_err(db_error)?).map_err(db_error))
            .collect()
    }
    // Called once on desktop startup, never by a reader opening an active registry.
    // No filesystem mutation or inference of success from HEAD alone.
    pub fn reconcile_pulls(&self) -> Result<()> {
        self.connection.lock().map_err(db_error)?.execute_batch("UPDATE pull_jobs SET record_json=json_set(record_json,'$.state','requires_inspection','$.error',json('{\"code\":\"recovery_required\",\"message\":\"Application interrupted this pull. Inspect HEAD, index, files and Git locks before retrying. No automatic recovery was attempted.\"}')) WHERE json_extract(record_json,'$.state')='running';").map_err(db_error)?;
        Ok(())
    }
    pub fn save_fetch(&self, job: &crate::sync::FetchJob) -> Result<()> {
        self.connection.lock().map_err(db_error)?.execute("INSERT INTO fetch_jobs(id,project_id,started_at,record_json) VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET record_json=excluded.record_json",params![job.id,job.project_id,job.started_at,serde_json::to_string(job).map_err(db_error)?]).map_err(db_error)?;
        Ok(())
    }
    pub fn fetch_history(&self, id: i64) -> Result<Vec<crate::sync::FetchJob>> {
        let connection = self.connection.lock().map_err(db_error)?;
        let mut stmt=connection.prepare("SELECT record_json FROM fetch_jobs WHERE project_id=?1 ORDER BY started_at DESC,rowid DESC LIMIT 50").map_err(db_error)?;
        let rows = stmt
            .query_map([id], |r| r.get::<_, String>(0))
            .map_err(db_error)?;
        rows.map(|row| serde_json::from_str(&row.map_err(db_error)?).map_err(db_error))
            .collect()
    }
}
