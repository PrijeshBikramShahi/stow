use crate::core::{AppError, ErrorCode, Result};
use std::path::PathBuf;
use tauri::Manager;

pub fn database_path(app: &tauri::AppHandle) -> Result<PathBuf> {
    let directory = app.path().app_data_dir().map_err(|_| {
        AppError::new(
            ErrorCode::Database,
            "Application data directory is unavailable.",
        )
    })?;
    std::fs::create_dir_all(&directory).map_err(AppError::io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            .map_err(AppError::io)?;
    }
    Ok(directory.join("registry.sqlite3"))
}
