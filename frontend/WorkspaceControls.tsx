import { useEffect, useState } from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import { api, errorMessage, type RemoteRepository, type WorkspaceSettings } from './api';
import { canRecover } from './workspace';

export function WorkspaceControls({ settings, repository, accountId, disabled, act }: {
  settings: WorkspaceSettings; repository?: RemoteRepository; accountId: number | null;
  disabled: boolean; act: (action: () => Promise<unknown>) => Promise<void>;
}) {
  const [root, setRoot] = useState(settings.preferences.root ?? '');
  const [editor, setEditor] = useState(settings.preferences.editor);
  const [destinationRoot, setDestinationRoot] = useState(settings.preferences.root ?? '');
  const [name, setName] = useState(repository?.name ?? '');
  const [validation, setValidation] = useState('');
  useEffect(() => { setRoot(settings.preferences.root ?? ''); setEditor(settings.preferences.editor); }, [settings.preferences.root, settings.preferences.editor]);
  useEffect(() => { setName(repository?.name ?? ''); setDestinationRoot(settings.preferences.root ?? ''); setValidation(''); }, [repository?.id, settings.preferences.root]);
  async function choose(set: (value: string) => void) {
    await act(async () => { const result = await open({ directory: true, multiple: false, title: 'Choose an existing workspace directory' }); if (typeof result === 'string') { set(result); setValidation(''); } });
  }
  const unfinished = settings.jobs.filter(job => job.state !== 'completed');
  const editorFailures = settings.jobs.filter(job => job.state === 'completed' && job.editor_error).slice(0, 5);
  return <section className="notice" aria-label="Clone and workspace settings">
    <h2>Clone &amp; Open</h2>
    <details><summary>Workspace preferences</summary>
      <label>Default workspace directory<input value={root} placeholder="Choose an existing directory" onChange={e => setRoot(e.target.value)} disabled={disabled} /></label>
      <div className="actions"><button disabled={disabled} onClick={() => void choose(setRoot)}>Choose default directory</button><button disabled={disabled} onClick={() => setRoot('')}>Clear default</button></div>
      <label>Preferred editor<select value={editor} disabled={disabled} onChange={e => setEditor(e.target.value)}>{settings.editors.map(item => <option value={item.id} key={item.id} disabled={!item.available && item.id !== settings.preferences.editor}>{item.name}{item.available ? '' : ' — not detected'}</option>)}</select></label>
      <button disabled={disabled} onClick={() => void act(() => api.savePreferences({ root: root || null, editor }))}>Save preferences</button>
      <p className="subtle">Editors are detected in Applications and your home Applications folder. Stow requests a new window with extensions disabled. Review editor workspace-trust prompts before allowing repository tasks.</p>
    </details>
    {repository ? <div>
      <h3>Clone {repository.full_name}</h3>
      <label>Destination root<input disabled={disabled} value={destinationRoot} onChange={e => { setDestinationRoot(e.target.value); setValidation(''); }} /></label>
      <button disabled={disabled} onClick={() => void choose(setDestinationRoot)}>Choose clone destination</button>
      <label>New folder name<input disabled={disabled} value={name} onChange={e => { setName(e.target.value); setValidation(''); }} /></label>
      <p className="path">{destinationRoot && name ? `${destinationRoot}/${name}` : 'Choose a destination and folder name.'}</p>
      <div className="actions">
        <button disabled={disabled || !destinationRoot || !name} onClick={() => void act(async () => { try { const result = await api.validateDestination(destinationRoot, name); setValidation(`Available now: ${result.destination}. Stow checks again when cloning and before promotion.`); } catch (error) { setValidation(errorMessage(error)); throw error; } })}>Check destination</button>
        <button className="primary" disabled={disabled || !destinationRoot || !name} onClick={() => void act(() => api.clone(repository.id, accountId, destinationRoot, name, true))}>Clone &amp; Open</button>
        <button disabled={disabled || !destinationRoot || !name} onClick={() => void act(() => api.clone(repository.id, accountId, destinationRoot, name, false))}>Clone only</button>
      </div>
      {validation && <p role="status">{validation}</p>}
      <p className="subtle">Choose an existing directory owned by you, without symlink components or group/world write permission. Existing destinations are never replaced. Failed staging data stays on disk for review. Submodules and Git LFS content are not downloaded.</p>
    </div> : <p className="subtle">Select a remote-only repository to clone it. Open registered workspaces from their details.</p>}
    {unfinished.length > 0 && <details open><summary>{unfinished.length} unfinished clone{unfinished.length === 1 ? '' : 's'} — data retained</summary>
      {unfinished.map(job => <article key={job.id} className="notice">
        <strong>{job.repository.full_name} · {job.state === 'running' ? 'In progress or interrupted' : job.state}</strong>
        <p>Last durable step: {job.phase}</p><p className="path">Destination: {job.destination}</p><p className="path">Staging: {job.root}/{job.staging}</p>
        {job.error && <p role="alert">{job.error.message}</p>}
        <p>{canRecover(job) ? 'Finish rechecks the verified workspace and safely completes promotion or registration. Changed data will be left for manual review.' : 'This attempt has not completed verification. Review retained data and start a new clone. Stow will not promote or delete incomplete data.'}</p>
        <div className="actions"><button disabled={disabled} onClick={() => void act(() => api.revealClone(job.id))}>Reveal retained data</button>{canRecover(job) && <button disabled={disabled} onClick={() => void act(() => api.recoverClone(job.id))}>Finish verified clone</button>}</div>
      </article>)}
    </details>}
    {editorFailures.map(job => <p key={job.id} role="status">Clone saved at {job.destination}. {job.editor_error?.message} {job.project_id && <button disabled={disabled} onClick={() => void act(() => api.openProject(job.project_id!, true))}>Reveal in Finder</button>}</p>)}
  </section>;
}
