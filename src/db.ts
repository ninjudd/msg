/** Read-only access to the Messages database. */

import { copyFileSync, existsSync, mkdtempSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { join } from 'node:path';
import { DatabaseSync, type SQLInputValue, type StatementSync } from 'node:sqlite';
import { fromAppleDate, messageBody } from './apple.js';
import { EMPTY_INDEX, nameHandles, type ContactIndex } from './contacts.js';

export const DEFAULT_DB = join(homedir(), 'Library', 'Messages', 'chat.db');

export class AccessDeniedError extends Error {}

export interface Message {
  rowid: number;
  guid: string;
  isFromMe: boolean;
  body: string | null;
  associatedMessageType: number;
  isTapback: boolean;
  date: Date | null;
  handle: string | null;
  contactName: string | null;
  sender: string;
  chatId: number;
  chatName: string | null;
  service: string | null;
}

export interface Chat {
  rowid: number;
  guid: string;
  identifier: string;
  displayName: string | null;
  handles: string | null;
  namedHandles: string | null;
  isFiltered: boolean;
  memberCount: number;
  isGroup: boolean;
  lastDate: Date | null;
  messageCount: number;
  name: string;
}

export function databasePath(override?: string | undefined): string {
  return override ?? process.env['MSG_DB'] ?? DEFAULT_DB;
}

export interface OpenOptions {
  /**
   * Copy the database when the write-ahead log cannot be opened alongside it.
   * A one-shot CLI wants this; a resident daemon does not, since the copy it
   * reads would never see another message arrive.
   */
  allowSnapshot?: boolean | undefined;
}

/**
 * Open the Messages database read-only, falling back to a temporary snapshot
 * when the write-ahead log cannot be opened alongside it.
 */
export function openDatabase(
  path?: string | undefined,
  options: OpenOptions = {},
): DatabaseSync {
  const location = databasePath(path);
  if (!existsSync(location)) {
    throw new AccessDeniedError(`no Messages database at ${location}`);
  }

  try {
    const db = new DatabaseSync(location, { readOnly: true });
    db.prepare('SELECT 1 FROM message LIMIT 1').get();
    return db;
  } catch (error) {
    const message = (error as Error).message.toLowerCase();
    if (message.includes('authorization denied') || message.includes('permission')) {
      throw new AccessDeniedError(deniedMessage(location));
    }
    if (options.allowSnapshot === false) {
      // TCC denial reaches us as SQLite's "unable to open database file", which
      // is indistinguishable from a locked write-ahead log. The CLI resolves
      // that ambiguity by trying a snapshot; a daemon has no snapshot to fall
      // back to, so for it the answer is always the permission.
      throw new AccessDeniedError(`${deniedMessage(location)}\n\n${(error as Error).message}`);
    }
    return openSnapshot(location);
  }
}

/**
 * Two ways out, and the daemon is the better one: it holds the grant instead of
 * the terminal, so nothing else run from that shell inherits it. See
 * docs/projects/all/daemon-and-permissions.md §1.
 */
function deniedMessage(location: string): string {
  return (
    `cannot read ${location}\n\n` +
    'Install the daemon, which holds Full Disk Access so your terminal does not:\n' +
    '  msg daemon install\n\n' +
    'Or grant Full Disk Access to your terminal in System Settings > Privacy & Security >\n' +
    'Full Disk Access, then restart it.'
  );
}

function isPermissionError(error: unknown): boolean {
  const code = (error as NodeJS.ErrnoException).code;
  return code === 'EPERM' || code === 'EACCES';
}

function openSnapshot(location: string): DatabaseSync {
  const directory = mkdtempSync(join(tmpdir(), 'msg-'));
  const copy = join(directory, 'chat.db');
  try {
    for (const suffix of ['', '-wal', '-shm']) {
      if (existsSync(location + suffix)) copyFileSync(location + suffix, copy + suffix);
    }
  } catch (error) {
    // TCC refuses the copy the same way it refused the open, but as an errno
    // rather than a SQLite message. Without this the user meets a raw EPERM.
    if (isPermissionError(error)) throw new AccessDeniedError(deniedMessage(location));
    throw error;
  }
  return new DatabaseSync(copy, { readOnly: true });
}

/** Read every integer as a BigInt, since Apple dates exceed Number.MAX_SAFE_INTEGER. */
function prepare(db: DatabaseSync, sql: string): StatementSync {
  const statement = db.prepare(sql);
  statement.setReadBigInts(true);
  return statement;
}

type Row = Record<string, unknown>;

function asNumber(value: unknown): number {
  return typeof value === 'bigint' ? Number(value) : Number(value ?? 0);
}

function asBigInt(value: unknown): bigint | null {
  return typeof value === 'bigint' ? value : null;
}

function asText(value: unknown): string | null {
  return typeof value === 'string' && value.length > 0 ? value : null;
}

function asBlob(value: unknown): Uint8Array | null {
  return value instanceof Uint8Array ? value : null;
}

const MESSAGE_COLUMNS = `
  message.rowid AS rowid, message.guid AS guid, message.is_from_me AS isFromMe,
  message.text AS text, message.attributedBody AS attributedBody,
  message.associated_message_type AS associatedMessageType, message.date AS date,
  handle.id AS handle, chat_message_join.chat_id AS chatId,
  NULLIF(chat.display_name, '') AS chatName, message.service AS service
`;

const MESSAGE_FROM = `
  FROM message
    LEFT JOIN handle ON message.handle_id = handle.rowid
    JOIN chat_message_join ON chat_message_join.message_id = message.rowid
    JOIN chat ON chat.rowid = chat_message_join.chat_id
`;

function toMessage(row: Row, contacts: ContactIndex): Message {
  const isFromMe = asNumber(row['isFromMe']) === 1;
  const handle = asText(row['handle']);
  const associatedMessageType = asNumber(row['associatedMessageType']);
  const name = contacts.lookup(handle);
  return {
    rowid: asNumber(row['rowid']),
    guid: asText(row['guid']) ?? '',
    isFromMe,
    body: messageBody(asText(row['text']), asBlob(row['attributedBody'])),
    associatedMessageType,
    isTapback: associatedMessageType !== 0,
    date: fromAppleDate(asBigInt(row['date'])),
    handle,
    contactName: name,
    sender: isFromMe ? 'me' : (name ?? handle ?? 'unknown'),
    chatId: asNumber(row['chatId']),
    chatName: asText(row['chatName']),
    service: asText(row['service']),
  };
}

export interface FetchMessagesOptions {
  chatId?: number | undefined;
  afterDate?: bigint | undefined;
  afterRowid?: number | undefined;
  query?: string | undefined;
  limit?: number | undefined;
  includeTapbacks?: boolean | undefined;
  includeFiltered?: boolean | undefined;
  contacts?: ContactIndex | undefined;
  /**
   * Take the oldest matches rather than the newest.
   *
   * Following a conversation wants this: the newest N above a watermark
   * silently skips everything between, so a burst larger than one batch would
   * lose its beginning. Reading wants the default, since a limit there means
   * "the last N".
   */
  oldestFirst?: boolean | undefined;
}

/** Fetch messages newest-first from the database, returned oldest-first. */
export function fetchMessages(db: DatabaseSync, options: FetchMessagesOptions = {}): Message[] {
  const {
    chatId,
    afterDate,
    afterRowid,
    query,
    limit = 50,
    includeTapbacks = false,
    includeFiltered = false,
    contacts = EMPTY_INDEX,
    oldestFirst = false,
  } = options;
  const clauses: string[] = [];
  const params: SQLInputValue[] = [];

  if (chatId !== undefined) {
    clauses.push('chat_message_join.chat_id = ?');
    params.push(BigInt(chatId));
  } else if (!includeFiltered) {
    // Only when sweeping every conversation; naming one is explicit enough.
    clauses.push('chat.is_filtered = 0');
  }
  if (afterDate !== undefined) {
    clauses.push('message.date > ?');
    params.push(afterDate);
  }
  if (afterRowid !== undefined) {
    clauses.push('message.rowid > ?');
    params.push(BigInt(afterRowid));
  }
  if (includeTapbacks !== true) {
    clauses.push('message.associated_message_type = 0');
  }
  if (query !== undefined) {
    // The body lives in attributedBody when text is NULL, so match the raw
    // blob too and filter precisely once decoded.
    clauses.push('(message.text LIKE ? OR CAST(message.attributedBody AS TEXT) LIKE ?)');
    params.push(`%${query}%`, `%${query}%`);
  }

  const where = clauses.length > 0 ? `WHERE ${clauses.join(' AND ')}` : '';
  // Ordering by rowid rather than date when taking the oldest: a watcher walks
  // rowids, and the two orders disagree for messages that arrive out of order.
  const order = oldestFirst ? 'ORDER BY message.rowid ASC' : 'ORDER BY message.date DESC';
  const rows = prepare(
    db,
    `SELECT ${MESSAGE_COLUMNS} ${MESSAGE_FROM} ${where} ${order} LIMIT ?`,
  ).all(...params, BigInt(limit)) as Row[];

  let messages = rows.map((row) => toMessage(row, contacts));
  if (query !== undefined) {
    const needle = query.toLowerCase();
    messages = messages.filter((m) => m.body !== null && m.body.toLowerCase().includes(needle));
  }
  return oldestFirst ? messages : messages.reverse();
}

/** How many chats to consider when a query has to be matched against names. */
const NAME_SEARCH_SCAN = 5_000;

/** Fetch chats ordered by most recent activity. */
export function fetchChats(
  db: DatabaseSync,
  query?: string | undefined,
  limit = 30,
  contacts: ContactIndex = EMPTY_INDEX,
  includeFiltered = false,
): Chat[] {
  const params: SQLInputValue[] = [];
  const conditions: string[] = includeFiltered ? [] : ['isFiltered = 0'];
  let where = '';
  // Contact names live in the Contacts database, so a query that might match
  // one is filtered after the rows are named rather than in SQL.
  const filterByName = query !== undefined && contacts.size > 0;
  if (query !== undefined && !filterByName) {
    conditions.push('(displayName LIKE ? OR chatIdentifier LIKE ? OR handles LIKE ?)');
    params.push(`%${query}%`, `%${query}%`, `%${query}%`);
  }
  if (conditions.length > 0) where = `WHERE ${conditions.join(' AND ')}`;

  const rows = prepare(
    db,
    `
    SELECT * FROM (
      SELECT chat.rowid AS rowid, chat.guid AS guid,
             chat.chat_identifier AS chatIdentifier,
             NULLIF(chat.display_name, '') AS displayName,
             chat.is_filtered AS isFiltered,
             (SELECT GROUP_CONCAT(handle.id) FROM chat_handle_join
                JOIN handle ON handle.rowid = chat_handle_join.handle_id
               WHERE chat_handle_join.chat_id = chat.rowid) AS handles,
             (SELECT COUNT(*) FROM chat_handle_join
               WHERE chat_handle_join.chat_id = chat.rowid) AS memberCount,
             (SELECT MAX(message.date) FROM message
                JOIN chat_message_join ON chat_message_join.message_id = message.rowid
               WHERE chat_message_join.chat_id = chat.rowid) AS lastDate,
             (SELECT COUNT(*) FROM chat_message_join
               WHERE chat_message_join.chat_id = chat.rowid) AS messageCount
        FROM chat
    ) ${where}
    ORDER BY lastDate DESC
    LIMIT ?
  `,
  ).all(...params, BigInt(filterByName ? NAME_SEARCH_SCAN : limit)) as Row[];

  const chats = rows.map((row) => {
    const displayName = asText(row['displayName']);
    const handles = asText(row['handles']);
    const identifier = asText(row['chatIdentifier']) ?? '';
    const memberCount = asNumber(row['memberCount']);
    const namedHandles = nameHandles(contacts, handles);
    return {
      rowid: asNumber(row['rowid']),
      guid: asText(row['guid']) ?? '',
      identifier,
      displayName,
      handles,
      namedHandles,
      // Messages sorts filtered conversations into categories, so any nonzero
      // value means the conversation is filtered.
      isFiltered: asNumber(row['isFiltered']) !== 0,
      memberCount,
      isGroup: memberCount > 1,
      lastDate: fromAppleDate(asBigInt(row['lastDate'])),
      messageCount: asNumber(row['messageCount']),
      name: displayName ?? namedHandles ?? identifier,
    };
  });

  if (!filterByName) return chats;

  const needle = query.toLowerCase();
  const matches = (value: string | null): boolean =>
    value !== null && value.toLowerCase().includes(needle);
  return chats
    .filter((c) => matches(c.name) || matches(c.displayName) || matches(c.handles) || matches(c.identifier))
    .slice(0, limit);
}

/** Find a single chat by rowid, identifier, or name substring. */
export function resolveChat(
  db: DatabaseSync,
  spec: string,
  contacts: ContactIndex = EMPTY_INDEX,
): Chat {
  // Naming a chat outright reaches it even when Messages filters it.
  const matches = /^\d+$/.test(spec)
    ? fetchChats(db, undefined, 10_000, contacts, true).filter((c) => c.rowid === Number(spec))
    : fetchChats(db, spec, 50, contacts, true);

  if (matches.length === 0) throw new Error(`no chat matching ${spec}`);
  if (matches.length === 1) return matches[0] as Chat;

  const exact = matches.filter((c) => c.name.toLowerCase() === spec.toLowerCase());
  if (exact.length === 1) return exact[0] as Chat;

  const names = matches
    .slice(0, 6)
    .map((c) => `${c.name} (${c.rowid})`)
    .join(', ');
  throw new Error(`${matches.length} chats match ${spec}: ${names}`);
}

/** The highest message rowid, used as a watermark when following new messages. */
export function latestRowid(db: DatabaseSync): number {
  const row = prepare(db, 'SELECT MAX(rowid) AS max FROM message').get() as Row | undefined;
  return row === undefined ? 0 : asNumber(row['max']);
}
