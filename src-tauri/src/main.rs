#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::{Arc, Mutex};
use stow_core::{
    database::Registry,
    git::Git,
    ipc::{self, AppState, Service},
    operations::Operations,
    platform,
};
use tauri::Manager;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let service = platform::database_path(app.handle())
                .and_then(|path| Registry::open(&path))
                .and_then(|registry| {
                    registry.reconcile_pulls()?;
                    registry.reconcile_pushes()?;
                    registry.reconcile_changes()?;
                    Ok(registry)
                })
                .map(|registry| {
                    Arc::new(Service {
                        registry,
                        git: Git::default(),
                        operations: Operations::default(),
                        admission: Mutex::new(()),
                        pull_confirmation: Mutex::new(None),
                        push_confirmation: Mutex::new(None),
                        change_snapshot: Mutex::new(None),
                        commit_confirmation: Mutex::new(None),
                    })
                });
            // Keep the window usable so database failures can be reported in the UI.
            app.manage(AppState(service));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ipc::sync_status,
            ipc::start_sync_inspection,
            ipc::start_fetch,
            ipc::start_pull,
            ipc::confirm_pull,
            ipc::start_push,
            ipc::confirm_push,
            ipc::review_changes,
            ipc::change_diff,
            ipc::change_status,
            ipc::start_index_change,
            ipc::start_commit,
            ipc::confirm_commit,
            ipc::workspace_settings,
            ipc::save_workspace_preferences,
            ipc::validate_clone_destination,
            ipc::start_clone,
            ipc::start_clone_recovery,
            ipc::open_project,
            ipc::reveal_clone,
            ipc::system_info,
            ipc::list_projects,
            ipc::list_operations,
            ipc::start_scan,
            ipc::start_inspection,
            ipc::cancel_operation,
            ipc::remove_project,
            ipc::github_status,
            ipc::github_login,
            ipc::start_github_refresh,
            ipc::repository_library,
            ipc::open_github_repository
        ])
        .run(tauri::generate_context!())
        .expect("Stow could not start its desktop runtime");
}
