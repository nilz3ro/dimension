import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import { mkdtempSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { initDb, type BridgeDb } from '../src/db.js';

describe('SQLite persistence layer', () => {
  let dir: string;
  let db: BridgeDb;

  beforeEach(() => {
    dir = mkdtempSync(join(tmpdir(), 'bridge-test-'));
    db = initDb(join(dir, 'test.db'));
  });

  afterEach(() => {
    db.close();
    rmSync(dir, { recursive: true, force: true });
  });

  // ── initDb ────────────────────────────────────────────────────────────────

  it('initDb creates tables in temp directory', () => {
    // If we got here without throwing, the tables were created.
    // Verify by running a basic select on each table.
    const session = db.getOrCreateSession('100');
    expect(session).toBeDefined();
  });

  // ── Sessions ──────────────────────────────────────────────────────────────

  it('getOrCreateSession creates new session with UUID', () => {
    const s = db.getOrCreateSession('42');
    expect(s.chat_id).toBe('42');
    expect(s.session_id).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
    );
    expect(s.created_at).toBeTruthy();
  });

  it('getOrCreateSession returns existing session for same chat_id', () => {
    const s1 = db.getOrCreateSession('42');
    const s2 = db.getOrCreateSession('42');
    expect(s1.session_id).toBe(s2.session_id);
    expect(s1.created_at).toBe(s2.created_at);
  });

  it('getSessionBySessionId returns correct session', () => {
    const s = db.getOrCreateSession('42');
    const found = db.getSessionBySessionId(s.session_id);
    expect(found).toBeDefined();
    expect(found!.chat_id).toBe('42');
    expect(found!.session_id).toBe(s.session_id);
  });

  it('getSessionBySessionId returns undefined for unknown id', () => {
    expect(db.getSessionBySessionId('nonexistent')).toBeUndefined();
  });

  it('resetSession rotates session_id for an existing chat', () => {
    const before = db.getOrCreateSession('42');
    const after = db.resetSession('42');

    expect(after.chat_id).toBe('42');
    expect(after.session_id).not.toBe(before.session_id);
    expect(after.session_id).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
    );
    // Chat now maps to the new id, and the old id no longer resolves.
    expect(db.getOrCreateSession('42').session_id).toBe(after.session_id);
    expect(db.getSessionBySessionId(before.session_id)).toBeUndefined();
    expect(db.getSessionBySessionId(after.session_id)?.chat_id).toBe('42');
  });

  it('resetSession creates a session when the chat is new', () => {
    const s = db.resetSession('99');
    expect(s.chat_id).toBe('99');
    expect(db.getSessionBySessionId(s.session_id)?.chat_id).toBe('99');
  });

  it('resetSession drops messages queued under the old session', () => {
    const before = db.getOrCreateSession('42');
    db.queueMessage(before.session_id, 'stale');

    const after = db.resetSession('42');
    expect(db.getQueuedMessages(before.session_id)).toHaveLength(0);
    expect(db.getQueuedMessages(after.session_id)).toHaveLength(0);
  });

  // ── Message queue ─────────────────────────────────────────────────────────

  it('queueMessage + getQueuedMessages returns messages in order', () => {
    const s = db.getOrCreateSession('42');
    db.queueMessage(s.session_id, 'hello');
    db.queueMessage(s.session_id, 'world');

    const msgs = db.getQueuedMessages(s.session_id);
    expect(msgs).toHaveLength(2);
    expect(msgs[0].text).toBe('hello');
    expect(msgs[1].text).toBe('world');
    expect(msgs[0].id).toBeLessThan(msgs[1].id);
  });

  it('clearQueue removes all messages for session', () => {
    const s = db.getOrCreateSession('42');
    db.queueMessage(s.session_id, 'msg1');
    db.queueMessage(s.session_id, 'msg2');

    db.clearQueue(s.session_id);
    expect(db.getQueuedMessages(s.session_id)).toHaveLength(0);
  });

  // ── Invocations ───────────────────────────────────────────────────────────

  it('setInvocationInFlight + getInFlightInvocation returns invocation', () => {
    const s = db.getOrCreateSession('42');
    db.setInvocationInFlight(s.session_id, 'inv-001');

    const inv = db.getInFlightInvocation(s.session_id);
    expect(inv).toBeDefined();
    expect(inv!.invocation_id).toBe('inv-001');
    expect(inv!.status).toBe('in_flight');
    expect(inv!.session_id).toBe(s.session_id);
  });

  it('completeInvocation updates status', () => {
    const s = db.getOrCreateSession('42');
    db.setInvocationInFlight(s.session_id, 'inv-002');
    db.completeInvocation('inv-002');

    // getInFlightInvocation should return undefined (not in_flight anymore)
    expect(db.getInFlightInvocation(s.session_id)).toBeUndefined();
  });

  it('failInvocation updates status to failed', () => {
    const s = db.getOrCreateSession('42');
    db.setInvocationInFlight(s.session_id, 'inv-003');
    db.failInvocation('inv-003');

    expect(db.getInFlightInvocation(s.session_id)).toBeUndefined();
  });

  it('getInFlightInvocation returns undefined when no active invocation', () => {
    const s = db.getOrCreateSession('42');
    expect(db.getInFlightInvocation(s.session_id)).toBeUndefined();
  });

  // ── Cross-session isolation ───────────────────────────────────────────────

  it('multiple sessions are independent', () => {
    const s1 = db.getOrCreateSession('100');
    const s2 = db.getOrCreateSession('200');

    // Different session IDs
    expect(s1.session_id).not.toBe(s2.session_id);

    // Queue messages in each session
    db.queueMessage(s1.session_id, 'msg-s1');
    db.queueMessage(s2.session_id, 'msg-s2-a');
    db.queueMessage(s2.session_id, 'msg-s2-b');

    expect(db.getQueuedMessages(s1.session_id)).toHaveLength(1);
    expect(db.getQueuedMessages(s2.session_id)).toHaveLength(2);

    // Clear s1 doesn't affect s2
    db.clearQueue(s1.session_id);
    expect(db.getQueuedMessages(s1.session_id)).toHaveLength(0);
    expect(db.getQueuedMessages(s2.session_id)).toHaveLength(2);

    // Invocations are per-session
    db.setInvocationInFlight(s1.session_id, 'inv-s1');
    expect(db.getInFlightInvocation(s1.session_id)).toBeDefined();
    expect(db.getInFlightInvocation(s2.session_id)).toBeUndefined();
  });

  // ── Edge cases ────────────────────────────────────────────────────────────

  it('getQueuedMessages returns empty array for unknown session', () => {
    expect(db.getQueuedMessages('nonexistent')).toEqual([]);
  });
});
