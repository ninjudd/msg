/**
 * Where the CLI gets its data.
 *
 * The daemon when one is listening, the database directly when it is not. The
 * fallback is deliberate: without it, a machine that has not installed the
 * daemon has no working `msg` at all, and `AccessDeniedError` is the first
 * thing a new user meets.
 */

import { setTimeout as sleep } from 'node:timers/promises';
import type { DatabaseSync } from 'node:sqlite';
import { sinceToAppleDate } from './apple.js';
import { EMPTY_INDEX, loadContacts, type ContactIndex } from './contacts.js';
import {
  fetchChats,
  fetchMessages,
  latestRowid,
  openDatabase,
  resolveChat,
  type Chat,
  type Message,
} from './db.js';
import { connectDaemon, request, stream } from './daemon/client.js';
import {
  reviveChat,
  reviveMessage,
  type ContactsReply,
  type SendReply,
  type StatusReply,
} from './daemon/protocol.js';

export interface ChatsQuery {
  query?: string | undefined;
  limit: number;
  unknown: boolean;
  names: boolean;
}

export interface ReadQuery {
  chat: string;
  limit: number;
  since?: string | undefined;
  tapbacks: boolean;
  names: boolean;
}

export interface SearchQuery {
  query: string;
  chat?: string | undefined;
  limit: number;
  since?: string | undefined;
  unknown: boolean;
  names: boolean;
}

export interface WatchQuery {
  chat?: string | undefined;
  tapbacks: boolean;
  unknown: boolean;
  names: boolean;
  /** Poll frequency for the direct path. The daemon watches the write-ahead log instead. */
  interval: number;
}

export interface SendQuery {
  chat: string;
  body?: string | undefined;
  /** Bytes rather than a path: the daemon never reads a path a client named (§6). */
  file?: { name: string; base64: string } | undefined;
  names: boolean;
}

export interface Source {
  readonly kind: 'daemon' | 'direct';
  chats(query: ChatsQuery): Promise<Chat[]>;
  read(query: ReadQuery): Promise<{ chat: Chat; messages: Message[] }>;
  search(query: SearchQuery): Promise<Message[]>;
  watch(query: WatchQuery, onMessage: (message: Message) => void): Promise<void>;
  resolve(chat: string, names: boolean): Promise<Chat>;
  send(query: SendQuery): Promise<SendReply>;
  contacts(handles: string[]): Promise<ContactsReply>;
  close(): void;
}

/** How many new messages one poll of the direct path will report. */
const WATCH_BATCH = 200;

export function directSource(dbPath?: string | undefined): Source {
  let db: DatabaseSync | null = null;
  let index: ContactIndex | null = null;

  const database = (): DatabaseSync => (db ??= openDatabase(dbPath));
  const contacts = (wanted: boolean): ContactIndex =>
    wanted ? (index ??= loadContacts()) : EMPTY_INDEX;

  return {
    kind: 'direct',

    chats(query) {
      const names = contacts(query.names);
      return Promise.resolve(
        fetchChats(database(), query.query, query.limit, names, query.unknown),
      );
    },

    read(query) {
      const handle = database();
      const names = contacts(query.names);
      const chat = resolveChat(handle, query.chat, names);
      const messages = fetchMessages(handle, {
        chatId: chat.rowid,
        limit: query.limit,
        includeTapbacks: query.tapbacks,
        contacts: names,
        afterDate: query.since === undefined ? undefined : sinceToAppleDate(query.since),
      });
      return Promise.resolve({ chat, messages });
    },

    search(query) {
      const handle = database();
      const names = contacts(query.names);
      const chatId =
        query.chat === undefined ? undefined : resolveChat(handle, query.chat, names).rowid;
      return Promise.resolve(
        fetchMessages(handle, {
          query: query.query,
          chatId,
          limit: query.limit,
          contacts: names,
          includeFiltered: query.unknown,
          afterDate: query.since === undefined ? undefined : sinceToAppleDate(query.since),
        }),
      );
    },

    async watch(query, onMessage) {
      const handle = database();
      const names = contacts(query.names);
      const chatId =
        query.chat === undefined ? undefined : resolveChat(handle, query.chat, names).rowid;
      let watermark = latestRowid(handle);

      for (;;) {
        const messages = fetchMessages(handle, {
          chatId,
          afterRowid: watermark,
          limit: WATCH_BATCH,
          includeTapbacks: query.tapbacks,
          includeFiltered: query.unknown,
          contacts: names,
          oldestFirst: true,
        });
        for (const message of messages) {
          watermark = Math.max(watermark, message.rowid);
          onMessage(message);
        }
        await sleep(query.interval * 1000);
      }
    },

    resolve(chat, wantNames) {
      return Promise.resolve(resolveChat(database(), chat, contacts(wantNames)));
    },

    /**
     * Sending needs Automation, and the CLI deliberately no longer asks for it.
     * A gate the sending process enforces on itself is not a gate, so the only
     * way to send is through a daemon macOS has been told may drive Messages
     * (§7).
     */
    send() {
      return Promise.reject(
        new Error(
          'sending needs the daemon, which holds the Automation permission.\n' +
            'Install it with `msg daemon install`.',
        ),
      );
    },

    contacts(handles) {
      const names = loadContacts();
      return Promise.resolve({
        size: names.size,
        resolved: handles.map((handle) => ({ handle, name: names.lookup(handle) })),
      });
    },

    close() {
      db?.close();
      db = null;
    },
  };
}

export function daemonSource(): Source {
  async function call(message: Parameters<typeof request>[1]): Promise<unknown> {
    const socket = await connectDaemon();
    if (socket === null) throw new Error('msgd stopped listening; try `msg daemon status`');
    return request(socket, message);
  }

  return {
    kind: 'daemon',

    async chats(query) {
      const value = await call({
        cmd: 'chats',
        query: query.query,
        limit: query.limit,
        unknown: query.unknown,
        names: query.names,
      });
      return (value as unknown[]).map(reviveChat);
    },

    async read(query) {
      const value = (await call({
        cmd: 'read',
        chat: query.chat,
        limit: query.limit,
        since: query.since,
        tapbacks: query.tapbacks,
        names: query.names,
      })) as { chat: unknown; messages: unknown[] };
      return { chat: reviveChat(value.chat), messages: value.messages.map(reviveMessage) };
    },

    async search(query) {
      const value = await call({
        cmd: 'search',
        query: query.query,
        chat: query.chat,
        limit: query.limit,
        since: query.since,
        unknown: query.unknown,
        names: query.names,
      });
      return (value as unknown[]).map(reviveMessage);
    },

    async watch(query, onMessage) {
      const socket = await connectDaemon();
      if (socket === null) throw new Error('msgd stopped listening; try `msg daemon status`');
      const messages = stream(socket, {
        cmd: 'watch',
        chat: query.chat,
        tapbacks: query.tapbacks,
        unknown: query.unknown,
        names: query.names,
      });
      for await (const message of messages) onMessage(reviveMessage(message));

      // Reaching the end means the daemon went away — stopped, upgraded, or
      // crashed. `watch` runs until interrupted, so returning quietly here
      // would have a pipeline or supervisor believe it is still following.
      throw new Error('msgd stopped, so watch ended. Check `msg daemon status`.');
    },

    async resolve(chat, names) {
      return reviveChat(await call({ cmd: 'resolve', chat, names }));
    },

    async send(query) {
      return (await call({
        cmd: 'send',
        chat: query.chat,
        body: query.body,
        file: query.file,
        names: query.names,
      })) as SendReply;
    },

    async contacts(handles) {
      return (await call({ cmd: 'contacts', handles })) as ContactsReply;
    },

    close() {
      // Every call owns its own connection, so there is nothing to keep.
    },
  };
}

/** Ask the daemon how it is doing, or null when it is not listening. */
export async function daemonStatus(): Promise<StatusReply | null> {
  const socket = await connectDaemon();
  if (socket === null) return null;
  return (await request(socket, { cmd: 'status' })) as StatusReply;
}

export interface SourceOptions {
  /** `--db` names a path, so it is answered here and never sent to the daemon (§6). */
  db?: string | undefined;
}

export async function openSource(options: SourceOptions = {}): Promise<Source> {
  // MSG_DB is documented as `--db` by another name, so it has to steer the same
  // way. Without this, pointing it at a fixture while a daemon is listening
  // reads the real database instead, which is the opposite of what it is for.
  const database = options.db ?? process.env['MSG_DB'];
  if (database !== undefined) return directSource(database);
  const socket = await connectDaemon();
  if (socket === null) return directSource();
  socket.destroy();
  return daemonSource();
}
