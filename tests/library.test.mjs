import { test } from 'node:test';
import assert from 'node:assert/strict';
import { libraryRows } from '../frontend/library.ts';
const local = { id: 1, name: 'workspace', path: '/tmp/workspace', inspection: { branch: 'work' } };
const remote = (id, owner, name, visibility, pushed_at) => ({ repository: { id, owner, name, full_name: `${owner}/${name}`, visibility, pushed_at } });
const fixture = { projects: [local], links: [{ project_id: 1, repository_id: 10, state: 'linked', candidates: [10] }], cache: { repositories: [remote(10, 'alice', 'one', 'private', '2026-01-01T00:00:00Z'), remote(20, 'org', 'two', 'public', '2026-02-01T00:00:00Z')] } };
const filters = { query: '', availability: 'all', visibility: 'all', sort: 'name' };
test('unified rows retain a local workspace and do not duplicate its remote', () => {
  const rows = libraryRows(fixture, filters); assert.equal(rows.length, 2); assert.equal(rows[0].project.id, 1); assert.equal(rows[0].remote.repository.id, 10); assert.equal(rows[1].state, 'Remote only');
});
test('search supports owner, name, local path and branch', () => {
  for (const query of [' ALICE ', 'one', '/tmp/workspace', 'work']) assert.equal(libraryRows(fixture, { ...filters, query })[0].project.id, 1);
  assert.equal(libraryRows(fixture, { ...filters, query: 'org' })[0].remote.repository.id, 20);
  assert.equal(libraryRows(fixture, { ...filters, query: 'missing' }).length, 0);
});
test('availability and visibility filters compose', () => {
  assert.equal(libraryRows(fixture, { ...filters, availability: 'local' }).length, 1);
  assert.equal(libraryRows(fixture, { ...filters, availability: 'remote' })[0].remote.repository.id, 20);
  assert.equal(libraryRows(fixture, { ...filters, visibility: 'private' })[0].project.id, 1);
  assert.equal(libraryRows(fixture, { ...filters, availability: 'local', visibility: 'public' }).length, 0);
});
test('recent activity sorts newest first and missing push dates last', () => {
  assert.equal(libraryRows(fixture, { ...filters, sort: 'activity' })[0].remote.repository.id, 20);
  const library = structuredClone(fixture); library.cache.repositories[1].repository.pushed_at = null;
  assert.equal(libraryRows(library, { ...filters, sort: 'activity' })[0].remote.repository.id, 10);
});
test('ambiguous link preserves separate local and remote identities', () => {
  const library = structuredClone(fixture); library.links[0] = { project_id: 1, repository_id: null, candidates: [10, 20], state: 'uncertain' };
  const rows = libraryRows(library, filters); assert.equal(rows.length, 3); assert.equal(rows.find(r => r.project).state, 'Local, remote link uncertain'); assert.equal(rows.filter(r => !r.project).length, 2);
});
