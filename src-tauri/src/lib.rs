pub mod core;
pub mod database;
pub mod git;
pub mod github;
#[cfg(feature = "desktop")]
pub mod ipc;
pub mod library;
pub mod operations;
#[cfg(feature = "desktop")]
pub mod platform;
pub mod projects;

pub mod clone;
pub mod editor;
pub mod safe_fs;

mod git_process;
pub mod sync;

pub mod pull;
