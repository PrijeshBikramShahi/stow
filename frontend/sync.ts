import type { SyncAssessment, FetchJob } from './api';
export function syncLabel(assessment: Pick<SyncAssessment, 'relation' | 'ahead' | 'behind'>): string {
 switch (assessment.relation) {
  case 'up_to_date': return 'Up to date with last fetched state';
  case 'ahead': return `${assessment.ahead} commits ahead of cached upstream`;
  case 'behind': return `${assessment.behind} commits behind cached upstream`;
  case 'diverged': return `Branches diverged — ${assessment.ahead} ahead / ${assessment.behind} behind`;
  case 'no_upstream': return 'No upstream configured';
  case 'missing_tracking_ref': return 'Upstream tracking ref missing — fetch required to refresh';
  case 'ambiguous_upstream': return 'Tracking configuration is ambiguous or unsupported';
  case 'detached': return 'Detached HEAD — no branch comparison';
  case 'unborn': return 'No local commit to compare';
  default: return 'Remote status unknown — inspect warnings';
 }
}
export function lastSuccessfulFetch(history: FetchJob[], remote: string, url: string | null | undefined): FetchJob | undefined {
 return history.find(job => !!url && job.url === url && job.remote === remote && job.state === 'completed' && job.phase === 'verified');
}

export function pullReason(a: SyncAssessment): string | null {
 if (!a.identity) return 'Pull requires a verified ordinary workspace.';
 if (a.inspection.detached || !a.inspection.branch || !a.inspection.commit) return 'Pull requires an ordinary checked-out branch with a commit.';
 if (Object.values(a.inspection.working_tree).some(n => n > 0)) return 'Uncommitted changes must be handled before pulling.';
 if (a.operations.length) return 'A Git operation or lock is active. Finish it before pulling.';
 if (a.relation === 'diverged') return 'Local and upstream histories have diverged.';
 if (a.relation === 'ahead') return 'Local branch is ahead only. Pull requires the upstream to descend from HEAD.';
 if (!['behind', 'up_to_date'].includes(a.relation)) return 'Pull requires a single valid upstream and an available ordinary tracking reference.';
 const remote = a.remotes.find(r => a.upstream_ref?.startsWith(`refs/remotes/${r.name}/`));
 return remote?.reason ?? (!remote ? 'Upstream remote is unavailable.' : null);
}
export function pullLabel(a: SyncAssessment): string {
 return pullReason(a) ?? (a.behind ? `${a.behind} commits available to pull. Fetch will verify current remote state.` : 'Already up to date with cached upstream. Pull will fetch to verify.');
}
export function pendingPull(pulls: import('./api').PullJob[], operation?: {kind: string; state: string}): import('./api').PullJob | undefined {
 return operation?.kind === 'pull' && operation.state === 'running' ? pulls.find(j => j.state === 'running' && j.phase === 'awaiting_confirmation') : undefined;
}
export function pushReason(a: SyncAssessment): string | null {
 if (!a.identity || a.inspection.detached || !a.inspection.branch || !a.inspection.commit) return 'Push requires a verified ordinary branch with an existing commit.';
 if (a.operations.length) return 'A Git operation or lock is active.';
 if (!['ahead', 'up_to_date', 'missing_tracking_ref'].includes(a.relation)) return 'Push requires one existing upstream and non-diverged history. Inspect and fetch to refresh.';
 return null;
}
export function pendingPush(pushes: import('./api').PushJob[], operation?: {kind: string; state: string}): import('./api').PushJob | undefined {
 return operation?.kind === 'push' && operation.state === 'running' ? pushes.find(j => j.state === 'running' && j.phase === 'awaiting_confirmation') : undefined;
}
