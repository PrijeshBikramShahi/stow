import type { CloneJob } from './api';
export function canRecover(job: Pick<CloneJob, 'state' | 'phase' | 'verified'>): boolean {
  return job.state !== 'completed' && job.verified !== null && ['verified', 'promoting', 'promoted'].includes(job.phase);
}
