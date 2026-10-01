// What the app does with a uwurdp://connect/<host-id> link, without a window:
//
//   pnpm test
//
// Node runs src/lib/link.ts as it is (type stripping), so this needs no
// test framework.

import assert from 'node:assert/strict';
import { test } from 'node:test';
import { resolveLink } from '../src/lib/link.ts';

const ID = '0f8b2c4e-6a1d-4c3b-9e7f-112233445566';
const host = { id: ID, name: 'Buchhaltung' };
const other = { id: '5d0c7a2b-1e3f-4a6b-8c9d-aabbccddeeff', name: 'Lager' };

/** Steps that record what was asked, with a host list that `sync` may change. */
function steps({ locked = false, opens = true, before = [], after = before, sync = 'done' } = {}) {
  const calls = [];
  let list = before;
  return {
    calls,
    vaultLocked: async () => (calls.push('vault'), locked),
    unlock: async () => (calls.push('unlock'), opens),
    hosts: async () => (calls.push('hosts'), list),
    sync: async () => {
      calls.push('sync');
      if (sync === 'done') list = after;
      return sync === 'failed' ? { kind: 'failed', message: 'unreachable' } : { kind: sync };
    },
    onSyncing: () => calls.push('syncing'),
  };
}

test('a known host connects without a sync', async () => {
  const s = steps({ before: [other, host] });
  assert.deepEqual(await resolveLink({ kind: 'connect', id: ID }, s), {
    kind: 'connect',
    host,
  });
  assert.deepEqual(s.calls, ['vault', 'hosts']);
});

test('the id matches in any case', async () => {
  const s = steps({ before: [host] });
  const outcome = await resolveLink({ kind: 'connect', id: ID.toUpperCase() }, s);
  assert.equal(outcome.kind, 'connect');
});

test('a locked vault is opened first, then the host connects', async () => {
  const s = steps({ locked: true, before: [host] });
  assert.equal((await resolveLink({ kind: 'connect', id: ID }, s)).kind, 'connect');
  assert.deepEqual(s.calls, ['vault', 'unlock', 'hosts']);
});

test('a vault that stays locked connects nothing', async () => {
  const s = steps({ locked: true, opens: false, before: [host] });
  assert.deepEqual(await resolveLink({ kind: 'connect', id: ID }, s), { kind: 'locked' });
  assert.deepEqual(s.calls, ['vault', 'unlock']);
});

test('an unknown host is synced first, then connects', async () => {
  const s = steps({ before: [other], after: [other, host] });
  assert.deepEqual(await resolveLink({ kind: 'connect', id: ID }, s), {
    kind: 'connect',
    host,
  });
  assert.deepEqual(s.calls, ['vault', 'hosts', 'syncing', 'sync', 'hosts']);
});

test('still unknown after the sync: says so', async () => {
  const s = steps({ before: [other] });
  assert.deepEqual(await resolveLink({ kind: 'connect', id: ID }, s), {
    kind: 'unknown',
    sync: { kind: 'done' },
  });
});

test('no sync on this device, or a failed one: unknown, with the reason', async () => {
  for (const sync of ['off', 'failed']) {
    const s = steps({ before: [other], after: [host], sync });
    const outcome = await resolveLink({ kind: 'connect', id: ID }, s);
    assert.equal(outcome.kind, 'unknown');
    assert.equal(outcome.sync.kind, sync);
    assert.equal(s.calls.filter((c) => c === 'hosts').length, 1, 'no second look');
  }
});

test('an invalid link touches nothing', async () => {
  const s = steps({ locked: true, before: [host] });
  assert.deepEqual(await resolveLink({ kind: 'invalid' }, s), { kind: 'invalid' });
  assert.deepEqual(s.calls, []);
});
