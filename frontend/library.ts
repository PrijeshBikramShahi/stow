import type { CachedRepository, Library, Project } from './api';
export interface LibraryRow { key: string; project?: Project; remote?: CachedRepository; state: string; candidates: number[] }
export interface Filters { query: string; availability: string; visibility: string; sort: string }
export function libraryRows(library: Library, filters: Filters): LibraryRow[] {
  const linked = new Set<number>();
  const remotes = new Map(library.cache.repositories.map(r => [r.repository.id, r]));
  const links = new Map(library.links.map(l => [l.project_id, l]));
  const rows: LibraryRow[] = library.projects.map(project => {
    const link = links.get(project.id);
    const remote = link?.repository_id ? remotes.get(link.repository_id) : undefined;
    if (remote) linked.add(remote.repository.id);
    return { key: `local-${project.id}`, project, remote, state: remote ? 'Local and remote linked' : link?.state === 'uncertain' ? 'Local, remote link uncertain' : 'Local', candidates: link?.candidates ?? [] };
  });
  for (const remote of remotes.values()) {
    if (!linked.has(remote.repository.id)) rows.push({ key: `remote-${remote.repository.id}`, remote, state: 'Remote only', candidates: [] });
  }
  const query = filters.query.trim().toLocaleLowerCase();
  return rows.filter(row => {
    const repo = row.remote?.repository;
    return `${row.project?.name ?? ''} ${row.project?.path ?? ''} ${row.project?.inspection?.branch ?? ''} ${repo?.full_name ?? ''}`.toLocaleLowerCase().includes(query)
      && (filters.availability === 'all' || (filters.availability === 'local' ? Boolean(row.project) : !row.project))
      && (filters.visibility === 'all' || repo?.visibility === filters.visibility);
  }).sort((a, b) => {
    if (filters.sort === 'activity') {
      const delta = (b.remote?.repository.pushed_at ?? '').localeCompare(a.remote?.repository.pushed_at ?? '');
      if (delta) return delta;
    }
    return (a.remote?.repository.full_name ?? a.project?.name ?? '').localeCompare(b.remote?.repository.full_name ?? b.project?.name ?? '') || a.key.localeCompare(b.key);
  });
}
