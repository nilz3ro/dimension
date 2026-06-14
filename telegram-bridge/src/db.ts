import Database from 'better-sqlite3';
import { randomUUID } from 'node:crypto';
import type { Session, QueuedMessage, Invocation } from './types.js';

export type BridgeDb = ReturnType<typeof initDb>;

/**
 * Initialise (or open) the SQLite database and return an object exposing
 * all CRUD operations via prepared statements.
 */
export function initDb(dbPath: string) {
  const db = new Database(dbPath);

  // Enable WAL for better concurrent read/write performance.
  db.pragma('journal_mode = WAL');

  // ── Schema ──────────────────────────────────────────────────────────────────

  db.exec(`
    CREATE TABLE IF NOT EXISTS sessions (
      chat_id    TEXT PRIMARY KEY,
      session_id TEXT UNIQUE NOT NULL,
      created_at TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS message_queue (
      id          INTEGER PRIMARY KEY AUTOINCREMENT,
      session_id  TEXT    NOT NULL,
      text        TEXT    NOT NULL,
      received_at TEXT    NOT NULL
    );

    CREATE TABLE IF NOT EXISTS invocations (
      invocation_id TEXT PRIMARY KEY,
      session_id    TEXT    NOT NULL,
      status        TEXT    NOT NULL DEFAULT 'pending',
      created_at    TEXT    NOT NULL
    );
  `);

  // ── Prepared statements ─────────────────────────────────────────────────────

  const stmts = {
    // Sessions
    getSessionByChatId: db.prepare<[string], Session>(
      'SELECT chat_id, session_id, created_at FROM sessions WHERE chat_id = ?',
    ),
    getSessionBySessionId: db.prepare<[string], Session>(
      'SELECT chat_id, session_id, created_at FROM sessions WHERE session_id = ?',
    ),
    insertSession: db.prepare(
      'INSERT INTO sessions (chat_id, session_id, created_at) VALUES (?, ?, ?)',
    ),
    updateSessionId: db.prepare(
      'UPDATE sessions SET session_id = ?, created_at = ? WHERE chat_id = ?',
    ),

    // Message queue
    insertMessage: db.prepare(
      'INSERT INTO message_queue (session_id, text, received_at) VALUES (?, ?, ?)',
    ),
    getMessages: db.prepare<[string], QueuedMessage>(
      'SELECT id, session_id, text, received_at FROM message_queue WHERE session_id = ? ORDER BY received_at ASC, id ASC',
    ),
    clearMessages: db.prepare('DELETE FROM message_queue WHERE session_id = ?'),

    // Invocations
    insertInvocation: db.prepare(
      'INSERT INTO invocations (session_id, invocation_id, status, created_at) VALUES (?, ?, ?, ?)',
    ),
    getInFlight: db.prepare<[string], Invocation>(
      "SELECT session_id, invocation_id, status, created_at FROM invocations WHERE session_id = ? AND status = 'in_flight'",
    ),
    updateStatus: db.prepare(
      'UPDATE invocations SET status = ? WHERE invocation_id = ?',
    ),
  };

  // ── Public API ──────────────────────────────────────────────────────────────

  function getOrCreateSession(chatId: string): Session {
    const existing = stmts.getSessionByChatId.get(chatId);
    if (existing) return existing;

    const sessionId = randomUUID();
    const createdAt = new Date().toISOString();
    stmts.insertSession.run(chatId, sessionId, createdAt);
    return { chat_id: chatId, session_id: sessionId, created_at: createdAt };
  }

  function getSessionBySessionId(sessionId: string): Session | undefined {
    return stmts.getSessionBySessionId.get(sessionId);
  }

  /**
   * Rotate a chat's `session_id` to a fresh UUID, starting a clean conversation.
   * The agent keys all per-session state (issue store, agent state, PDFs) off the
   * `session_id` as its S3 prefix, so a new id means a blank slate. Any messages
   * still queued under the old id are dropped so they can't resurface after the
   * reset. Returns the new {@link Session}. (Backs the `/new` Telegram command.)
   */
  function resetSession(chatId: string): Session {
    const sessionId = randomUUID();
    const createdAt = new Date().toISOString();
    const existing = stmts.getSessionByChatId.get(chatId);
    if (existing) {
      stmts.clearMessages.run(existing.session_id);
      stmts.updateSessionId.run(sessionId, createdAt, chatId);
    } else {
      stmts.insertSession.run(chatId, sessionId, createdAt);
    }
    return { chat_id: chatId, session_id: sessionId, created_at: createdAt };
  }

  function queueMessage(sessionId: string, text: string): void {
    stmts.insertMessage.run(sessionId, text, new Date().toISOString());
  }

  function getQueuedMessages(sessionId: string): QueuedMessage[] {
    return stmts.getMessages.all(sessionId);
  }

  function clearQueue(sessionId: string): void {
    stmts.clearMessages.run(sessionId);
  }

  function setInvocationInFlight(
    sessionId: string,
    invocationId: string,
  ): void {
    stmts.insertInvocation.run(
      sessionId,
      invocationId,
      'in_flight',
      new Date().toISOString(),
    );
  }

  function getInFlightInvocation(sessionId: string): Invocation | undefined {
    return stmts.getInFlight.get(sessionId);
  }

  function completeInvocation(invocationId: string): void {
    stmts.updateStatus.run('completed', invocationId);
  }

  function failInvocation(invocationId: string): void {
    stmts.updateStatus.run('failed', invocationId);
  }

  function close(): void {
    db.close();
  }

  return {
    getOrCreateSession,
    getSessionBySessionId,
    resetSession,
    queueMessage,
    getQueuedMessages,
    clearQueue,
    setInvocationInFlight,
    getInFlightInvocation,
    completeInvocation,
    failInvocation,
    close,
  };
}
