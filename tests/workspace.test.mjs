import test from 'node:test';
import assert from 'node:assert/strict';
import { canRecover } from '../frontend/workspace.ts';
test('only verified unfinished clones offer recovery', () => {
  for (const phase of ['reserved', 'staging', 'creating']) assert.equal(canRecover({state:'failed', phase, verified:{}}), false);
  for (const phase of ['verified', 'promoting', 'promoted']) {
    assert.equal(canRecover({state:'failed', phase, verified:{}}), true);
    assert.equal(canRecover({state:'running', phase, verified:{}}), true);
    assert.equal(canRecover({state:'cancelled', phase, verified:{}}), true);
    assert.equal(canRecover({state:'completed', phase, verified:{}}), false);
    assert.equal(canRecover({state:'failed', phase, verified:null}), false);
  }
});
