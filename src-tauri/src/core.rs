use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidPath,
    PermissionDenied,
    Missing,
    Unsupported,
    GitUnavailable,
    GitFailed,
    GithubUnavailable,
    Authentication,
    Network,
    RateLimited,
    ProviderAccess,
    ProviderFailed,
    ProviderData,
    LoginHandoff,
    Collision,
    CloneFailed,
    FetchFailed,
    RecoveryRequired,
    EditorUnavailable,
    EditorFailed,
    Database,
    Cancelled,
    Timeout,
    LimitReached,
    Busy,
    Internal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
}
impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    pub fn io(error: std::io::Error) -> Self {
        let code = match error.kind() {
            std::io::ErrorKind::NotFound => ErrorCode::Missing,
            std::io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
            _ => ErrorCode::InvalidPath,
        };
        Self::new(
            code,
            format!("Filesystem access failed ({:?}).", error.kind()),
        )
    }
}
impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for AppError {}
pub type Result<T> = std::result::Result<T, AppError>;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkingTree {
    pub staged: u64,
    pub unstaged: u64,
    pub untracked: u64,
    pub conflicted: u64,
}
impl WorkingTree {
    pub fn is_clean(&self) -> bool {
        self.staged + self.unstaged + self.untracked + self.conflicted == 0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Remote {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Inspection {
    pub branch: Option<String>,
    pub detached: bool,
    pub commit: Option<String>,
    pub upstream: Option<String>,
    pub remotes: Vec<Remote>,
    pub working_tree: WorkingTree,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: i64,
    pub name: String,
    pub path: String,
    pub registered_at: i64,
    pub inspected_at: Option<i64>,
    pub availability: String,
    pub inspection: Option<Inspection>,
    pub error: Option<AppError>,
}

pub fn path_string(path: &std::path::Path) -> Result<String> {
    path.to_str()
        .filter(|p| !p.contains('\0'))
        .map(str::to_owned)
        .ok_or_else(|| {
            AppError::new(
                ErrorCode::InvalidPath,
                "Paths must be valid UTF-8 and contain no NUL characters.",
            )
        })
}
