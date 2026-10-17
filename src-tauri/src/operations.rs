use crate::core::{now, AppError, ErrorCode, Result};
use crate::projects::ScanProgress;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Running,
    Completed,
    Failed,
    Cancelled,
    RequiresInspection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Operation {
    pub id: String,
    pub kind: String,
    pub state: OperationState,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    pub progress: ScanProgress,
    pub error: Option<AppError>,
}

#[derive(Default)]
pub struct Operations {
    next: AtomicU64,
    entries: Mutex<VecDeque<(Operation, Arc<AtomicBool>)>>,
}

impl Operations {
    pub fn start(&self, kind: &str) -> Result<(Operation, Arc<AtomicBool>)> {
        let mut entries = self.entries.lock().map_err(lock_error)?;
        if entries
            .iter()
            .any(|(op, _)| op.state == OperationState::Running)
        {
            return Err(AppError::new(
                ErrorCode::Busy,
                "An operation is already running. Wait or cancel it first.",
            ));
        }
        let id = format!("{}-{}", now(), self.next.fetch_add(1, Ordering::Relaxed));
        let operation = Operation {
            id,
            kind: kind.to_owned(),
            state: OperationState::Running,
            started_at: now(),
            completed_at: None,
            progress: ScanProgress::default(),
            error: None,
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        if entries.len() >= 100 {
            entries.pop_front();
        }
        entries.push_back((operation.clone(), cancelled.clone()));
        Ok((operation, cancelled))
    }

    pub fn list(&self) -> Result<Vec<Operation>> {
        Ok(self
            .entries
            .lock()
            .map_err(lock_error)?
            .iter()
            .rev()
            .map(|(op, _)| op.clone())
            .collect())
    }

    pub fn cancel(&self, id: &str) -> Result<()> {
        let entries = self.entries.lock().map_err(lock_error)?;
        if let Some((op, flag)) = entries.iter().find(|(op, _)| op.id == id) {
            if op.state == OperationState::Running {
                flag.store(true, Ordering::Relaxed);
            }
            Ok(())
        } else {
            Err(AppError::new(
                ErrorCode::Missing,
                "Operation no longer exists.",
            ))
        }
    }

    pub fn progress(&self, id: &str, progress: ScanProgress) -> Result<()> {
        if let Some((op, _)) = self
            .entries
            .lock()
            .map_err(lock_error)?
            .iter_mut()
            .find(|(op, _)| op.id == id)
        {
            op.progress = progress;
        }
        Ok(())
    }

    pub fn finish(&self, id: &str, outcome: Result<()>) -> Result<()> {
        if let Some((op, _)) = self
            .entries
            .lock()
            .map_err(lock_error)?
            .iter_mut()
            .find(|(op, _)| op.id == id)
        {
            op.completed_at = Some(now());
            match outcome {
                Ok(()) => op.state = OperationState::Completed,
                Err(error) => {
                    op.state = if error.code == ErrorCode::RecoveryRequired {
                        OperationState::RequiresInspection
                    } else if error.code == ErrorCode::Cancelled {
                        OperationState::Cancelled
                    } else {
                        OperationState::Failed
                    };
                    op.error = Some(error);
                }
            }
        }
        Ok(())
    }

    pub fn ensure_idle(&self) -> Result<()> {
        if self
            .list()?
            .iter()
            .any(|op| op.state == OperationState::Running)
        {
            Err(AppError::new(
                ErrorCode::Busy,
                "Wait for the current operation before removing a registration.",
            ))
        } else {
            Ok(())
        }
    }
}
fn lock_error<T>(_: T) -> AppError {
    AppError::new(
        ErrorCode::Internal,
        "Operation state is unavailable. Restart Stow.",
    )
}
