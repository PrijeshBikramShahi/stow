import { useEffect, useState } from 'react';
import { api, errorMessage, type Change, type ChangeReview, type ChangeStatus, type Operation, type Project } from './api';
import { commitReason, pendingCommit, displayPath } from './changes';

export function ChangesDetails({project, operation, busy, act}: {project: Project; operation?: Operation; busy: boolean; act: (action: () => Promise<unknown>) => Promise<void>}) {
 const [review, setReview] = useState<ChangeReview | null>(null);
 const [status, setStatus] = useState<ChangeStatus | null>(null);
 const [error, setError] = useState<string | null>(null);
 const [loading, setLoading] = useState(false);
 const [refresh, setRefresh] = useState(0);
 const [staged, setStaged] = useState<string[]>([]);
 const [unstaged, setUnstaged] = useState<string[]>([]);
 const [message, setMessage] = useState('');
 const [preview, setPreview] = useState<{title: string; text: string} | null>(null);
 useEffect(() => {
  let disposed = false;
  void api.changeStatus(project.id).then(s => {if (!disposed) setStatus(s);}).catch(e => {if (!disposed) setError(errorMessage(e));});
  return () => {disposed = true;};
 }, [project.id, operation?.id, operation?.state, operation?.progress.message]);
 useEffect(() => {
  if (busy) return;
  let disposed = false;
  setLoading(true); setError(null); setReview(null); setStaged([]); setUnstaged([]); setPreview(null);
  void api.reviewChanges(project.id).then(r => {if (!disposed) setReview(r);}).catch(e => {if (!disposed) setError(errorMessage(e));}).finally(() => {if (!disposed) setLoading(false);});
  return () => {disposed = true;};
 }, [project.id, operation?.id, operation?.state, busy, refresh]);
 const confirmation = pendingCommit(status?.confirmation, operation);
 const reason = commitReason(review, message);
 const disabled = busy || loading;
 function toggle(path: string, value: boolean, inIndex: boolean) {
  const setter = inIndex ? setStaged : setUnstaged;
  setter(old => value ? [...old, path] : old.filter(p => p !== path));
 }
 async function showDiff(entry: Change, inIndex: boolean) {
  if (!review) return;
  setLoading(true); setError(null);
  try {const d = await api.changeDiff(project.id, review.token, entry.path, inIndex);setPreview({title: `${inIndex ? 'Staged' : 'Working'} · ${displayPath(entry.path)}`, text: d.text});}
  catch (e) {setError(errorMessage(e));setPreview(null);}
  finally {setLoading(false);}
 }
 function list(entries: Change[], inIndex: boolean) {
  return entries.length ? <ul className="change-list">{entries.map(e => <li key={e.path}>
   <label><input type="checkbox" disabled={disabled} checked={(inIndex ? staged : unstaged).includes(e.path)} onChange={event => toggle(e.path, event.target.checked, inIndex)} />
    <span>{e.original && <>{displayPath(e.original)} → </>}{displayPath(e.path)} · {inIndex ? e.staged : e.untracked ? 'untracked' : e.unstaged}{e.binary ? ' · binary' : ''}</span>
   </label>
   <button disabled={disabled} onClick={() => void showDiff(e, inIndex)}>View {inIndex ? 'staged' : 'working'} diff</button>
  </li>)}</ul> : <p className="subtle">No files.</p>;
 }
 return <section className="notice" aria-label="Local changes">
  <h3>Changes</h3>
  <button disabled={disabled} onClick={() => setRefresh(n => n + 1)}>Refresh changes</button>
  {loading && <p role="status">Reading bounded change review…</p>}
  {error && <p role="alert">{error}</p>}
  {review && <>
   <p>{review.branch} · <code>{review.head}</code></p>
   <h4>Staged ({review.entries.filter(e => e.staged).length})</h4>
   {list(review.entries.filter(e => e.staged), true)}
   <button disabled={disabled || !staged.length} onClick={() => void act(() => api.changeIndex(project.id, review.token, staged, true))}>Unstage selected files</button>
   <h4>Unstaged ({review.entries.filter(e => e.unstaged).length})</h4>
   {list(review.entries.filter(e => e.unstaged), false)}
   <h4>Untracked ({review.entries.filter(e => e.untracked).length})</h4>
   {list(review.entries.filter(e => e.untracked), false)}
   <button disabled={disabled || !unstaged.length} onClick={() => void act(() => api.changeIndex(project.id, review.token, unstaged, false))}>Stage selected files</button>
   <p className="subtle">File-level selection only. A selected rename includes both displayed paths. Unstaging preserves working files. Local ignore rules apply; global ignore configuration is disabled by the existing Git policy.</p>
   {preview && <div><h4>{preview.title}</h4><pre className="diff-preview">{preview.text || 'No textual differences.'}</pre></div>}
   <label>Commit message<textarea value={message} disabled={disabled} onChange={e => setMessage(e.target.value)} rows={4} /></label>
   {review.author && <p>Author: {review.author.name} &lt;{review.author.email}&gt;</p>}
   {review.author_error && <p role="alert">{review.author_error.message}</p>}
   {reason && <p className="subtle">{reason}</p>}
   <button disabled={disabled || !!reason} onClick={() => void act(() => api.commit(project.id, review.token, message))}>Review commit</button>
  </>}
  {confirmation && <div className="notice" role="alertdialog" aria-label="Confirm staged commit">
   <h4>Confirm staged commit</h4><p className="path">{confirmation.repository}</p>
   <p>Branch: {confirmation.job.branch} · Parent: <code>{confirmation.job.before_head}</code></p>
   <p>Staged tree: <code>{confirmation.job.tree}</code></p>
   <ul>{confirmation.entries.map(e => <li key={e.path}>{e.staged}: {e.original && <>{displayPath(e.original)} → </>}{displayPath(e.path)}</li>)}</ul>
   <p>Author and committer: {confirmation.author.name} &lt;{confirmation.author.email}&gt;</p>
   <pre className="diff-preview">{confirmation.message}</pre>
   <p>Only this staged snapshot is committed. Hooks, signing, and project tests are not run. Nothing is pushed. Keep other Git tools idle; cancellation is deferred during the atomic branch update.</p>
   <button onClick={() => void act(() => api.confirmCommit(confirmation.job.id, true))}>Confirm commit</button>
   <button onClick={() => void act(() => api.confirmCommit(confirmation.job.id, false))}>Decline commit</button>
  </div>}
  {operation && ['stage','unstage','commit'].includes(operation.kind) && <p role="status">{operation.progress.message ?? operation.state}{operation.error ? ` ${operation.error.message}` : ''}</p>}
  {!!status?.history.length && <details><summary>Recent change history</summary>{status.history.map(j => <article key={j.id}>
   <p><strong>{j.kind} · {j.state}</strong> · {j.phase} · {j.paths.length} paths</p>
   {j.after_head && <p>Recorded commit: <code>{j.after_head}</code></p>}
   {j.error && <p role="alert">{j.error.message}</p>}
   {(j.state === 'requires_inspection' || (j.state === 'running' && operation?.state !== 'running')) && <p>Refresh HEAD and changes, inspect Git locks, and review history before retrying. No automatic reset, lock removal, or retry occurs.</p>}
  </article>)}</details>}
 </section>;
}
