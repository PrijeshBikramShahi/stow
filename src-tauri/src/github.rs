//! GitHub.com metadata only. No Git execution, token access, or shell IPC.
use crate::core::{now, AppError, ErrorCode, Result};
use crate::git::check_cancel;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    io::Read,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

const CANDIDATES: &[&str] = &[
    "/opt/homebrew/bin/gh",
    "/usr/local/bin/gh",
    "/opt/local/bin/gh",
    "/usr/bin/gh",
];
const OUTPUT_LIMIT: usize = 32 * 1024 * 1024;
pub const MAX_REPOSITORIES: usize = 10_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthState {
    CliUnavailable,
    NotAuthenticated,
    Authenticated,
    Invalid,
    Uncertain,
    NetworkUnavailable,
    Unsupported,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthStatus {
    pub cli_version: Option<String>,
    pub state: AuthState,
    pub login: Option<String>,
    pub known_accounts: usize,
    pub unsupported_hosts: bool,
    pub checked_at: i64,
    pub message: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Account {
    pub id: i64,
    pub login: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    pub id: i64,
    pub owner: String,
    pub name: String,
    pub full_name: String,
    pub description: Option<String>,
    pub web_url: String,
    pub https_url: String,
    pub ssh_url: String,
    pub visibility: String,
    pub fork: bool,
    pub archived: bool,
    pub default_branch: Option<String>,
    pub pushed_at: Option<String>,
    pub updated_at: String,
}
#[derive(Debug)]
pub struct Snapshot {
    pub account: Account,
    pub repositories: Vec<Repository>,
}

#[derive(Clone)]
pub struct Github {
    executable: PathBuf,
    timeout: Duration,
    output_limit: usize,
}
impl Default for Github {
    fn default() -> Self {
        Self {
            executable: CANDIDATES
                .iter()
                .map(PathBuf::from)
                .find(|p| p.is_file())
                .unwrap_or_else(|| PathBuf::from(CANDIDATES[0])),
            timeout: Duration::from_secs(120),
            output_limit: OUTPUT_LIMIT,
        }
    }
}
struct Output {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}
fn failure(code: ErrorCode, message: &str) -> AppError {
    AppError::new(code, message)
}
fn malformed() -> AppError {
    failure(
        ErrorCode::ProviderData,
        "GitHub returned unsupported or malformed data. The previous cache is preserved.",
    )
}

impl Github {
    // Private vocabulary: callers cannot supply commands, arguments, or environment.
    fn run(&self, args: &[&str], cancel: &AtomicBool, timeout: Duration) -> Result<Output> {
        let mut command = Command::new(&self.executable);
        command
            .args(args)
            .current_dir("/")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("GH_NO_EXTENSION_UPDATE_NOTIFIER", "1")
            .env("GH_PAGER", "cat")
            .env("NO_COLOR", "1")
            .env("GH_HOST", "github.com");
        // Only gh reads its own credential store. Never inherit tokens, debug flags,
        // repository context, proxy commands, pagers, or arbitrary PATH entries.
        for key in ["HOME", "GH_CONFIG_DIR", "XDG_CONFIG_HOME"] {
            if let Some(value) = std::env::var_os(key).filter(|v| PathBuf::from(v).is_absolute()) {
                command.env(key, value);
            }
        }
        supervise(
            command,
            cancel,
            timeout.min(self.timeout),
            self.output_limit,
            ErrorCode::GithubUnavailable,
        )
    }
    pub(crate) fn executable(&self) -> &std::path::Path {
        &self.executable
    }

    pub fn verify_repository(
        &self,
        expected: &Repository,
        cancel: &AtomicBool,
    ) -> Result<Repository> {
        if expected.id <= 0
            || !valid_owner(&expected.owner)
            || !valid_name(&expected.name)
            || expected.full_name != format!("{}/{}", expected.owner, expected.name)
            || expected.https_url != format!("https://github.com/{}.git", expected.full_name)
        {
            return Err(malformed());
        }
        let endpoint = format!("/repos/{}/{}", expected.owner, expected.name);
        let current = serde_json::from_slice::<ApiRepository>(&self.api(&endpoint, false, cancel)?)
            .map_err(|_| malformed())?
            .validate()?;
        if current.id != expected.id
            || current.full_name != expected.full_name
            || current.https_url != expected.https_url
        {
            return Err(failure(ErrorCode::ProviderData, "Repository identity changed. Refresh the GitHub library and review the repository before cloning."));
        }
        Ok(current)
    }

    pub fn version(&self) -> Result<String> {
        let output = self.run(
            &["--version"],
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?;
        let text = std::str::from_utf8(&output.stdout).map_err(|_| malformed())?;
        let version = text.lines().next().unwrap_or("");
        if !output.success
            || !version.starts_with("gh version ")
            || version.len() > 120
            || version.chars().any(char::is_control)
        {
            return Err(failure(
                ErrorCode::GithubUnavailable,
                "GitHub CLI is unusable. Install or update the official gh CLI.",
            ));
        }
        Ok(version.to_owned())
    }
    pub fn auth(&self, cancel: &AtomicBool) -> AuthStatus {
        let version = match self.version() {
            Ok(v) => v,
            Err(_) => return auth_status(None, AuthState::CliUnavailable, None, 0, false),
        };
        match self.run(
            &["auth", "status", "--json", "hosts"],
            cancel,
            Duration::from_secs(30),
        ) {
            Ok(output) if output.success => parse_auth(&output.stdout, version.clone())
                .unwrap_or_else(|_| {
                    auth_status(Some(version), AuthState::Uncertain, None, 0, false)
                }),
            Ok(output) => {
                let state = match classify(&output.stderr).code {
                    ErrorCode::Network => AuthState::NetworkUnavailable,
                    _ => AuthState::Uncertain,
                };
                auth_status(Some(version), state, None, 0, false)
            }
            Err(e) => auth_status(
                Some(version),
                if e.code == ErrorCode::Timeout {
                    AuthState::NetworkUnavailable
                } else {
                    AuthState::Uncertain
                },
                None,
                0,
                false,
            ),
        }
    }
    fn api(&self, endpoint: &str, paginate: bool, cancel: &AtomicBool) -> Result<Vec<u8>> {
        let mut args = vec![
            "api",
            "--hostname",
            "github.com",
            "--method",
            "GET",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "X-GitHub-Api-Version: 2022-11-28",
        ];
        if paginate {
            args.extend(["--paginate", "--slurp"]);
        }
        args.push(endpoint);
        let output = self.run(
            &args,
            cancel,
            if paginate {
                self.timeout
            } else {
                Duration::from_secs(30)
            },
        )?;
        if !output.success {
            return Err(classify(&output.stderr));
        }
        Ok(output.stdout)
    }
    fn account(&self, cancel: &AtomicBool) -> Result<Account> {
        let account: Account =
            serde_json::from_slice(&self.api("/user", false, cancel)?).map_err(|_| malformed())?;
        if account.id <= 0 || !valid_owner(&account.login) {
            return Err(malformed());
        }
        Ok(account)
    }
    pub fn fetch(
        &self,
        cancel: &AtomicBool,
        mut progress: impl FnMut(usize, usize),
    ) -> Result<Snapshot> {
        let status = self.auth(cancel);
        check_cancel(cancel)?;
        if status.state != AuthState::Authenticated {
            return Err(failure(
                match status.state {
                    AuthState::CliUnavailable => ErrorCode::GithubUnavailable,
                    AuthState::NetworkUnavailable => ErrorCode::Network,
                    AuthState::Unsupported => ErrorCode::Unsupported,
                    _ => ErrorCode::Authentication,
                },
                &status.message,
            ));
        }
        let before = self.account(cancel)?;
        if !status
            .login
            .as_ref()
            .is_some_and(|s| s.eq_ignore_ascii_case(&before.login))
        {
            return Err(account_changed());
        }
        // gh follows Link headers. Nonzero exit on ANY page discards all output.
        let bytes = self.api("/user/repos?visibility=all&affiliation=owner,collaborator,organization_member&per_page=100&sort=full_name&direction=asc", true, cancel)?;
        let repositories = parse_pages(&bytes, cancel, &mut progress)?;
        let after = self.account(cancel)?;
        if before != after {
            return Err(account_changed());
        }
        check_cancel(cancel)?;
        Ok(Snapshot {
            account: after,
            repositories,
        })
    }
    pub fn login(&self) -> Result<()> {
        self.version()?;
        if !cfg!(target_os = "macos") {
            return Err(failure(ErrorCode::Unsupported, "Run gh auth login --hostname github.com --web in your terminal, then recheck authentication."));
        }
        // Only a fixed discovered executable is ever embedded in the Terminal command.
        let path = self
            .executable
            .to_str()
            .filter(|p| CANDIDATES.contains(p))
            .ok_or_else(|| {
                failure(
                    ErrorCode::LoginHandoff,
                    "GitHub CLI login path is unsupported.",
                )
            })?;
        let home = std::env::var_os("HOME")
            .ok_or_else(|| failure(ErrorCode::LoginHandoff, "Home directory is unavailable."))?;
        let mut command = Command::new("/usr/bin/osascript");
        // AppleScript's quoted form handles HOME/config paths as one shell argument.
        // No login output passes through Stow. Prompts and credentials stay in Terminal/gh.
        command.args(["-e", "on run argv", "-e", "set loginCommand to \"/usr/bin/env -i PATH=/usr/bin:/bin TERM=xterm-256color \" & quoted form of (\"HOME=\" & item 1 of argv) & \" \" & quoted form of (\"GH_CONFIG_DIR=\" & item 2 of argv) & \" \" & quoted form of (item 3 of argv) & \" auth login --hostname github.com --web --git-protocol https --skip-ssh-key\"", "-e", "tell application \"Terminal\"", "-e", "activate", "-e", "do script loginCommand", "-e", "end tell", "-e", "end run", "--"]);
        let config = std::env::var_os("GH_CONFIG_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| {
                std::env::var_os("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute())
                    .map(|p| p.join("gh"))
            })
            .unwrap_or_else(|| PathBuf::from(&home).join(".config/gh"));
        command
            .arg(home)
            .arg(config)
            .arg(path)
            .env_clear()
            .env("PATH", "/usr/bin:/bin");
        handoff(command)
    }
}
fn account_changed() -> AppError {
    failure(ErrorCode::Authentication, "The active GitHub account changed during refresh. The previous cache is preserved; recheck and retry.")
}

fn auth_status(
    version: Option<String>,
    state: AuthState,
    login: Option<String>,
    known_accounts: usize,
    unsupported_hosts: bool,
) -> AuthStatus {
    let message = match state {
        AuthState::CliUnavailable => "GitHub CLI is missing or unusable. Install official gh in a supported location.",
        AuthState::NotAuthenticated => "No GitHub.com account is connected. Login is optional.",
        AuthState::Authenticated => "The active GitHub.com account passed the authentication check.",
        AuthState::Invalid => "The active account credentials are invalid or expired. Login again, then recheck.",
        AuthState::Uncertain => "Authentication could not be confirmed. Update gh if needed, check in Terminal, then recheck.",
        AuthState::NetworkUnavailable => "GitHub could not be reached. Cached repositories remain available.",
        AuthState::Unsupported => "This host or active-account configuration is unsupported. Stow supports GitHub.com only and never switches accounts.",
    }.to_owned();
    AuthStatus {
        cli_version: version,
        state,
        login,
        known_accounts,
        unsupported_hosts,
        checked_at: now(),
        message,
    }
}
#[derive(Deserialize)]
struct AuthEntry {
    state: String,
    active: bool,
    host: String,
    login: String,
    #[serde(default)]
    error: String,
}
#[derive(Deserialize)]
struct AuthResponse {
    hosts: BTreeMap<String, Vec<AuthEntry>>,
}
fn parse_auth(bytes: &[u8], version: String) -> Result<AuthStatus> {
    let response: AuthResponse = serde_json::from_slice(bytes).map_err(|_| malformed())?;
    let unsupported = response.hosts.keys().any(|h| h != "github.com");
    let accounts = response
        .hosts
        .get("github.com")
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let active: Vec<_> = accounts.iter().filter(|a| a.active).collect();
    let mut login = None;
    let state = if accounts.is_empty() {
        if unsupported {
            AuthState::Unsupported
        } else {
            AuthState::NotAuthenticated
        }
    } else if active.len() != 1 || active[0].host != "github.com" {
        AuthState::Unsupported
    } else {
        let entry = active[0];
        if valid_owner(&entry.login) {
            login = Some(entry.login.clone());
        }
        match entry.state.as_str() {
            "success" if login.is_some() && entry.error.is_empty() => AuthState::Authenticated,
            "timeout" => AuthState::NetworkUnavailable,
            "error" => match classify(entry.error.as_bytes()).code {
                ErrorCode::Authentication => AuthState::Invalid,
                ErrorCode::Network => AuthState::NetworkUnavailable,
                _ => AuthState::Uncertain,
            },
            _ => AuthState::Uncertain,
        }
    };
    Ok(auth_status(
        Some(version),
        state,
        login,
        accounts.len(),
        unsupported,
    ))
}
fn classify(bytes: &[u8]) -> AppError {
    // Inspect bounded diagnostics only to select a constant safe message; never return them.
    let s = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    if s.contains("rate limit") || s.contains("http 429") {
        failure(
            ErrorCode::RateLimited,
            "GitHub rate limit reached. Wait before refreshing; cached results are retained.",
        )
    } else if s.contains("http 401")
        || s.contains("bad credentials")
        || s.contains("token is invalid")
        || s.contains("not logged")
        || s.contains("authentication token")
    {
        failure(
            ErrorCode::Authentication,
            "GitHub credentials are missing, expired, or invalid. Login and recheck.",
        )
    } else if [
        "timeout",
        "timed out",
        "no such host",
        "network",
        "connection",
        "dial tcp",
        "tls handshake",
    ]
    .iter()
    .any(|x| s.contains(x))
    {
        failure(ErrorCode::Network, "GitHub is unreachable. Check the network and refresh later; cached results are retained.")
    } else if s.contains("http 403") {
        failure(
            ErrorCode::ProviderAccess,
            "GitHub denied access. Check account permissions, organization SSO, and rate limits.",
        )
    } else {
        failure(
            ErrorCode::ProviderFailed,
            "GitHub request failed. Check gh in Terminal and retry. No cache changes were applied.",
        )
    }
}
pub fn valid_owner(s: &str) -> bool {
    !s.is_empty() && s.len() <= 100 && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}
pub fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
}
fn safe_text(s: &str, limit: usize) -> bool {
    s.len() <= limit && !s.chars().any(char::is_control)
}
fn timestamp(s: &str) -> bool {
    s.len() == 20
        && s.bytes().enumerate().all(|(i, c)| match i {
            4 | 7 => c == b'-',
            10 => c == b'T',
            13 | 16 => c == b':',
            19 => c == b'Z',
            _ => c.is_ascii_digit(),
        })
}
#[derive(Deserialize)]
struct ApiOwner {
    login: String,
}
#[derive(Deserialize)]
struct ApiRepository {
    id: i64,
    owner: ApiOwner,
    name: String,
    full_name: String,
    description: Option<String>,
    html_url: String,
    clone_url: String,
    ssh_url: String,
    visibility: String,
    private: bool,
    fork: bool,
    archived: bool,
    default_branch: Option<String>,
    pushed_at: Option<String>,
    updated_at: String,
}
impl ApiRepository {
    fn validate(self) -> Result<Repository> {
        let canonical = format!("{}/{}", self.owner.login, self.name);
        if self.id <= 0
            || !valid_owner(&self.owner.login)
            || !valid_name(&self.name)
            || self.full_name != canonical
            || self.html_url != format!("https://github.com/{canonical}")
            || self.clone_url != format!("https://github.com/{canonical}.git")
            || self.ssh_url != format!("git@github.com:{canonical}.git")
            || !["public", "private"].contains(&self.visibility.as_str())
            || self.private != (self.visibility == "private")
            || !timestamp(&self.updated_at)
            || self.pushed_at.as_ref().is_some_and(|s| !timestamp(s))
            || self
                .description
                .as_ref()
                .is_some_and(|s| !safe_text(s, 4096))
            || self
                .default_branch
                .as_ref()
                .is_some_and(|s| s.is_empty() || !safe_text(s, 1024))
        {
            return Err(malformed());
        }
        Ok(Repository {
            id: self.id,
            owner: self.owner.login,
            name: self.name,
            full_name: canonical,
            description: self.description,
            web_url: self.html_url,
            https_url: self.clone_url,
            ssh_url: self.ssh_url,
            visibility: self.visibility,
            fork: self.fork,
            archived: self.archived,
            default_branch: self.default_branch,
            pushed_at: self.pushed_at,
            updated_at: self.updated_at,
        })
    }
}
fn parse_pages(
    bytes: &[u8],
    cancel: &AtomicBool,
    progress: &mut impl FnMut(usize, usize),
) -> Result<Vec<Repository>> {
    let mut repos = Vec::new();
    let mut ids = HashSet::new();
    let mut names = HashSet::new();
    let mut pages = 0;
    // --slurp preserves page boundaries even when gh stdout is piped.
    // The raw response has already passed the 32 MiB process-output bound.
    let response: Vec<Vec<ApiRepository>> =
        serde_json::from_slice(bytes).map_err(|_| malformed())?;
    for page in response {
        check_cancel(cancel)?;
        pages += 1;
        if page.len() > 100 || pages > 1000 || repos.len() + page.len() > MAX_REPOSITORIES {
            return Err(failure(ErrorCode::LimitReached, "GitHub library exceeds the supported 10,000 repository / 1,000 page limit. Cache preserved."));
        }
        for repo in page {
            let repo = repo.validate()?;
            if !ids.insert(repo.id) || !names.insert(repo.full_name.to_ascii_lowercase()) {
                return Err(failure(ErrorCode::ProviderData, "GitHub pagination contained duplicate identities, possibly from changes during refresh. Retry; cache preserved."));
            }
            repos.push(repo);
        }
        progress(pages, repos.len());
    }
    if pages == 0 {
        return Err(malformed());
    }
    Ok(repos)
}
fn read_bounded(mut stream: impl Read, limit: usize, overflow: Arc<AtomicBool>) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let n = stream.read(&mut buffer).map_err(|_| {
            failure(
                ErrorCode::ProviderFailed,
                "GitHub CLI output could not be read.",
            )
        })?;
        if n == 0 {
            break;
        }
        let remaining = limit.saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..n.min(remaining)]);
        if n > remaining {
            overflow.store(true, Ordering::Relaxed);
        }
    }
    Ok(bytes)
}
fn supervise(
    mut command: Command,
    cancel: &AtomicBool,
    timeout: Duration,
    limit: usize,
    spawn_error: ErrorCode,
) -> Result<Output> {
    check_cancel(cancel)?;
    let mut child = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|_| failure(spawn_error, "The required application could not be started. Check its installation and permissions."))?;
    let overflow = Arc::new(AtomicBool::new(false));
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let flag = overflow.clone();
    let reader = thread::spawn(move || read_bounded(stdout, limit, flag));
    let flag = overflow.clone();
    let error_reader = thread::spawn(move || read_bounded(stderr, 1024 * 1024, flag));
    let start = Instant::now();
    let result = loop {
        let error = if cancel.load(Ordering::Relaxed) {
            Some(failure(
                ErrorCode::Cancelled,
                "GitHub refresh cancelled. Previous cache retained.",
            ))
        } else if overflow.load(Ordering::Relaxed) {
            Some(failure(
                ErrorCode::LimitReached,
                "GitHub CLI response exceeded the bounded output limit. Cache retained.",
            ))
        } else if start.elapsed() > timeout {
            Some(failure(
                ErrorCode::Timeout,
                "GitHub CLI request timed out. Cache retained.",
            ))
        } else {
            None
        };
        if let Some(error) = error {
            let _ = child.kill();
            let _ = child.wait();
            break Err(error);
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status.success()),
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(failure(
                    ErrorCode::ProviderFailed,
                    "GitHub CLI process failed.",
                ));
            }
        }
    };
    let stdout = reader.join().map_err(|_| malformed())??;
    let stderr = error_reader.join().map_err(|_| malformed())??;
    let success = result?;
    check_cancel(cancel)?;
    if overflow.load(Ordering::Relaxed) {
        return Err(failure(
            ErrorCode::LimitReached,
            "GitHub CLI response exceeded the bounded output limit. Cache retained.",
        ));
    }
    Ok(Output {
        success,
        stdout,
        stderr,
    })
}
fn handoff(command: Command) -> Result<()> {
    let output = supervise(
        command,
        &AtomicBool::new(false),
        Duration::from_secs(15),
        4096,
        ErrorCode::LoginHandoff,
    )?;
    if output.success {
        Ok(())
    } else {
        Err(failure(ErrorCode::LoginHandoff, "Terminal login could not be opened. Run gh auth login --hostname github.com --web in Terminal, then recheck authentication."))
    }
}

pub fn open_repository(repository: &Repository) -> Result<()> {
    let url = format!(
        "https://github.com/{}/{}",
        repository.owner, repository.name
    );
    if !valid_owner(&repository.owner) || !valid_name(&repository.name) || repository.web_url != url
    {
        return Err(malformed());
    }
    if !cfg!(target_os = "macos") {
        return Err(failure(
            ErrorCode::Unsupported,
            "Opening GitHub URLs is currently supported on macOS only.",
        ));
    }
    let mut command = Command::new("/usr/bin/open");
    command.arg(&url).env_clear().env("PATH", "/usr/bin:/bin");
    handoff(command)
}

#[cfg(test)]
#[path = "github_tests.rs"]
mod tests;
