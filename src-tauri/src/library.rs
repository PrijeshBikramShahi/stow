use crate::{
    core::Project,
    github::{valid_name, valid_owner, Account, Repository},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedAccount {
    pub account: Account,
    pub refreshed_at: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedRepository {
    pub repository: Repository,
    pub fetched_at: i64,
    pub last_account: Account,
    pub last_seen_at: i64,
    pub availability: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cache {
    pub accounts: Vec<CachedAccount>,
    pub account_id: Option<i64>,
    pub repositories: Vec<CachedRepository>,
    pub attempted_at: Option<i64>,
    pub error: Option<crate::core::AppError>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Link {
    pub project_id: i64,
    pub repository_id: Option<i64>,
    pub candidates: Vec<i64>,
    pub state: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Library {
    pub projects: Vec<Project>,
    pub cache: Cache,
    pub links: Vec<Link>,
}

// Only conventional GitHub.com URLs. No query/fragment, port, encoded path,
// arbitrary userinfo, aliases, local paths, or project display-name matching.
pub fn remote_identity(raw: &str) -> Option<String> {
    let lower = raw.to_ascii_lowercase();
    let path = [
        "https://github.com/",
        "ssh://git@github.com/",
        "ssh://github.com/",
        "git@github.com:",
        "github.com:",
    ]
    .iter()
    .find_map(|prefix| lower.strip_prefix(prefix))?;
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    if !valid_owner(owner) || !valid_name(name) {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

pub fn match_projects(
    projects: &[Project],
    repositories: &[CachedRepository],
    names: &[(String, i64)],
) -> Vec<Link> {
    projects
        .iter()
        .map(|project| {
            let urls: BTreeSet<_> = project
                .inspection
                .as_ref()
                .map(|i| {
                    i.remotes
                        .iter()
                        .filter_map(|r| remote_identity(&r.url))
                        .collect()
                })
                .unwrap_or_default();
            let mut candidates = BTreeSet::new();
            let mut uncertain = project.availability != "available";
            for url in &urls {
                let current: BTreeSet<_> = repositories
                    .iter()
                    .filter(|r| r.repository.full_name.eq_ignore_ascii_case(url))
                    .map(|r| r.repository.id)
                    .collect();
                let historical: BTreeSet<_> = names
                    .iter()
                    .filter(|(name, _)| name.eq_ignore_ascii_case(url))
                    .map(|(_, id)| *id)
                    .collect();
                if current.len() != 1 || historical.iter().any(|id| !current.contains(id)) {
                    uncertain = true;
                }
                candidates.extend(current);
                candidates.extend(historical);
            }
            let linked = candidates.len() == 1 && !uncertain;
            Link {
                project_id: project.id,
                repository_id: if linked {
                    candidates.first().copied()
                } else {
                    None
                },
                state: if linked {
                    "linked"
                } else if !candidates.is_empty() {
                    "uncertain"
                } else {
                    "local"
                }
                .to_owned(),
                candidates: candidates.into_iter().collect(),
            }
        })
        .collect()
}
