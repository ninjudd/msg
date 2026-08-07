/**
 * The wire between `msg` and `msgd`: newline-delimited JSON over a unix socket.
 *
 * One request per connection. The daemon answers with `result` and closes, or
 * with a stream of `item` frames that ends when the client disconnects.
 *
 * No request carries a filesystem path. That is the rule the daemon's whole
 * security value rests on, since it holds Full Disk Access and a path argument
 * would turn it into a general-purpose reader: see
 * docs/projects/all/daemon-and-permissions.md §6.
 */

import { homedir } from 'node:os';
import { join } from 'node:path';
import type { Interface } from 'node:readline';
import type { Chat, Message } from '../db.js';

export const PROTOCOL_VERSION = 1;

/** The launchd job label, and the bundle identifier the TCC grant lands on. */
export const LABEL = 'com.ninjudd.msgd';

/** Owner-only directory holding the socket and the daemon's log. */
export function stateDirectory(): string {
  return process.env['MSG_STATE_DIR'] ?? join(homedir(), '.local', 'state', 'msg');
}

export function socketPath(): string {
  return process.env['MSG_SOCKET'] ?? join(stateDirectory(), 'msgd.sock');
}

export interface ChatsRequest {
  cmd: 'chats';
  query?: string | undefined;
  limit?: number | undefined;
  unknown?: boolean | undefined;
  names?: boolean | undefined;
}

export interface ReadRequest {
  cmd: 'read';
  chat: string;
  limit?: number | undefined;
  since?: string | undefined;
  tapbacks?: boolean | undefined;
  names?: boolean | undefined;
}

export interface SearchRequest {
  cmd: 'search';
  query: string;
  chat?: string | undefined;
  limit?: number | undefined;
  since?: string | undefined;
  unknown?: boolean | undefined;
  names?: boolean | undefined;
}

export interface WatchRequest {
  cmd: 'watch';
  chat?: string | undefined;
  tapbacks?: boolean | undefined;
  unknown?: boolean | undefined;
  names?: boolean | undefined;
}

/**
 * Sending, which needs Automation rather than Full Disk Access.
 *
 * `chat` may be a guid, in which case the daemon addresses it without reading
 * the database at all — so a daemon granted Automation and refused Full Disk
 * Access can still send. An attachment arrives as bytes rather than a path,
 * because a path argument would make the daemon read arbitrary files (§6).
 */
export interface SendRequest {
  cmd: 'send';
  chat: string;
  body?: string | undefined;
  file?: { name: string; base64: string } | undefined;
  names?: boolean | undefined;
}

export interface SendReply {
  guid: string;
  name: string;
}

/** Naming one conversation, which is what `send` needs before it can address it. */
export interface ResolveRequest {
  cmd: 'resolve';
  chat: string;
  names?: boolean | undefined;
}

export interface ContactsRequest {
  cmd: 'contacts';
  handles: string[];
}

export interface StatusRequest {
  cmd: 'status';
}

/**
 * Exercise the Automation permission without sending anything, so the entry in
 * System Settings exists to be switched off.
 */
export interface AutomationRequest {
  cmd: 'automation';
}

export interface AutomationReply {
  allowed: boolean;
  detail: string;
  /** Whether the config would let a send through, independent of macOS. */
  configEnabled: boolean;
}

export type Request =
  | ChatsRequest
  | ReadRequest
  | SearchRequest
  | WatchRequest
  | ResolveRequest
  | SendRequest
  | ContactsRequest
  | AutomationRequest
  | StatusRequest;

/** What the client writes: a request plus the protocol version it speaks. */
export type Envelope = Request & { v: number };

export interface StatusReply {
  version: string;
  protocol: number;
  pid: number;
  uptimeSeconds: number;
  database: string;
  messageCount: number;
  contactCount: number;
  /** Why the contact index is empty, when it is. Empty when all is well. */
  contactProblems: readonly string[];
  watchers: number;
}

export interface ContactsReply {
  size: number;
  resolved: Array<{ handle: string; name: string | null }>;
}

/**
 * `access-denied` is the one code the CLI acts on, since it maps to the exit
 * status the README documents.
 */
export type ErrorCode = 'access-denied' | 'send-disabled' | 'error' | 'version';

export type Frame =
  | { type: 'result'; value: unknown }
  | { type: 'item'; value: unknown }
  | { type: 'error'; code: ErrorCode; message: string };

/**
 * Whether a string is a chat guid rather than a name to look up.
 *
 * Messages writes them as `<service>;<kind>;<identifier>`, as in
 * `iMessage;-;+13105551234` or `iMessage;+;chat9`.
 */
export function isChatGuid(value: string): boolean {
  return /^[A-Za-z][A-Za-z0-9]*;[-+];.+$/.test(value);
}

export function encode(value: Envelope | Frame): string {
  return `${JSON.stringify(value)}\n`;
}

export function decodeFrame(line: string): Frame {
  const value = JSON.parse(line) as Frame;
  if (value.type !== 'result' && value.type !== 'item' && value.type !== 'error') {
    throw new Error(`unexpected frame from msgd: ${line.slice(0, 80)}`);
  }
  return value;
}

/** An error the daemon reported, carrying its code so the CLI can exit on it. */
export class DaemonError extends Error {
  constructor(
    message: string,
    readonly code: ErrorCode,
  ) {
    super(message);
  }
}

/** Dates cross the wire as ISO strings and have to come back as Dates. */
function toDate(value: unknown): Date | null {
  return typeof value === 'string' ? new Date(value) : null;
}

export function reviveMessage(value: unknown): Message {
  const raw = value as Omit<Message, 'date'> & { date: unknown };
  return { ...raw, date: toDate(raw.date) };
}

export function reviveChat(value: unknown): Chat {
  const raw = value as Omit<Chat, 'lastDate'> & { lastDate: unknown };
  return { ...raw, lastDate: toDate(raw.lastDate) };
}

/** Lines from a readline interface, as an async iterable of non-empty strings. */
export async function* lines(reader: Interface): AsyncGenerator<string> {
  for await (const line of reader) {
    const trimmed = line.trim();
    if (trimmed.length > 0) yield trimmed;
  }
}
