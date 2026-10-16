import { useEffect, useState } from 'react';
import { api, errorMessage, type Operation, type Project, type SyncView } from './api';
import { syncLabel, lastSuccessfulFetch, pullReason, pullLabel, pendingPull } from './sync';
const when = (value: number | null) => value ? new Date(value * 1000).toLocaleString() : 'Unknown';
export function SyncDetails({ project, operation, busy, act }: { project: Project; operation?: Operation; busy: boolean; act: (action: () => Promise<unknown>) => Promise<void> }) {
 const [view, setView] = useState<SyncView | null>(null);
 const [error, setError] = useState<string | null>(null);
 const [remote, setRemote] = useState('');
 useEffect(() => {
  let disposed = false;
  void api.syncStatus(project.id).then(value => {if (!disposed) { setView(value); setError(null); setRemote(current => value.record.assessment?.remotes.some(r => r.name === current) ? current : value.record.assessment?.remotes[0]?.name ?? ''); }}).catch(error => {if (!disposed) setError(errorMessage(error));});
  return () => { disposed = true; };
 }, [project.id, project.inspected_at, operation?.id, operation?.state, operation?.progress.message]);
 const a = view?.record.assessment;
 const confirmation = pendingPull(view?.pulls ?? [], operation);
 const selected = a?.remotes.find(r => r.name === remote);
 const last = view ? lastSuccessfulFetch(view.history, remote, selected?.fetch_key) : undefined;
 const canFetch = a && selected && !selected.reason && a.identity && a.relation !== 'unsupported' && !a.operations.length && !view?.record.error;
 return <section aria-label="Synchronization status" className="notice">
  <h3>Synchronization preflight</h3>
  <button disabled={busy} onClick={() => void act(() => api.inspectSync(project.id))}>Inspect sync</button>
  {error && <p role="alert">{error}</p>}
  {view?.record.error && <p role="alert">{view.record.error.message} Previous assessment below may be stale.</p>}
  {!a ? <p>Inspect local Git state before authorizing fetch. This inspection makes no network request.</p> : <>
   <p><strong>{syncLabel(a)}</strong></p>
   <p>{pullLabel(a)}</p>
   <button disabled={busy || !!pullReason(a) || !!view?.record.error} onClick={() => void act(() => api.pull(project.id, a.token))}>Pull (Fast-Forward Only)</button>
   <p className="subtle">Pull preflight also checks ignored files, index flags, hooks, attributes, and filesystem safety before confirmation.</p>
   {confirmation && <div role="alertdialog" aria-label="Confirm fast-forward pull" className="notice">
    <p>Fetch the configured remote and fast-forward <strong>{confirmation.branch}</strong> from <code>{confirmation.before_head}</code> to the freshly verified commit of <strong>{confirmation.upstream}</strong>, or do nothing if already up to date?</p>
    <p>This updates branch HEAD, index, and working files. Once integration starts, cancellation is deferred until Git exits and verification finishes. Keep other Git tools and file editors idle during pull.</p>
    <button onClick={() => void act(() => api.confirmPull(confirmation.id, true))}>Confirm fetch & fast-forward</button>
    <button onClick={() => void act(() => api.confirmPull(confirmation.id, false))}>Decline pull</button>
   </div>}
   <dl><dt>Local inspected</dt><dd>{when(a.inspected_at)}</dd><dt>Branch</dt><dd>{a.inspection.detached ? 'Detached HEAD' : a.inspection.branch ?? 'Unknown'}</dd><dt>HEAD</dt><dd><code>{a.inspection.commit ?? 'No commit'}</code></dd><dt>Upstream</dt><dd>{a.upstream_ref ?? a.inspection.upstream ?? 'Not configured'}</dd><dt>Cached upstream</dt><dd><code>{a.upstream_commit ?? 'Unknown'}</code></dd></dl>
   <p>Files: {a.inspection.working_tree.staged} staged · {a.inspection.working_tree.unstaged} unstaged · {a.inspection.working_tree.untracked} untracked · {a.inspection.working_tree.conflicted} conflicts</p>
   {a.operations.length > 0 && <p role="alert">Git operation/lock present: {a.operations.join(', ')}</p>}
   {a.warnings.map(warning => <p className="subtle" key={warning}>{warning}</p>)}
   {a.remotes.length ? <><label>Remote to fetch<select value={remote} disabled={busy} onChange={e => setRemote(e.target.value)}>{a.remotes.map(r => <option key={r.name} value={r.name}>{r.name}</option>)}</select></label><p className="path">{selected?.url}</p>{selected?.reason && <p role="alert">{selected.reason}</p>}<button disabled={busy || !canFetch} onClick={() => void act(() => api.fetch(project.id, a.token, remote))}>Fetch selected remote</button></> : <p>No configured remote to fetch.</p>}
   <p>Last successful Stow fetch for {remote || 'selected remote'}: {when(last?.completed_at ?? null)}</p>
   <p className="subtle">Remote status unknown now. Counts use cached refs; fetch required to refresh. Fetch only updates Git objects and remote-tracking metadata, never pushes or pulls. Deleted remote branches are retained. Prior success does not establish current remote availability.</p>
  </>}
  {operation?.kind === 'pull' && <p role="status">{operation.progress.message ?? operation.state}{operation.error ? ` ${operation.error.message}` : ''}</p>}
  {!!view?.pulls?.length && <details open><summary>Recent pull history</summary>{view.pulls.map(job => <article key={job.id}>
   <p><strong>{job.branch} ← {job.upstream} · {job.state}</strong> · {when(job.started_at)} · {job.phase}</p>
   {job.error && <p role="alert">{job.error.message}</p>}
   {job.target && <p>Verified target: <code>{job.target}</code></p>}
   {job.after_head && <p>Post-update HEAD: <code>{job.after_head}</code> · {job.after_relation}</p>}
   {job.state === 'requires_inspection' && <p>Do not retry until Git activity has stopped and you have inspected HEAD, index, files, and locks. No automatic reset or recovery is performed.</p>}
  </article>)}</details>}
  {!!view?.history.length && <details><summary>Recent fetch history ({view.history.length}, up to 50 shown)</summary>{view.history.map(job => <article key={job.id}><p><strong>{job.remote} · {job.state === 'running' ? 'In progress or interrupted — inspect before retrying' : job.state}</strong> · {when(job.started_at)} · {job.phase}</p>{job.error && <p role="alert">{job.error.message}</p>}{job.staging && <p className="path">Retained metadata: {job.staging}</p>}{job.state !== 'completed' && <p className="subtle">No successful fetch is recorded for this attempt. Some objects or tracking updates may have been written; inspect again. Stow does not delete retained data or automatically retry.</p>}</article>)}</details>}
 </section>;
}
