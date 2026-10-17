//! Shared registration/descriptor admission for new mutation policies.
use crate::{
    core::{AppError, ErrorCode, Result},
    database::Registry,
    git::Git,
    safe_fs::Directory,
    sync,
};
use std::{path::Path, sync::atomic::AtomicBool};

pub(crate) struct LockedWorkspace {
    pub root: Directory,
    pub meta: Directory,
}
impl LockedWorkspace {
    pub fn open(db: &Registry, id: i64) -> Result<Self> {
        let project = db.get(id)?;
        let saved = db
            .sync_record(id)?
            .assessment
            .and_then(|a| a.identity)
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::Unsupported,
                    "Inspect sync to verify this registered workspace first.",
                )
            })?;
        let root = Directory::open(Path::new(&project.path))?;
        let meta = root.child(".git")?;
        if saved
            != (sync::WorkspaceIdentity {
                root: root.identity()?,
                git: meta.identity()?,
            })
        {
            return Err(crate::safe_fs::changed());
        }
        root.lock()?;
        meta.lock()?;
        let result = Self { root, meta };
        result.validate()?;
        Ok(result)
    }
    pub fn validate(&self) -> Result<()> {
        sync::checked_layout(&self.root, &self.meta)
    }
    pub fn branch(&self, git: &Git, cancel: &AtomicBool) -> Result<(String, String)> {
        self.validate()?;
        let a = sync::assess(git, &self.root.path, cancel)?;
        let branch = a.inspection.branch.ok_or_else(|| {
            AppError::new(
                ErrorCode::Unsupported,
                "An ordinary checked-out branch is required.",
            )
        })?;
        let head = a.inspection.commit.ok_or_else(|| {
            AppError::new(
                ErrorCode::Unsupported,
                "A branch with an existing commit is required.",
            )
        })?;
        if !sync::oid(&head)
            || a.relation == "unsupported"
            || sync::output(
                git,
                &self.root.path,
                &["symbolic-ref", "--no-recurse", "HEAD"],
                cancel,
            )? != format!("refs/heads/{branch}")
        {
            return Err(AppError::new(
                ErrorCode::Unsupported,
                "Redirected HEAD or unsupported history cannot be mutated.",
            ));
        }
        Ok((branch, head))
    }
}

pub(crate) fn reject_hooks(meta: &Path) -> Result<()> {
    match std::fs::read_dir(meta.join("hooks")) {
        Ok(entries) => {
            for entry in entries {
                if !entry
                    .map_err(AppError::io)?
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".sample")
                {
                    return Err(AppError::new(ErrorCode::Unsupported, "Non-sample Git hooks are unsupported. Review this operation in your terminal; Stow will not execute or remove hooks."));
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(AppError::io(e)),
    }
    Ok(())
}
