//! Bounded process-group supervision shared by separate creation/fetch policies.
use crate::{
    core::{AppError, ErrorCode, Result},
    git::check_cancel,
};
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
fn error(code: ErrorCode, message: &str) -> AppError {
    AppError::new(code, message)
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn supervise(
    mut command: Command,
    cancel: &AtomicBool,
    deadline: Instant,
    allow_missing: bool,
    progress: &mut dyn FnMut(&str, Option<u8>),
    input: Option<Vec<u8>>,
    failure_code: ErrorCode,
) -> Result<(bool, Vec<u8>)> {
    let mut child = command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| {
            error(
                failure_code.clone(),
                "Git transfer process could not start. Check the Git installation.",
            )
        })?;
    let writer = input.map(|bytes| {
        let mut pipe = child.stdin.take().expect("piped input");
        thread::spawn(move || pipe.write_all(&bytes))
    });
    let overflow = Arc::new(AtomicBool::new(false));
    let percent = Arc::new(AtomicU8::new(255));
    let stdout = child.stdout.take().expect("piped output");
    let stderr = child.stderr.take().expect("piped errors");
    let flag = overflow.clone();
    let out = thread::spawn(move || drain(stdout, flag, None));
    let flag = overflow.clone();
    let pct = percent.clone();
    let err = thread::spawn(move || drain(stderr, flag, Some(pct)));
    let mut last_percent = 255;
    let result = loop {
        let problem = if cancel.load(Ordering::Relaxed) {
            Some(error(
                ErrorCode::Cancelled,
                "Git transfer cancelled. Staging data is retained for review.",
            ))
        } else if overflow.load(Ordering::Relaxed) {
            Some(error(
                ErrorCode::LimitReached,
                "Git transfer output exceeded 8 MiB per stream. Staging data is retained.",
            ))
        } else if Instant::now() > deadline {
            Some(error(
                ErrorCode::Timeout,
                "Git transfer exceeded its 15-minute operation deadline. Staging data is retained.",
            ))
        } else {
            None
        };
        if let Some(e) = problem {
            break Err(e);
        }
        let p = percent.load(Ordering::Relaxed);
        if p != 255 && p != last_percent {
            progress("Receiving Git objects (transfer phase)", Some(p));
            last_percent = p;
        }
        match exited_without_reaping(child.id()) {
            Ok(true) => break Ok(()),
            Ok(false) => thread::sleep(Duration::from_millis(20)),
            Err(_) => {
                break Err(error(
                    failure_code.clone(),
                    "Git process supervision failed. Staging data is retained.",
                ))
            }
        }
    };
    // Kill the entire dedicated process group, including transport/helper children,
    // before joining pipe readers. Keep the leader unreaped until after kill,
    // preventing PID/process-group reuse from targeting an unrelated process.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let status = child
        .wait()
        .map_err(|_| error(failure_code.clone(), "Git process could not be reaped."));
    let bytes = out
        .join()
        .map_err(|_| error(ErrorCode::Internal, "Git transfer output reader failed."))??;
    let stderr = err
        .join()
        .map_err(|_| error(ErrorCode::Internal, "Git transfer error reader failed."))??;
    let written = writer.map(|writer| writer.join());
    result?;
    let status = status?;
    check_cancel(cancel)?;
    if overflow.load(Ordering::Relaxed) {
        return Err(error(
            ErrorCode::LimitReached,
            "Git transfer output exceeded the bounded limit. Data retained.",
        ));
    }
    if !status.success() && !(allow_missing && status.code() == Some(1)) {
        if failure_code == ErrorCode::FetchFailed {
            let diagnostic = String::from_utf8_lossy(&stderr).to_ascii_lowercase();
            if diagnostic.contains("authentication failed")
                || diagnostic.contains("could not read username")
                || diagnostic.contains("invalid username or token")
            {
                return Err(error(ErrorCode::Authentication,"Fetch authentication failed. Recheck the saved GitHub CLI login and repository access."));
            }
            if diagnostic.contains("could not resolve host")
                || diagnostic.contains("failed to connect")
            {
                return Err(error(
                    ErrorCode::Network,
                    "Fetch could not reach the remote. Check connectivity and retry explicitly.",
                ));
            }
            if diagnostic.contains("non-fast-forward") {
                return Err(error(ErrorCode::FetchFailed,"Remote history rewound or diverged from cached tracking refs. No force update was applied; review the history in your terminal."));
            }
        }
        return Err(error(failure_code.clone(),"Git transfer step failed. Check GitHub login, network access, disk space, and repository availability. Retained staging data can be reviewed; retry uses a new staging directory."));
    }
    if let Some(written) = written {
        written
            .map_err(|_| error(ErrorCode::Internal, "Git input writer failed."))?
            .map_err(|_| {
                error(
                    failure_code,
                    "Git did not receive the complete transaction. Inspect before retrying.",
                )
            })?;
    }
    Ok((status.success(), bytes))
}
fn exited_without_reaping(pid: u32) -> std::io::Result<bool> {
    // SAFETY: zero-initialized siginfo is writable; WNOWAIT keeps our child/PID
    // reserved until Child::wait after group termination. No other reaper is used.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { info.si_pid() } != 0)
}

pub(crate) fn drain(
    mut input: impl Read,
    overflow: Arc<AtomicBool>,
    percent: Option<Arc<AtomicU8>>,
) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut line = Vec::new();
    loop {
        let n = input
            .read(&mut buffer)
            .map_err(|_| error(ErrorCode::GitFailed, "Git output could not be read."))?;
        if n == 0 {
            break;
        }
        let remaining = (8 * 1024 * 1024usize).saturating_sub(output.len());
        output.extend_from_slice(&buffer[..n.min(remaining)]);
        if n > remaining {
            overflow.store(true, Ordering::Relaxed);
        }
        if let Some(pct) = &percent {
            for byte in &buffer[..n] {
                if *byte == b'\r' || *byte == b'\n' {
                    if let Ok(s) = std::str::from_utf8(&line) {
                        if let Some(rest) = s.strip_prefix("Receiving objects:") {
                            if let Some((n, _)) = rest.trim().split_once('%') {
                                if let Ok(value) = n.trim().parse::<u8>() {
                                    if value <= 100 {
                                        pct.store(value, Ordering::Relaxed);
                                    }
                                }
                            }
                        }
                    }
                    line.clear();
                } else if line.len() < 1024 {
                    line.push(*byte);
                }
            }
        }
    }
    Ok(output)
}
