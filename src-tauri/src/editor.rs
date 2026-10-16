use crate::{
    core::{AppError, ErrorCode, Result},
    safe_fs::Directory,
};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
#[derive(Serialize)]
pub struct Editor {
    pub id: String,
    pub name: String,
    pub available: bool,
}
pub fn validate_editor(id: &str) -> Result<()> {
    if ["code", "cursor", "finder"].contains(&id) {
        Ok(())
    } else {
        Err(AppError::new(
            ErrorCode::EditorUnavailable,
            "Choose VS Code, Cursor, or Finder.",
        ))
    }
}
fn executable(id: &str) -> Result<Option<PathBuf>> {
    validate_editor(id)?;
    if id == "finder" {
        return Ok(Some(PathBuf::from("/usr/bin/open")));
    }
    let (bundle, cli) = if id == "code" {
        ("Visual Studio Code.app", "code")
    } else {
        ("Cursor.app", "cursor")
    };
    let mut locations = vec![PathBuf::from("/Applications")];
    if let Some(home) = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        locations.push(home.join("Applications"));
    }
    Ok(locations
        .into_iter()
        .map(|p| p.join(bundle).join("Contents/Resources/app/bin").join(cli))
        .find(|p| p.is_file()))
}
pub fn installed() -> Vec<Editor> {
    [
        ("code", "VS Code"),
        ("cursor", "Cursor"),
        ("finder", "Reveal in Finder"),
    ]
    .into_iter()
    .map(|(id, name)| Editor {
        id: id.into(),
        name: name.into(),
        available: cfg!(target_os = "macos") && executable(id).ok().flatten().is_some(),
    })
    .collect()
}
pub fn open_workspace(path: &Path, id: &str) -> Result<()> {
    if !cfg!(target_os = "macos") {
        return Err(AppError::new(
            ErrorCode::Unsupported,
            "Editor launching is currently supported on macOS only.",
        ));
    }
    let directory = Directory::open(path)?;
    directory.validate_path()?;
    let executable=executable(id)?.ok_or_else(||AppError::new(ErrorCode::EditorUnavailable,"Preferred editor was not found in Applications. Choose another editor or Reveal in Finder. The workspace remains registered."))?;
    let mut command = Command::new(executable);
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .current_dir("/");
    if let Some(home) = std::env::var_os("HOME") {
        command.env("HOME", home);
    }
    if id == "finder" {
        command.arg("-R");
    } else {
        command.args(["--new-window", "--disable-extensions"]);
    }
    command.arg(&directory.path);
    launch(command)
}
fn launch(mut command: Command) -> Result<()> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| {
            AppError::new(
                ErrorCode::EditorFailed,
                "Editor/Finder could not start. The workspace is preserved; try Reveal in Finder.",
            )
        })?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err(AppError::new(ErrorCode::EditorFailed,"Editor/Finder handoff failed. The workspace is preserved; choose another editor or reveal it in Finder."))
                }
            }
            Ok(None) if start.elapsed() < Duration::from_secs(15) => {
                thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AppError::new(
                    ErrorCode::EditorFailed,
                    "Editor/Finder handoff timed out. The workspace is preserved.",
                ));
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launch_success_failure_and_missing() {
        assert!(launch(Command::new("/usr/bin/true")).is_ok());
        assert_eq!(
            launch(Command::new("/usr/bin/false")).unwrap_err().code,
            ErrorCode::EditorFailed
        );
        assert!(launch(Command::new("/not-an-editor")).is_err());
        assert!(validate_editor("sh -c anything").is_err());
    }
    #[test]
    fn symlink_workspaces_rejected() {
        let t = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(t.path(), t.path().join("alias")).unwrap();
        assert!(open_workspace(&t.path().join("alias"), "finder").is_err());
    }
}
