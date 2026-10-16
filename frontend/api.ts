import { invoke } from '@tauri-apps/api/core';

export interface AppError { code: string; message: string }
export interface Inspection {
  branch: string | null;
  detached: boolean;
  commit: string | null;
  upstream: string | null;
  remotes: { name: string; url: string }[];
  working_tree: { staged: number; unstaged: number; untracked: number; conflicted: number };
  warnings: string[];
}
export interface Project {
  id: number;
  name: string;
  path: string;
  registered_at: number;
  inspected_at: number | null;
  availability: 'available' | 'missing' | 'unsupported' | 'error';
  inspection: Inspection | null;
  error: AppError | null;
}
export interface Operation {
  id: string;
  kind: string;
  state: 'running' | 'completed' | 'failed' | 'cancelled' | 'requires_inspection';
  started_at: number;
  completed_at: number | null;
  progress: { message?: string | null; percent?: number | null; visited: number; registered: number; skipped: number; issues: { path: string; error: AppError }[] };
  error: AppError | null;
}
export interface SystemInfo { git_version: string | null; git_error: AppError | null; database_error: AppError | null }

// All renderer/native calls go through this contract. No command accepts shell arguments.
export const api = {
  pull: (projectId: number, token: string) => invoke<Operation>('start_pull', { request: { project_id: projectId, token } }),
  confirmPull: (id: string, confirmed: boolean) => invoke<void>('confirm_pull', { id, confirmed }),
  syncStatus: (id: number) => invoke<SyncView>('sync_status', { id }),
  inspectSync: (id: number) => invoke<Operation>('start_sync_inspection', { id }),
  fetch: (projectId: number, token: string, remote: string) => invoke<Operation>('start_fetch', { request: { project_id: projectId, token, remote } }),
  workspaceSettings: () => invoke<WorkspaceSettings>('workspace_settings'),
  savePreferences: (preferences: Preferences) => invoke<void>('save_workspace_preferences', { preferences }),
  validateDestination: (root: string, name: string) => invoke<{root: string; destination: string}>('validate_clone_destination', { root, name }),
  clone: (repositoryId: number, accountId: number | null, root: string, name: string, openAfter: boolean) => invoke<Operation>('start_clone', { request: { repository_id: repositoryId, account_id: accountId, root, name, open_after: openAfter } }),
  recoverClone: (id: string) => invoke<Operation>('start_clone_recovery', { id, openAfter: false }),
  openProject: (id: number, reveal = false) => invoke<void>('open_project', { id, reveal }),
  revealClone: (id: string) => invoke<void>('reveal_clone', { id }),
  library: (accountId: number | null = null) => invoke<Library>('repository_library', { accountId }),
  githubStatus: () => invoke<AuthStatus>('github_status'),
  githubLogin: () => invoke<void>('github_login'),
  githubRefresh: () => invoke<Operation>('start_github_refresh'),
  openGithub: (id: number, accountId: number | null) => invoke<void>('open_github_repository', { id, accountId }),
  system: () => invoke<SystemInfo>('system_info'),
  projects: () => invoke<Project[]>('list_projects'),
  operations: () => invoke<Operation[]>('list_operations'),
  scan: (roots: string[]) => invoke<Operation>('start_scan', { roots }),
  inspect: (id: number | null) => invoke<Operation>('start_inspection', { id }),
  cancel: (id: string) => invoke<void>('cancel_operation', { id }),
  remove: (id: number) => invoke<void>('remove_project', { id }),
};

export function errorMessage(error: unknown): string {
  if (error && typeof error === 'object' && 'message' in error) return String(error.message);
  return typeof error === 'string' ? error : 'The operation failed. Please try again.';
}

export interface AuthStatus {
  cli_version: string | null;
  state: 'cli_unavailable' | 'not_authenticated' | 'authenticated' | 'invalid' | 'uncertain' | 'network_unavailable' | 'unsupported';
  login: string | null;
  known_accounts: number;
  unsupported_hosts: boolean;
  checked_at: number;
  message: string;
}
export interface Account { id: number; login: string }
export interface RemoteRepository {
  id: number; owner: string; name: string; full_name: string;
  description: string | null; web_url: string; https_url: string; ssh_url: string;
  visibility: 'public' | 'private'; fork: boolean; archived: boolean;
  default_branch: string | null; pushed_at: string | null; updated_at: string;
}
export interface CachedRepository {
  repository: RemoteRepository; fetched_at: number; last_account: Account;
  last_seen_at: number; availability: 'seen' | 'not_returned';
}
export interface Library {
  projects: Project[];
  links: { project_id: number; repository_id: number | null; candidates: number[]; state: 'local' | 'linked' | 'uncertain' }[];
  cache: {
    accounts: { account: Account; refreshed_at: number }[];
    account_id: number | null; repositories: CachedRepository[];
    attempted_at: number | null; error: AppError | null;
  };
}

export interface Preferences { root: string | null; editor: string }
export interface CloneJob {
  id: string; repository: RemoteRepository; root: string; staging: string; destination: string;
  phase: string; state: string; verified: Inspection | null; project_id: number | null;
  error: AppError | null; editor_error: AppError | null;
}
export interface WorkspaceSettings { preferences: Preferences; editors: { id: string; name: string; available: boolean }[]; jobs: CloneJob[] }

export interface SyncAssessment {
  token: string; inspected_at: number; inspection: Inspection; identity: unknown | null;
  upstream_ref: string | null; upstream_commit: string | null; ahead: number | null; behind: number | null;
  relation: string; operations: string[]; remotes: {name: string; url: string; reason: string | null; fetch_key: string | null}[]; warnings: string[];
}
export interface FetchJob {id: string; project_id: number; remote: string; url: string; started_at: number; completed_at: number | null; state: string; phase: string; staging: string | null; error: AppError | null}
export interface SyncView {record: {project_id: number; assessment: SyncAssessment | null; error: AppError | null}; history: FetchJob[]; pulls: PullJob[]}

export interface PullJob { id: string; project_id: number; started_at: number; completed_at: number | null; state: string; phase: string; branch: string; upstream: string; before_head: string; target: string | null; after_head: string | null; after_relation: string | null; error: AppError | null }
