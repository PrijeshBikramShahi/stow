import test from 'node:test';
import assert from 'node:assert/strict';
import { syncLabel, lastSuccessfulFetch } from '../frontend/sync.ts';
test('sync labels distinguish cached relations and unsupported states', () => {
 assert.match(syncLabel({relation:'up_to_date',ahead:0,behind:0}), /last fetched state/);
 assert.match(syncLabel({relation:'ahead',ahead:3,behind:0}), /3 commits ahead/);
 assert.match(syncLabel({relation:'behind',ahead:0,behind:2}), /2 commits behind/);
 assert.match(syncLabel({relation:'diverged',ahead:3,behind:2}), /diverged/);
 assert.match(syncLabel({relation:'no_upstream'}), /No upstream/);
 assert.match(syncLabel({relation:'missing_tracking_ref'}), /missing/);
 assert.match(syncLabel({relation:'detached'}), /Detached/);
 assert.match(syncLabel({relation:'ambiguous_upstream'}), /ambiguous/);
});
test('fetch freshness requires verified success for the selected remote', () => {
 const history=[{remote:'origin',url:'https://github.com/a/b.git',state:'cancelled',phase:'applying_refs'}, {remote:'other',state:'completed',phase:'verified'}, {remote:'origin',url:'https://github.com/a/b.git',state:'running',phase:'verifying'}, {remote:'origin',url:'https://github.com/a/b.git',state:'completed',phase:'verified',completed_at:10}];
 assert.equal(lastSuccessfulFetch(history,'origin','https://github.com/a/b.git')?.completed_at,10);
 assert.equal(lastSuccessfulFetch(history,'origin','https://github.com/a/changed.git'),undefined);
 assert.equal(lastSuccessfulFetch(history,'origin',null),undefined);
 assert.equal(lastSuccessfulFetch(history.slice(0,3),'origin','https://github.com/a/b.git'),undefined);
});

import {pullReason, pullLabel, pendingPull} from '../frontend/sync.ts';
const eligible = () => ({identity:{},inspection:{branch:'main',commit:'abc',detached:false,working_tree:{staged:0,unstaged:0,untracked:0,conflicted:0}},operations:[],relation:'behind',ahead:0,behind:3,upstream_ref:'refs/remotes/origin/main',remotes:[{name:'origin',reason:null}]});
test('pull eligibility and confirmation are explicit and scoped to an active pull', () => {
 const a=eligible();
 assert.equal(pullReason(a),null);
 assert.match(pullLabel(a),/3 commits available to pull/);
 for (const field of ['staged','unstaged','untracked','conflicted']) {const dirty=eligible();dirty.inspection.working_tree[field]=1;assert.match(pullReason(dirty),/Uncommitted changes/);}
 assert.match(pullReason({...a,relation:'diverged'}),/histories have diverged/);
 assert.match(pullReason({...a,relation:'ahead'}),/ahead only/);
 assert.match(pullReason({...a,identity:null}),/verified/);
 assert.match(pullReason({...a,operations:['index.lock']}),/lock/);
 assert.match(pullLabel({...a,relation:'up_to_date',behind:0}),/Already up to date/);
 const jobs=[{id:'pull1',state:'running',phase:'awaiting_confirmation'}];
 assert.equal(pendingPull(jobs,{kind:'pull',state:'running'})?.id,'pull1');
 assert.equal(pendingPull(jobs,{kind:'pull',state:'requires_inspection'}),undefined);
 assert.equal(pendingPull(jobs,{kind:'fetch',state:'running'}),undefined);
 assert.equal(pendingPull([{...jobs[0],state:'requires_inspection'}],{kind:'pull',state:'running'}),undefined);
});
