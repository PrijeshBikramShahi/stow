use crate::{
    core::{AppError, ErrorCode, Project, Result},
    database::Registry,
    git::{check_cancel, Git},
    operations::{Operation, Operations},
    projects::{self, ScanIssue, ScanLimits, ScanProgress},
};
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tauri::{Emitter, State};

pub struct Service {
    pub registry: Registry,
    pub git: Git,
    pub operations: Operations,
    // Serialize operation admission and registry removal, avoiding unregister/scan races.
    pub admission: Mutex<()>,
    pub pull_confirmation: Mutex<Option<(String, std::sync::mpsc::Sender<bool>)>>,
    pub push_confirmation: Mutex<Option<(String, std::sync::mpsc::Sender<bool>)>>,
    pub change_snapshot: Mutex<Option<crate::changes::Snapshot>>,
    pub commit_confirmation:
        Mutex<Option<(crate::changes::CommitPreview, std::sync::mpsc::Sender<bool>)>>,
}
pub struct AppState(pub Result<Arc<Service>>);
fn service(state: &State<'_, AppState>) -> Result<Arc<Service>> {
    state.0.clone()
}

#[derive(Serialize)]
pub struct SystemInfo {
    git_version: Option<String>,
    git_error: Option<AppError>,
    database_error: Option<AppError>,
}

#[tauri::command]
pub async fn system_info(state: State<'_, AppState>) -> Result<SystemInfo> {
    let database_error = state.0.as_ref().err().cloned();
    let git = tauri::async_runtime::spawn_blocking(|| Git::default().version())
        .await
        .map_err(join_error)?;
    Ok(SystemInfo {
        git_version: git.as_ref().ok().cloned(),
        git_error: git.err(),
        database_error,
    })
}

#[tauri::command]
pub async fn list_projects(state: State<'_, AppState>) -> Result<Vec<Project>> {
    let service = service(&state)?;
    tauri::async_runtime::spawn_blocking(move || service.registry.list())
        .await
        .map_err(join_error)?
}

#[tauri::command]
pub fn list_operations(state: State<'_, AppState>) -> Result<Vec<Operation>> {
    service(&state)?.operations.list()
}

#[tauri::command]
pub fn cancel_operation(id: String, state: State<'_, AppState>) -> Result<()> {
    service(&state)?.operations.cancel(&id)
}

#[tauri::command]
pub async fn remove_project(id: i64, state: State<'_, AppState>) -> Result<()> {
    let service = service(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = service.admission.lock().map_err(join_error)?;
        service.operations.ensure_idle()?;
        service.registry.remove(id)
    })
    .await
    .map_err(join_error)?
}

#[tauri::command]
pub fn start_scan(
    roots: Vec<String>,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Operation> {
    if roots.is_empty() || roots.len() > 16 || roots.iter().any(|r| r.len() > 4096) {
        return Err(AppError::new(
            ErrorCode::InvalidPath,
            "Choose 1–16 directories with valid paths.",
        ));
    }
    let service = service(&state)?;
    let (operation, cancel) = {
        let _guard = service.admission.lock().map_err(join_error)?;
        service.operations.start("scan")?
    };
    let id = operation.id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let roots: Vec<_> = roots.into_iter().map(PathBuf::from).collect();
            let mut last_event = std::time::Instant::now();
            projects::scan(
                &roots,
                &service.registry,
                &service.git,
                &cancel,
                &ScanLimits::default(),
                |progress| {
                    let _ = service.operations.progress(&id, progress.clone());
                    if last_event.elapsed() >= std::time::Duration::from_millis(250) {
                        let _ = app.emit("operation-progress", &id);
                        last_event = std::time::Instant::now();
                    }
                },
            )
            .map(|_| ())
        }))
        .unwrap_or_else(|_| {
            Err(AppError::new(
                ErrorCode::Internal,
                "Scan worker failed. Previously registered projects are preserved.",
            ))
        });
        let _ = service.operations.finish(&id, outcome);
        let _ = app.emit("operation-progress", &id);
    });
    Ok(operation)
}

#[tauri::command]
pub fn start_inspection(
    id: Option<i64>,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Operation> {
    let service = service(&state)?;
    let (operation, cancel) = {
        let _guard = service.admission.lock().map_err(join_error)?;
        service.operations.start(if id.is_some() {
            "inspection"
        } else {
            "refresh"
        })?
    };
    let operation_id = operation.id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let projects = if let Some(id) = id {
                vec![service.registry.get(id)?]
            } else {
                service.registry.list()?
            };
            let mut progress = ScanProgress::default();
            for project in projects {
                check_cancel(&cancel)?;
                match projects::inspect_registered(
                    &service.registry,
                    &service.git,
                    project.id,
                    &cancel,
                ) {
                    Ok(()) => progress.registered += 1,
                    Err(error)
                        if error.code == ErrorCode::Cancelled
                            || error.code == ErrorCode::Database =>
                    {
                        return Err(error)
                    }
                    Err(error) => {
                        progress.skipped += 1;
                        if progress.issues.len() < 100 {
                            progress.issues.push(ScanIssue {
                                path: project.path,
                                error: error.clone(),
                            });
                        }
                        if id.is_some() {
                            return Err(error);
                        }
                    }
                }
                progress.visited += 1;
                service
                    .operations
                    .progress(&operation_id, progress.clone())?;
                let _ = app.emit("operation-progress", &operation_id);
            }
            Ok(())
        }))
        .unwrap_or_else(|_| {
            Err(AppError::new(
                ErrorCode::Internal,
                "Inspection worker failed.",
            ))
        });
        let _ = service.operations.finish(&operation_id, outcome);
        let _ = app.emit("operation-progress", &operation_id);
    });
    Ok(operation)
}
fn join_error<T>(_: T) -> AppError {
    AppError::new(
        ErrorCode::Internal,
        "Background task failed. Restart Stow and try again.",
    )
}

fn reviewed(service: &Service, project_id: i64, token: &str) -> Result<crate::changes::Snapshot> {
    service
        .change_snapshot
        .lock()
        .map_err(join_error)?
        .as_ref()
        .filter(|s| s.view.project_id == project_id && s.view.token == token)
        .cloned()
        .ok_or_else(|| {
            AppError::new(
                ErrorCode::RecoveryRequired,
                "Change review expired. Refresh changes and select again.",
            )
        })
}
#[tauri::command]
pub async fn review_changes(
    project_id: i64,
    state: State<'_, AppState>,
) -> Result<crate::changes::Review> {
    let service = service(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = service.admission.lock().map_err(join_error)?;
        service.operations.ensure_idle()?;
        // Only one bounded in-memory review is retained; no file contents go to SQLite.
        *service.change_snapshot.lock().map_err(join_error)? = None;
        let snapshot = crate::changes::review(
            &service.registry,
            &service.git,
            project_id,
            &std::sync::atomic::AtomicBool::new(false),
        )?;
        let view = snapshot.view.clone();
        *service.change_snapshot.lock().map_err(join_error)? = Some(snapshot);
        Ok(view)
    })
    .await
    .map_err(join_error)?
}
#[derive(serde::Deserialize)]
pub struct DiffRequest {
    project_id: i64,
    token: String,
    path: String,
    staged: bool,
}
#[tauri::command]
pub async fn change_diff(
    request: DiffRequest,
    state: State<'_, AppState>,
) -> Result<crate::changes::Diff> {
    let service = service(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = service.admission.lock().map_err(join_error)?;
        service.operations.ensure_idle()?;
        let snapshot = reviewed(&service, request.project_id, &request.token)?;
        crate::changes::diff(
            &service.registry,
            &service.git,
            &snapshot,
            &request.path,
            request.staged,
            &std::sync::atomic::AtomicBool::new(false),
        )
    })
    .await
    .map_err(join_error)?
}
#[derive(Serialize)]
pub struct ChangeStatus {
    history: Vec<crate::changes::ChangeJob>,
    confirmation: Option<crate::changes::CommitPreview>,
}
#[tauri::command]
pub async fn change_status(project_id: i64, state: State<'_, AppState>) -> Result<ChangeStatus> {
    let service = service(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        let confirmation = service
            .commit_confirmation
            .lock()
            .map_err(join_error)?
            .as_ref()
            .filter(|(p, _)| p.job.project_id == project_id)
            .map(|(p, _)| crate::changes::CommitPreview {
                job: p.job.clone(),
                repository: p.repository.clone(),
                author: p.author.clone(),
                message: p.message.clone(),
                entries: p.entries.clone(),
            });
        Ok(ChangeStatus {
            history: service.registry.change_history(project_id)?,
            confirmation,
        })
    })
    .await
    .map_err(join_error)?
}
#[derive(serde::Deserialize)]
pub struct IndexRequest {
    project_id: i64,
    token: String,
    paths: Vec<String>,
    unstage: bool,
}
#[tauri::command]
pub fn start_index_change(
    request: IndexRequest,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Operation> {
    let service = service(&state)?;
    let (snapshot, operation, cancel) = {
        let _guard = service.admission.lock().map_err(join_error)?;
        let snapshot = reviewed(&service, request.project_id, &request.token)?;
        let (operation, cancel) =
            service
                .operations
                .start(if request.unstage { "unstage" } else { "stage" })?;
        *service.change_snapshot.lock().map_err(join_error)? = None;
        (snapshot, operation, cancel)
    };
    let id = operation.id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let outcome=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||crate::changes::mutate_index(&service.registry,&service.git,&snapshot,&request.paths,request.unstage,&cancel,&mut|message,percent|{
            let _=service.operations.progress(&id,ScanProgress{message:Some(message.into()),percent,..Default::default()});let _=app.emit("operation-progress",&id);
        }))).unwrap_or_else(|_|Err(AppError::new(ErrorCode::RecoveryRequired,"Index worker interrupted. Inspect index, HEAD, locks, and change history; refresh changes before retrying.")));
        let _ = service.operations.finish(&id, outcome);
        let _ = app.emit("operation-progress", &id);
    });
    Ok(operation)
}
#[derive(serde::Deserialize)]
pub struct CommitRequest {
    project_id: i64,
    token: String,
    message: String,
}
#[tauri::command]
pub fn start_commit(
    request: CommitRequest,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Operation> {
    let service = service(&state)?;
    let (snapshot, operation, cancel) = {
        let _guard = service.admission.lock().map_err(join_error)?;
        let snapshot = reviewed(&service, request.project_id, &request.token)?;
        let (operation, cancel) = service.operations.start("commit")?;
        *service.change_snapshot.lock().map_err(join_error)? = None;
        (snapshot, operation, cancel)
    };
    let id = operation.id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut prepared = None;
        let outcome=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||{
            prepared=Some(crate::changes::prepare_commit(&service.registry,&service.git,&snapshot,request.message,&cancel)?);
            let commit=prepared.as_mut().expect("prepared commit");let(tx,rx)=std::sync::mpsc::channel();
            *service.commit_confirmation.lock().map_err(join_error)?=Some((commit.preview(),tx));
            let mut report=|message:&str,percent:Option<u8>|{let _=service.operations.progress(&id,ScanProgress{message:Some(message.into()),percent,..Default::default()});let _=app.emit("operation-progress",&id);};
            report("Review staged files, message, and author in Changes. Workspace reserved.",None);
            wait_confirmation(&rx,&cancel)?;*service.commit_confirmation.lock().map_err(join_error)?=None;
            commit.execute(&service.registry,&service.git,&cancel,&mut report)
        })).unwrap_or_else(|_|Err(AppError::new(ErrorCode::RecoveryRequired,"Commit worker interrupted. Inspect HEAD, index, locks, and change history; no automatic retry occurred.")));
        if let Ok(mut p) = service.commit_confirmation.lock() {
            *p = None;
        }
        if let (Some(commit), Err(error)) = (prepared.as_mut(), &outcome) {
            if commit.job.state == "running" {
                let _ = commit.stop(&service.registry, error.clone());
            }
        }
        drop(prepared);
        let _ = service.operations.finish(&id, outcome);
        let _ = app.emit("operation-progress", &id);
    });
    Ok(operation)
}
#[tauri::command]
pub fn confirm_commit(id: String, confirmed: bool, state: State<'_, AppState>) -> Result<()> {
    let service = service(&state)?;
    let mut pending = service.commit_confirmation.lock().map_err(join_error)?;
    if pending
        .as_ref()
        .is_none_or(|(preview, _)| preview.job.id != id)
    {
        return Err(AppError::new(
            ErrorCode::RecoveryRequired,
            "Commit confirmation expired. Review again.",
        ));
    }
    pending
        .take()
        .expect("checked confirmation")
        .1
        .send(confirmed)
        .map_err(join_error)
}

#[tauri::command]
pub fn start_push(
    project_id: i64,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Operation> {
    let service = service(&state)?;
    let (operation, cancel) = {
        let _guard = service.admission.lock().map_err(join_error)?;
        service.operations.start("push")?
    };
    let id = operation.id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut prepared = None;
        let outcome=std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut report=|message:&str,percent:Option<u8>| {
                let _=service.operations.progress(&id,ScanProgress{message:Some(message.into()),percent,..Default::default()});
                let _=app.emit("operation-progress",&id);
            };
            prepared=Some(crate::push::prepare(&service.registry,&service.git,project_id,&cancel,&mut report)?);
            let push=prepared.as_mut().expect("prepared push");
            let(tx,rx)=std::sync::mpsc::channel();
            *service.push_confirmation.lock().map_err(join_error)?=Some((push.job.id.clone(),tx));
            report("Review exact push destination and commit in Sync details. Workspace reserved.",None);
            wait_confirmation(&rx,&cancel)?;
            *service.push_confirmation.lock().map_err(join_error)?=None;
            push.execute(&service.registry,&service.git,&cancel,&mut report)
        })).unwrap_or_else(|_|Err(AppError::new(ErrorCode::RecoveryRequired,"Push worker interrupted. Inspect the durable push history and exact remote branch before retrying.")));
        if let Ok(mut pending) = service.push_confirmation.lock() {
            *pending = None;
        }
        if let (Some(push), Err(error)) = (prepared.as_mut(), &outcome) {
            if push.job.state == "running" {
                let _ = push.stop(&service.registry, error.clone());
            }
        }
        drop(prepared);
        let _ = service.operations.finish(&id, outcome);
        let _ = app.emit("operation-progress", &id);
    });
    Ok(operation)
}
fn wait_confirmation(
    rx: &std::sync::mpsc::Receiver<bool>,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
    loop {
        check_cancel(cancel)?;
        match rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(true) => return Ok(()),
            Ok(false) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(AppError::new(
                    ErrorCode::Cancelled,
                    "Confirmation declined.",
                ))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if std::time::Instant::now() >= deadline {
                    return Err(AppError::new(
                        ErrorCode::Cancelled,
                        "Confirmation expired. Review again.",
                    ));
                }
            }
        }
    }
}
#[tauri::command]
pub fn confirm_push(id: String, confirmed: bool, state: State<'_, AppState>) -> Result<()> {
    let service = service(&state)?;
    let mut pending = service.push_confirmation.lock().map_err(join_error)?;
    if pending.as_ref().is_none_or(|(job, _)| job != &id) {
        return Err(AppError::new(
            ErrorCode::RecoveryRequired,
            "Push confirmation expired. Review again.",
        ));
    }
    pending
        .take()
        .expect("checked confirmation")
        .1
        .send(confirmed)
        .map_err(join_error)
}

#[tauri::command]
pub async fn repository_library(
    account_id: Option<i64>,
    state: State<'_, AppState>,
) -> Result<crate::library::Library> {
    let service = service(&state)?;
    tauri::async_runtime::spawn_blocking(move || service.registry.library(account_id))
        .await
        .map_err(join_error)?
}
#[tauri::command]
pub async fn github_status(state: State<'_, AppState>) -> Result<crate::github::AuthStatus> {
    let _ = service(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        crate::github::Github::default().auth(&std::sync::atomic::AtomicBool::new(false))
    })
    .await
    .map_err(join_error)
}
#[tauri::command]
pub async fn github_login(state: State<'_, AppState>) -> Result<()> {
    let service = service(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = service.admission.lock().map_err(join_error)?;
        service.operations.ensure_idle()?;
        crate::github::Github::default().login()
    })
    .await
    .map_err(join_error)?
}
#[tauri::command]
pub async fn open_github_repository(
    id: i64,
    account_id: Option<i64>,
    state: State<'_, AppState>,
) -> Result<()> {
    let service = service(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        let library = service.registry.library(account_id)?;
        let repo = library
            .cache
            .repositories
            .iter()
            .find(|r| r.repository.id == id)
            .ok_or_else(|| {
                AppError::new(ErrorCode::Missing, "Cached repository no longer exists.")
            })?;
        crate::github::open_repository(&repo.repository)
    })
    .await
    .map_err(join_error)?
}
#[tauri::command]
pub fn start_github_refresh(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Operation> {
    let service = service(&state)?;
    let (operation, cancel) = {
        let _guard = service.admission.lock().map_err(join_error)?;
        service.operations.start("github_refresh")?
    };
    let id = operation.id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut last_event = std::time::Instant::now();
            let snapshot =
                crate::github::Github::default().fetch(&cancel, |pages, repositories| {
                    let progress = ScanProgress {
                        visited: pages,
                        registered: repositories,
                        ..Default::default()
                    };
                    let _ = service.operations.progress(&id, progress);
                    if last_event.elapsed() >= std::time::Duration::from_millis(250) {
                        let _ = app.emit("operation-progress", &id);
                        last_event = std::time::Instant::now();
                    }
                })?;
            service.registry.apply_github(&snapshot, &cancel)
        }))
        .unwrap_or_else(|_| {
            Err(AppError::new(
                ErrorCode::Internal,
                "GitHub worker failed. Previous cache retained.",
            ))
        });
        if let Err(error) = &outcome {
            let _ = service.registry.github_failed(error);
        }
        let _ = service.operations.finish(&id, outcome);
        let _ = app.emit("operation-progress", &id);
    });
    Ok(operation)
}

#[derive(Serialize)]
pub struct WorkspaceSettings {
    preferences: crate::clone::Preferences,
    editors: Vec<crate::editor::Editor>,
    jobs: Vec<crate::clone::CloneJob>,
}
#[tauri::command]
pub async fn workspace_settings(state: State<'_, AppState>) -> Result<WorkspaceSettings> {
    let service = service(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        Ok(WorkspaceSettings {
            preferences: service.registry.preferences()?,
            editors: crate::editor::installed(),
            jobs: service.registry.clone_jobs()?,
        })
    })
    .await
    .map_err(join_error)?
}
#[tauri::command]
pub async fn save_workspace_preferences(
    preferences: crate::clone::Preferences,
    state: State<'_, AppState>,
) -> Result<()> {
    let service = service(&state)?;
    tauri::async_runtime::spawn_blocking(move || service.registry.save_preferences(&preferences))
        .await
        .map_err(join_error)?
}
#[tauri::command]
pub async fn validate_clone_destination(
    root: String,
    name: String,
) -> Result<crate::clone::Destination> {
    tauri::async_runtime::spawn_blocking(move || crate::clone::validate_destination(&root, &name))
        .await
        .map_err(join_error)?
}
#[tauri::command]
pub async fn open_project(id: i64, reveal: bool, state: State<'_, AppState>) -> Result<()> {
    let service = service(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        let project = service.registry.get(id)?;
        let editor = if reveal {
            "finder".into()
        } else {
            service.registry.preferences()?.editor
        };
        crate::editor::open_workspace(std::path::Path::new(&project.path), &editor)
    })
    .await
    .map_err(join_error)?
}
#[tauri::command]
pub async fn reveal_clone(id: String, state: State<'_, AppState>) -> Result<()> {
    let service = service(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        let job = service.registry.clone_job(&id)?;
        crate::editor::open_workspace(&crate::clone::review_path(&job)?, "finder")
    })
    .await
    .map_err(join_error)?
}
#[derive(serde::Deserialize)]
pub struct CloneRequest {
    pub repository_id: i64,
    pub account_id: Option<i64>,
    pub root: String,
    pub name: String,
    pub open_after: bool,
}
#[tauri::command]
pub fn start_clone(
    request: CloneRequest,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Operation> {
    clone_operation(service(&state)?, app, Some(request), None, false)
}
#[tauri::command]
pub fn start_clone_recovery(
    id: String,
    open_after: bool,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Operation> {
    clone_operation(service(&state)?, app, None, Some(id), open_after)
}
fn clone_operation(
    service: Arc<Service>,
    app: tauri::AppHandle,
    request: Option<CloneRequest>,
    recovery: Option<String>,
    open_after: bool,
) -> Result<Operation> {
    let (operation, cancel) = {
        let _guard = service.admission.lock().map_err(join_error)?;
        service.operations.start(if recovery.is_some() {
            "clone_recovery"
        } else {
            "clone"
        })?
    };
    let id = operation.id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let outcome=std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut progress=ScanProgress::default();let mut last_event=std::time::Instant::now();
            let mut report=|message:&str,percent:Option<u8>| {
                progress.message=Some(message.into());progress.percent=percent;
                let _=service.operations.progress(&id,progress.clone());
                if last_event.elapsed()>=std::time::Duration::from_millis(250){let _=app.emit("operation-progress",&id);last_event=std::time::Instant::now();}
            };
            let should_open=request.as_ref().map(|r|r.open_after).unwrap_or(open_after);
            let mut job=if let Some(request)=request {
                let library=service.registry.library(request.account_id)?;
                let repo=library.cache.repositories.iter().find(|r|r.repository.id==request.repository_id).ok_or_else(||AppError::new(ErrorCode::Missing,"Refresh the GitHub library before cloning this repository."))?;
                crate::clone::execute(&service.registry,&service.git,&repo.repository,&request.root,&request.name,&cancel,&mut report)?
            }else{crate::clone::recover(&service.registry,&service.git,&recovery.expect("recovery id"),&cancel,&mut report)?};
            if should_open {
                let opened=service.registry.preferences().and_then(|preferences|crate::editor::open_workspace(std::path::Path::new(&job.destination),&preferences.editor));
                if let Err(error)=opened {
                    job.editor_error=Some(error.clone());let _=service.registry.save_clone(&job);
                    progress.issues.push(ScanIssue {path:job.destination,error});
                    progress.message=Some("Clone succeeded. Editor handoff failed; use Open or Reveal in Finder.".into());
                }
            }
            progress.registered=1;service.operations.progress(&id,progress)?;Ok(())
        })).unwrap_or_else(|_|Err(AppError::new(ErrorCode::Internal,"Clone worker stopped unexpectedly. Review the retained clone journal before retrying.")));
        let _ = service.operations.finish(&id, outcome);
        let _ = app.emit("operation-progress", &id);
    });
    Ok(operation)
}

#[tauri::command]
pub async fn sync_status(id: i64, state: State<'_, AppState>) -> Result<crate::sync::SyncView> {
    let service = service(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        service.registry.get(id)?;
        Ok(crate::sync::SyncView {
            record: service.registry.sync_record(id)?,
            history: service.registry.fetch_history(id)?,
            pulls: service.registry.pull_history(id)?,
            pushes: service.registry.push_history(id)?,
        })
    })
    .await
    .map_err(join_error)?
}
#[tauri::command]
pub fn start_sync_inspection(
    id: i64,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Operation> {
    sync_operation(service(&state)?, app, id, None)
}
#[derive(serde::Deserialize)]
pub struct FetchRequest {
    pub project_id: i64,
    pub token: String,
    pub remote: String,
}
#[tauri::command]
pub fn start_fetch(
    request: FetchRequest,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Operation> {
    if request.token.len() != 32 || request.remote.len() > 100 {
        return Err(AppError::new(
            ErrorCode::Unsupported,
            "Inspect sync and select a supported remote before fetching.",
        ));
    }
    sync_operation(service(&state)?, app, request.project_id, Some(request))
}
fn sync_operation(
    service: Arc<Service>,
    app: tauri::AppHandle,
    project_id: i64,
    fetch: Option<FetchRequest>,
) -> Result<Operation> {
    let (operation, cancel) = {
        let _guard = service.admission.lock().map_err(join_error)?;
        service.operations.start(if fetch.is_some() {
            "fetch"
        } else {
            "sync_inspection"
        })?
    };
    let id = operation.id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut progress = ScanProgress::default();
            let mut last_event = std::time::Instant::now();
            let mut report = |message: &str, percent: Option<u8>| {
                progress.message = Some(message.into());
                progress.percent = percent;
                let _ = service.operations.progress(&id, progress.clone());
                if last_event.elapsed() >= std::time::Duration::from_millis(250) {
                    let _ = app.emit("operation-progress", &id);
                    last_event = std::time::Instant::now();
                }
            };
            if let Some(request) = fetch {
                crate::sync::fetch(&service.registry, &service.git, project_id,
                    &request.token, &request.remote, &cancel, &mut report)?;
            } else {
                report("Inspecting local synchronization state; no network request", None);
                crate::sync::inspect(&service.registry, &service.git, project_id, &cancel)?;
                report("Local synchronization inspection completed", None);
            }
            Ok(())
        })).unwrap_or_else(|_| Err(AppError::new(ErrorCode::Internal,
            "Sync worker stopped unexpectedly. Inspect sync and review persisted fetch history; no automatic retry is performed.")));
        let _ = service.operations.finish(&id, outcome);
        let _ = app.emit("operation-progress", &id);
    });
    Ok(operation)
}

#[derive(serde::Deserialize)]
pub struct PullRequest {
    pub project_id: i64,
    pub token: String,
}
#[tauri::command]
pub fn start_pull(
    request: PullRequest,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Operation> {
    if request.token.len() != 32 {
        return Err(AppError::new(
            ErrorCode::Unsupported,
            "Inspect sync before pulling.",
        ));
    }
    let service = service(&state)?;
    let (operation, cancel) = {
        let _guard = service.admission.lock().map_err(join_error)?;
        service.operations.start("pull")?
    };
    let id = operation.id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut prepared = None;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            prepared = Some(crate::pull::prepare(&service.registry, &service.git, request.project_id, &request.token, &cancel)?);
            let pull = prepared.as_mut().expect("prepared pull");
            let (tx, rx) = std::sync::mpsc::channel();
            *service.pull_confirmation.lock().map_err(join_error)? = Some((pull.job.id.clone(), tx));
            service.operations.progress(&id, ScanProgress { message: Some("Review pull confirmation in Sync details. Workspace reserved.".into()), ..Default::default() })?;
            let _ = app.emit("operation-progress", &id);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
            loop {
                check_cancel(&cancel)?;
                match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                    Ok(true) => break,
                    Ok(false) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Err(AppError::new(ErrorCode::Cancelled, "Pull confirmation declined.")),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if std::time::Instant::now() >= deadline { return Err(AppError::new(ErrorCode::Cancelled, "Pull confirmation expired. Inspect and review again.")); }
                    }
                }
            }
            *service.pull_confirmation.lock().map_err(join_error)? = None;
            let mut progress = ScanProgress::default();
            pull.execute(&service.registry, &service.git, &cancel, &mut |message, percent| {
                progress.message = Some(message.into()); progress.percent = percent;
                let _ = service.operations.progress(&id, progress.clone());
                let _ = app.emit("operation-progress", &id);
            })
        })).unwrap_or_else(|_| Err(AppError::new(ErrorCode::RecoveryRequired, "Pull worker interrupted. Inspect the workspace and durable pull history before retrying.")));
        if let Ok(mut pending) = service.pull_confirmation.lock() {
            *pending = None;
        }
        if let (Some(pull), Err(error)) = (prepared.as_mut(), &outcome) {
            // Covers confirmation cancellation and panics as well as execution errors.
            let _ = pull.stop(&service.registry, error.clone());
        }
        drop(prepared); // release workspace locks before admitting the next operation
        let _ = service.operations.finish(&id, outcome);
        let _ = app.emit("operation-progress", &id);
    });
    Ok(operation)
}
#[tauri::command]
pub fn confirm_pull(id: String, confirmed: bool, state: State<'_, AppState>) -> Result<()> {
    let service = service(&state)?;
    let mut pending = service.pull_confirmation.lock().map_err(join_error)?;
    if pending.as_ref().is_none_or(|(job, _)| job != &id) {
        return Err(AppError::new(
            ErrorCode::RecoveryRequired,
            "Pull confirmation expired. Review again.",
        ));
    }
    let (_, sender) = pending.take().expect("checked confirmation");
    sender.send(confirmed).map_err(|_| {
        AppError::new(
            ErrorCode::RecoveryRequired,
            "Pull confirmation expired. Review again.",
        )
    })
}
