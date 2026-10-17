import test from 'node:test';
import assert from 'node:assert/strict';
import {commitReason,pendingCommit,displayPath} from '../frontend/changes.ts';
test('commit requires staged content, message and known author', () => {
 const r={entries:[{staged:'modified'}],author:{name:'Test',email:'test@example.invalid'}};
 assert.equal(commitReason(r,'A real message'),null);
 assert.match(commitReason(r,'  \n'),/message/);
 assert.match(commitReason({...r,entries:[]},'Message'),/Stage/);
 assert.match(commitReason({...r,author:null},'Message'),/author/);
 assert.match(commitReason(r,'x'.repeat(65537)),/64 KiB/);
 assert.match(commitReason(r,'a\0b'),/NUL/);
});
test('commit confirmation is bound to live confirmed phase and paths remain exact', () => {
 const p={job:{state:'running',phase:'awaiting_confirmation'}};
 assert.equal(pendingCommit(p,{kind:'commit',state:'running'}),p);
 assert.equal(pendingCommit(p,{kind:'commit',state:'completed'}),null);
 assert.equal(pendingCommit(p,{kind:'push',state:'running'}),null);
 assert.equal(pendingCommit({job:{state:'requires_inspection',phase:'awaiting_confirmation'}},{kind:'commit',state:'running'}),null);
 const path='नमस्ते with\ttab\nand newline';assert.equal(JSON.parse(displayPath(path)),path);
});
