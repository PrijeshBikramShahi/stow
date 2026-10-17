import { useCallback, useEffect, useState } from 'react';
import { isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';
import { api, errorMessage, type Operation, type Project, type SystemInfo, type Library, type AuthStatus } from './api';

import { SyncDetails } from './SyncDetails';
import { ChangesDetails } from './ChangesDetails';
import { WorkspaceControls } from './WorkspaceControls';
import type { WorkspaceSettings } from './api';
import { libraryRows } from './library';

const time = (value: number | null) => value ? new Date(value * 1000).toLocaleString() : 'Not inspected';
function condition(project: Project) {
  if (project.availability !== 'available') return project.availability;
  if (!project.inspection) return 'Unknown';
  return Object.values(project.inspection.working_tree).some(Boolean) ? 'Dirty' : 'Clean';
}

export function App() {
  const [workspaceSettings, setWorkspaceSettings] = useState<WorkspaceSettings | null>(null);
  const [library, setLibrary] = useState<Library | null>(null);
  const [auth, setAuth] = useState<AuthStatus | null>(null);
  const [authPending, setAuthPending] = useState(false);
  const [loginStarted, setLoginStarted] = useState(false);
  const [cacheAccount, setCacheAccount] = useState<number | null>(null);
  const [availabilityFilter, setAvailabilityFilter] = useState('all');
  const [visibilityFilter, setVisibilityFilter] = useState('all');
  const [sort, setSort] = useState('name');
  const [remoteId, setRemoteId] = useState<number | null>(null);
  const [projects, setProjects] = useState<Project[]>([]);
  const [operations, setOperations] = useState<Operation[]>([]);
  const [system, setSystem] = useState<SystemInfo | null>(null);
  const [roots, setRoots] = useState<string[]>([]);
  const [query, setQuery] = useState('');
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [pending, setPending] = useState(false);
  const [removeId, setRemoveId] = useState<number | null>(null);
  const [cancelling, setCancelling] = useState(false);
  const active = operations.find(op => op.state === 'running');
  const latest = operations[0];
  const busy = pending || Boolean(active);
  const selected = projects.find(project => project.id === (selectedId ?? library?.links.find(link => link.repository_id === remoteId && remoteId !== null)?.project_id));

  const reload = useCallback(async () => {
    const [library, operations, settings] = await Promise.all([api.library(cacheAccount), api.operations(), api.workspaceSettings()]);
    setWorkspaceSettings(settings);
    setLibrary(library);
    setProjects(library.projects);
    setOperations(operations);
  }, [cacheAccount]);

  useEffect(() => {
    if (!isTauri()) {
      setError('Open Stow as a desktop application with npm run desktop. This browser preview cannot access the local registry.');
      setLoading(false);
      return;
    }
    let disposed = false;
    const initialize = async () => {
      try {
        const info = await api.system();
        if (disposed) return;
        setSystem(info);
        if (info.database_error) throw info.database_error;
        await reload();
      } catch (error) { if (!disposed) setError(errorMessage(error)); }
      finally { if (!disposed) setLoading(false); }
    };
    void initialize();
    setAuthPending(true);
    void api.githubStatus().then(status => { if (!disposed) setAuth(status); }).catch(e => { if (!disposed) setError(errorMessage(e)); }).finally(() => { if (!disposed) setAuthPending(false); });
    // Events provide prompt completion; polling recovers from a missed event/reload.
    const unlisten = listen('operation-progress', () => { if (!disposed) void reload().catch(e => setError(errorMessage(e))); });
    return () => { disposed = true; void unlisten.then(stop => stop()); };
  }, [reload]);

  useEffect(() => {
    if (!active) { setCancelling(false); return; }
    const timer = window.setInterval(() => void reload().catch(e => setError(errorMessage(e))), 1000);
    return () => window.clearInterval(timer);
  }, [active?.id, reload]);

  async function act(action: () => Promise<unknown>) {
    setError(null);
    setPending(true);
    try { await action(); await reload(); }
    catch (error) { setError(errorMessage(error)); }
    finally { setPending(false); }
  }

  async function chooseDirectories() {
    setError(null);
    try {
      const chosen = await open({ directory: true, multiple: true, title: 'Choose directories to scan for repositories' });
      if (chosen) setRoots(existing => [...new Set([...existing, ...(Array.isArray(chosen) ? chosen : [chosen])])].slice(0, 16));
    } catch (error) { setError(errorMessage(error)); }
  }

  const visible = library ? libraryRows(library, { query, availability: availabilityFilter, visibility: visibilityFilter, sort }) : [];
  const selectedRemote = library?.cache.repositories.find(r => r.repository.id === remoteId);
  const cachedAccount = library?.cache.accounts.find(a => a.account.id === library.cache.account_id);
  const cacheStale = Boolean(library?.cache.error) || auth?.state !== 'authenticated' || auth?.login !== cachedAccount?.account.login || Boolean(cachedAccount && Date.now() / 1000 - cachedAccount.refreshed_at > 86400);
  async function recheckAuth() {
    setAuthPending(true); setError(null);
    try { setAuth(await api.githubStatus()); } catch (e) { setError(errorMessage(e)); } finally { setAuthPending(false); }
  }
  const available = isTauri() && !loading && !system?.database_error;

  return <main>
    <header className="app-header"><div><h1>Stow</h1><p>Your projects. On demand.</p></div><span className="subtle">{system?.git_version ?? 'Local workspace registry'}</span></header>
    <section className="toolbar" aria-label="Project actions">
      <div><h2>Repository library <span className="count">{visible.length}</span></h2><p>Local workspaces and cached GitHub repositories.</p></div>
      <div className="actions"><button disabled={!available || busy} onClick={() => void chooseDirectories()}>Add scan directory</button><button disabled={!available || busy || !projects.length || !system?.git_version} onClick={() => void act(() => api.inspect(null))}>Inspect all local</button></div>
    </section>
    <section className="notice" aria-label="GitHub account">
      <h2>GitHub.com</h2>
      <p>{authPending ? 'Checking GitHub authentication…' : auth?.message ?? 'Authentication has not been checked.'}</p>
      <p className="subtle">{auth?.cli_version ?? 'GitHub CLI not confirmed'} · Active account: {auth?.login ?? 'None confirmed'}{auth ? ` · Checked ${time(auth.checked_at)}` : ''}</p>
      {auth && auth.known_accounts > 1 && <p className="subtle">{auth.known_accounts} known GitHub.com accounts. Stow uses the active account and does not switch accounts.</p>}
      {auth?.unsupported_hosts && <p className="subtle">Other GitHub hosts were detected. Enterprise repository listing is unsupported.</p>}
      <div className="actions">
        <button disabled={!available || busy || authPending || !auth?.cli_version} onClick={() => void act(async () => { await api.githubLogin(); setLoginStarted(true); })}>Connect in Terminal</button>
        <button disabled={!available || authPending || busy} onClick={() => void recheckAuth()}>Recheck authentication</button>
        <button className="primary" disabled={!available || busy || authPending || auth?.state !== 'authenticated'} onClick={() => void act(() => api.githubRefresh())}>Refresh GitHub repositories</button>
      </div>
      <p className="subtle">Connect opens Terminal and runs official gh browser login for GitHub.com. Complete its prompts, then recheck here. Credentials stay with gh. Login is optional.</p>
      {loginStarted && <p role="status">Login opened in Terminal. After completing it, choose Recheck authentication, then Refresh GitHub repositories.</p>}
      <details><summary>Installation and manual login</summary><p>Install the official GitHub CLI. Stow checks /opt/homebrew/bin/gh, /usr/local/bin/gh, /opt/local/bin/gh, and /usr/bin/gh. It uses gh’s saved login, not token environment variables.</p><code>gh auth login --hostname github.com --web --git-protocol https --skip-ssh-key</code><p>Run this in Terminal if the handoff is unavailable, then recheck. No personal token needs to be entered into Stow.</p></details>
      <p>Last successful repository refresh: {cachedAccount ? `${time(cachedAccount.refreshed_at)} (${cachedAccount.account.login})` : 'Never'}</p>
      {library && library.cache.accounts.length > 0 && <label>Cached account <select value={cacheAccount ?? ''} onChange={e => { setCacheAccount(e.target.value ? Number(e.target.value) : null); setRemoteId(null); }}><option value="">Most recently refreshed</option>{library.cache.accounts.map(a => <option key={a.account.id} value={a.account.id}>{a.account.login}</option>)}</select></label>}
      {cachedAccount && <p className={cacheStale ? 'error-text' : 'subtle'}>{cacheStale ? 'Remote cache stale or not currently confirmed.' : 'Cached metadata from the last successful refresh.'} This is not a live access or recoverability check.</p>}
      {cachedAccount && auth?.login && cachedAccount.account.login !== auth.login && <p className="notice">Showing the saved library for {cachedAccount.account.login}. Refresh uses the active account {auth.login}; previous account history is retained.</p>}
      {library?.cache.error && <p role="alert" className="error-text">Last refresh attempt {time(library.cache.attempted_at)}: {library.cache.error.message}</p>}
      {cachedAccount && !library?.cache.repositories.length && <p>No repositories were returned for this account at its last successful refresh.</p>}
      <p className="subtle">Results include owned, collaborator, and organization repositories permitted by your credentials. Scopes, SSO, and organization policies can limit this list. Refresh is manual.</p>
    </section>
    {workspaceSettings && <WorkspaceControls settings={workspaceSettings} repository={!selected ? selectedRemote?.repository : undefined} accountId={library?.cache.account_id ?? null} disabled={!available || busy} act={act} />}
    {error && <div role="alert" className="notice error">{error}<button aria-label="Dismiss error" onClick={() => setError(null)}>Dismiss</button></div>}
    {system?.git_error && <div role="alert" className="notice error">{system.git_error.message} Existing registrations remain available.</div>}
    {roots.length > 0 && <section className="scan-roots" aria-label="Selected scan directories">
      <h3>Scan directories</h3>
      <ul>{roots.map(root => <li key={root}><code>{root}</code><button disabled={busy} aria-label={`Remove scan directory ${root}`} onClick={() => setRoots(roots.filter(p => p !== root))}>Remove</button></li>)}</ul>
      <div className="actions"><button className="primary" disabled={busy || !available || !system?.git_version} onClick={() => void act(() => api.scan(roots))}>Scan selected directories</button><span className="subtle">Up to 6 levels · 20,000 entries · repository boundaries respected</span></div>
    </section>}
    {latest && <section className="operation" aria-label="Latest operation">
      <div className="actions"><strong aria-live="polite">{latest.kind} — {latest.state}</strong>{active && <button disabled={cancelling} onClick={() => { setCancelling(true); void api.cancel(active.id).catch(e => { setError(errorMessage(e)); setCancelling(false); }); }}>{cancelling ? 'Cancelling…' : 'Cancel'}</button>}</div>
      <p>{latest.progress.message ? `${latest.progress.message}${latest.progress.percent != null ? ` · ${latest.progress.percent}%` : ''}` : latest.kind === 'github_refresh' ? `${latest.progress.visited} pages validated · ${latest.progress.registered} repositories staged (saved only on completion)` : `${latest.progress.visited} entries checked · ${latest.progress.registered} repositories inspected · ${latest.progress.skipped} skipped`}</p>
      <p className="subtle">Started {time(latest.started_at)}{latest.completed_at ? ` · Finished ${time(latest.completed_at)}` : ''}</p>
      {latest.error && <p role="alert" className="error-text">{latest.error.message}</p>}
      {latest.progress.issues.length > 0 && <details><summary>{latest.progress.issues.length} reported issues (up to 100 shown)</summary><ul>{latest.progress.issues.map((issue, i) => <li key={i}><code>{issue.path}</code><p>{issue.error.message}</p></li>)}</ul></details>}
    </section>}
    <label className="search">Search library<input type="search" placeholder="Name, owner, local path, or branch" value={query} onChange={e => setQuery(e.target.value)} /></label>
    <div className="actions filters">
      <label>Availability <select value={availabilityFilter} onChange={e => setAvailabilityFilter(e.target.value)}><option value="all">All</option><option value="local">Local workspaces</option><option value="remote">Remote only</option></select></label>
      <label>Visibility <select value={visibilityFilter} onChange={e => setVisibilityFilter(e.target.value)}><option value="all">All / unknown</option><option value="public">Public</option><option value="private">Private</option></select></label>
      <label>Sort <select value={sort} onChange={e => setSort(e.target.value)}><option value="name">Name</option><option value="activity">Recent remote push</option></select></label>
    </div>
    <div className="workspace">
      <section aria-label="Repository library" className="project-list" aria-busy={loading}>
        {loading ? <p className="empty" role="status">Loading registry…</p> : !visible.length ? <div className="empty"><h3>No matching repositories</h3><p>Adjust filters, scan a local directory, or connect GitHub and refresh its repository list.</p></div> : <ul>{visible.map(row => <li key={row.key}>
          <button className={`project ${(row.project ? selectedId === row.project.id : !selectedId && remoteId === row.remote?.repository.id) ? 'selected' : ''}`} onClick={() => { setSelectedId(row.project?.id ?? null); setRemoteId(row.remote?.repository.id ?? null); setRemoveId(null); }}>
            <span className="project-heading"><strong>{row.remote?.repository.full_name ?? row.project?.name}</strong><span className="badge">{row.remote?.repository.visibility ?? 'Visibility unknown'}</span></span>
            <span>{row.state}{row.project ? ` · ${condition(row.project)}` : ''}</span>
            {row.state.includes('uncertain') && <span className="error-text">Multiple or historical remote identities need review. No association was selected.</span>}
            {row.project && <><span className="path">{row.project.path}</span><span>Local branch: {row.project.inspection?.detached ? 'Detached HEAD' : row.project.inspection?.branch ?? 'Unavailable'}</span><span className="subtle">Local inspection: {time(row.project.inspected_at)}</span></>}
            {row.remote && <><span>{row.remote.repository.description}</span><span>Remote default branch: {row.remote.repository.default_branch ?? 'None'}</span><span className="subtle">Provider updated: {row.remote.repository.updated_at} · Cached: {time(row.remote.fetched_at)}</span>{row.remote.availability === 'not_returned' && <span className="error-text">Remote inaccessible or no longer returned for this account. Cached history retained.</span>}</>}
            {row.project && row.project.availability !== 'available' && <span className="error-text">Local metadata may be stale. Inspect for current status.</span>}
          </button>
        </li>)}</ul>}
      </section>
      <aside aria-label="Project details" className="detail">
        {selectedRemote && <section aria-label="GitHub repository details">
          <h2>{selectedRemote.repository.full_name}</h2><p>{selectedRemote.repository.description}</p>
          <p>{selectedRemote.repository.visibility} · {selectedRemote.repository.fork ? 'Fork' : 'Original repository'} · {selectedRemote.repository.archived ? 'Archived on GitHub' : 'Not archived on GitHub'}</p>
          <button disabled={pending} onClick={() => void act(() => api.openGithub(selectedRemote.repository.id, library?.cache.account_id ?? null))}>Open on GitHub</button>
          <dl><dt>Owner</dt><dd>{selectedRemote.repository.owner}</dd><dt>Default branch</dt><dd>{selectedRemote.repository.default_branch ?? 'None'}</dd><dt>Last push</dt><dd>{selectedRemote.repository.pushed_at ?? 'None'}</dd><dt>Provider update</dt><dd>{selectedRemote.repository.updated_at}</dd><dt>Metadata fetched</dt><dd>{time(selectedRemote.fetched_at)} via {selectedRemote.last_account.login}</dd><dt>Account last saw</dt><dd>{time(selectedRemote.last_seen_at)}</dd><dt>Access at refresh</dt><dd>{selectedRemote.availability === 'seen' ? 'Returned by GitHub' : 'Not returned; reason unknown'}</dd></dl>
          <p className="notice subtle">Cached remote metadata and links do not establish backup coverage or eligibility for workspace removal.</p>
        </section>}
        {!selected ? !selectedRemote && <p className="empty">Select a repository to see its metadata.</p> : <>
          <SyncDetails key={selected.id} project={selected} operation={latest} busy={busy} act={act} />
          <ChangesDetails key={`changes-${selected.id}`} project={selected} operation={latest} busy={busy} act={act} />
          <h2>{selected.name}</h2><p className="path">{selected.path}</p>
          <div className="actions"><button disabled={busy} onClick={() => void act(() => api.openProject(selected.id))}>Open workspace</button><button disabled={busy} onClick={() => void act(() => api.openProject(selected.id, true))}>Reveal in Finder</button><button className="primary" disabled={busy || !system?.git_version} onClick={() => void act(() => api.inspect(selected.id))}>Inspect project</button><button disabled={busy} onClick={() => setRemoveId(selected.id)}>Unregister</button></div>
          {removeId === selected.id && <div className="notice"><p>Remove “{selected.name}” from the registry? Its folder and files will stay on disk.</p><div className="actions"><button disabled={busy} onClick={() => void act(async () => { await api.remove(selected.id); setSelectedId(null); setRemoteId(null); setRemoveId(null); })}>Remove registration</button><button onClick={() => setRemoveId(null)}>Keep</button></div></div>}
          {selected.error && <p role="alert" className="notice error">{selected.error.message} Last successful metadata is retained below.</p>}
          <dl>
            <dt>Availability</dt><dd>{selected.availability}</dd>
            <dt>Branch</dt><dd>{selected.inspection?.detached ? 'Detached HEAD' : selected.inspection?.branch ?? 'Unknown'}</dd>
            <dt>Commit</dt><dd><code>{selected.inspection?.commit ?? 'No commit'}</code></dd>
            <dt>Upstream</dt><dd>{selected.inspection?.upstream ?? 'Not configured'}</dd>
            <dt>Registered</dt><dd>{time(selected.registered_at)}</dd>
            <dt>Last success</dt><dd>{time(selected.inspected_at)}</dd>
          </dl>
          {selected.inspection && <>
            <h3>Working tree</h3><dl className="status-counts"><dt>Staged</dt><dd>{selected.inspection.working_tree.staged}</dd><dt>Unstaged</dt><dd>{selected.inspection.working_tree.unstaged}</dd><dt>Untracked</dt><dd>{selected.inspection.working_tree.untracked}</dd><dt>Conflicted</dt><dd>{selected.inspection.working_tree.conflicted}</dd></dl>
            <h3>Remotes</h3>{!selected.inspection.remotes.length ? <p className="subtle">No remote configured.</p> : <ul className="remotes">{selected.inspection.remotes.map((remote, i) => <li key={i}><strong>{remote.name}</strong><code>{remote.url}</code></li>)}</ul>}
            <p className="subtle">URL credentials, query strings, and fragments are omitted.</p>
            {selected.inspection.warnings.map(warning => <p className="notice subtle" key={warning}>{warning}</p>)}
          </>}
        </>}
      </aside>
    </div>
    <footer>Local inspection and cloning · Explicit fetch, fast-forward pull, push, and staged commits · No workspace deletion · Nested repositories and submodules are not automatically scanned.</footer>
  </main>;
}
