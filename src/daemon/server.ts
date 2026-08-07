/**
 * The daemon. It is the only process holding Full Disk Access, so it answers a
 * deliberately small set of questions and takes no filesystem path from anyone:
 * see docs/projects/all/daemon-and-permissions.md §6.
 */

import { chmodSync, existsSync, mkdirSync, rmSync, watch, type FSWatcher } from 'node:fs';
import { createServer, type Server, type Socket } from 'node:net';
import { dirname } from 'node:path';
import { createInterface } from 'node:readline';
import type { DatabaseSync } from 'node:sqlite';
import { sinceToAppleDate } from '../apple.js';
import { EMPTY_INDEX, loadContacts, type ContactIndex } from '../contacts.js';
import {
  AccessDeniedError,
  databasePath,
  fetchChats,
  fetchMessages,
  latestRowid,
  openDatabase,
  resolveChat,
} from '../db.js';
import { VERSION } from '../version.js';
import {
  encode,
  lines,
  PROTOCOL_VERSION,
  socketPath,
  type ContactsReply,
  type Envelope,
  type Frame,
  type Request,
  type StatusReply,
  type WatchRequest,
} from './protocol.js';

/** How often to look for new messages when the filesystem says nothing. */
const POLL_MS = 2_000;

/** How long to let a burst of writes settle before querying. */
const SETTLE_MS = 100;

/** Contacts changes are rare, so the index is reloaded on a slow timer. */
const CONTACTS_TTL_MS = 10 * 60 * 1000;

/** How many new messages a single tick will hand to one watcher. */
const WATCH_BATCH = 200;

interface Watcher {
  socket: Socket;
  request: WatchRequest;
  chatId: number | undefined;
  contacts: ContactIndex;
  watermark: number;
}

export interface DaemonOptions {
  /** Overrides the database the daemon reads. Set by its own environment, never by a client. */
  dbPath?: string | undefined;
}

export class Daemon {
  readonly #dbPath: string | undefined;
  readonly #startedAt = Date.now();
  readonly #watchers = new Set<Watcher>();
  #db: DatabaseSync | null = null;
  #contacts: { index: ContactIndex; loadedAt: number } | null = null;
  #server: Server | null = null;
  #timer: NodeJS.Timeout | null = null;
  #fsWatcher: FSWatcher | null = null;
  #settle: NodeJS.Timeout | null = null;
  #lastSeen = 0;

  constructor(options: DaemonOptions = {}) {
    this.#dbPath = options.dbPath;
  }

  /**
   * Open the database, or throw.
   *
   * A failure is not cached. The install flow depends on that: the daemon runs
   * before the grant exists, fails, and that failure is what creates the entry
   * in the Full Disk Access list (§9). It has to start working once the switch
   * is flipped, without a restart.
   */
  database(): DatabaseSync {
    // A snapshot copy would be frozen at the moment it was taken, which is
    // wrong for a process that stays up and streams new messages.
    this.#db ??= openDatabase(this.#dbPath, { allowSnapshot: false });
    return this.#db;
  }

  contacts(wanted: boolean): ContactIndex {
    if (!wanted) return EMPTY_INDEX;
    const now = Date.now();
    if (this.#contacts === null || now - this.#contacts.loadedAt > CONTACTS_TTL_MS) {
      this.#contacts = { index: loadContacts(), loadedAt: now };
    }
    return this.#contacts.index;
  }

  async listen(path = socketPath()): Promise<string> {
    // sun_path is 104 bytes on macOS, and the kernel reports a bare EINVAL.
    if (Buffer.byteLength(path) > 103) {
      throw new Error(`socket path is too long for macOS (${String(path.length)} > 103): ${path}`);
    }
    const directory = dirname(path);
    mkdirSync(directory, { recursive: true, mode: 0o700 });
    // A socket left behind by a killed daemon would block the bind.
    if (existsSync(path)) rmSync(path);

    const server = createServer((socket) => void this.#accept(socket));
    this.#server = server;
    await new Promise<void>((resolve, reject) => {
      server.once('error', reject);
      server.listen(path, resolve);
    });
    // The kernel restricts the socket to this uid, which is the whole of the
    // access control the daemon has or wants (§5).
    chmodSync(path, 0o600);

    this.#timer = setInterval(() => this.#tick(), POLL_MS);
    this.#timer.unref();
    this.#watchDatabaseFile();
    return path;
  }

  async close(): Promise<void> {
    if (this.#timer !== null) clearInterval(this.#timer);
    if (this.#settle !== null) clearTimeout(this.#settle);
    this.#fsWatcher?.close();
    for (const watcher of this.#watchers) watcher.socket.destroy();
    this.#watchers.clear();
    this.#db?.close();
    this.#db = null;
    const server = this.#server;
    if (server !== null) await new Promise<void>((resolve) => server.close(() => resolve()));
    this.#server = null;
  }

  get watcherCount(): number {
    return this.#watchers.size;
  }

  /**
   * New messages land in the write-ahead log, so a change there is the earliest
   * signal available without polling. The timer stays as a safety net, since a
   * missed event would otherwise stall a watcher indefinitely.
   */
  #watchDatabaseFile(): void {
    try {
      this.#fsWatcher = watch(dirname(databasePath(this.#dbPath)), () => {
        if (this.#settle !== null) clearTimeout(this.#settle);
        this.#settle = setTimeout(() => this.#tick(), SETTLE_MS);
        this.#settle.unref();
      });
    } catch {
      // Unwatchable directory: the poll timer alone still delivers.
    }
  }

  #tick(): void {
    if (this.#watchers.size === 0) return;
    let latest: number;
    try {
      latest = latestRowid(this.database());
    } catch {
      return;
    }
    // Rowids only climb, so one query answers "is there anything new" for
    // every watcher at once. This is the reason to have a daemon at all.
    if (latest <= this.#lastSeen) return;
    this.#lastSeen = latest;

    for (const watcher of this.#watchers) this.#deliver(watcher);
  }

  #deliver(watcher: Watcher): void {
    let messages;
    try {
      messages = fetchMessages(this.database(), {
        chatId: watcher.chatId,
        afterRowid: watcher.watermark,
        limit: WATCH_BATCH,
        includeTapbacks: watcher.request.tapbacks === true,
        includeFiltered: watcher.request.unknown === true,
        contacts: watcher.contacts,
      });
    } catch {
      return;
    }
    for (const message of messages) {
      watcher.watermark = Math.max(watcher.watermark, message.rowid);
      watcher.socket.write(encode({ type: 'item', value: message }));
    }
  }

  async #accept(socket: Socket): Promise<void> {
    socket.on('error', () => socket.destroy());
    const reader = createInterface({ input: socket, crlfDelay: Infinity });
    for await (const line of lines(reader)) {
      await this.#serve(socket, line);
      // One request per connection; a watch leaves the socket open behind us.
      break;
    }
    reader.close();
  }

  async #serve(socket: Socket, line: string): Promise<void> {
    const send = (frame: Frame): void => void socket.write(encode(frame));
    let envelope: Envelope;
    try {
      envelope = JSON.parse(line) as Envelope;
    } catch {
      send({ type: 'error', code: 'error', message: 'malformed request' });
      socket.end();
      return;
    }

    if (envelope.v !== PROTOCOL_VERSION) {
      send({
        type: 'error',
        code: 'version',
        message:
          `msgd speaks protocol ${PROTOCOL_VERSION}, this client speaks ${String(envelope.v)}. ` +
          'Reinstall the daemon with `msg daemon install`.',
      });
      socket.end();
      return;
    }

    try {
      if (envelope.cmd === 'watch') {
        this.#subscribe(socket, envelope);
        return;
      }
      send({ type: 'result', value: this.#answer(envelope) });
    } catch (error) {
      const denied = error instanceof AccessDeniedError;
      send({
        type: 'error',
        code: denied ? 'access-denied' : 'error',
        message: denied ? DENIED : describe(error),
      });
    }
    socket.end();
  }

  #answer(request: Exclude<Request, WatchRequest>): unknown {
    switch (request.cmd) {
      case 'chats': {
        const contacts = this.contacts(request.names !== false);
        return fetchChats(
          this.database(),
          request.query,
          request.limit ?? 30,
          contacts,
          request.unknown === true,
        );
      }
      case 'read': {
        const db = this.database();
        const contacts = this.contacts(request.names !== false);
        const chat = resolveChat(db, request.chat, contacts);
        const messages = fetchMessages(db, {
          chatId: chat.rowid,
          limit: request.limit ?? 50,
          includeTapbacks: request.tapbacks === true,
          contacts,
          afterDate: request.since === undefined ? undefined : sinceToAppleDate(request.since),
        });
        return { chat, messages };
      }
      case 'search': {
        const db = this.database();
        const contacts = this.contacts(request.names !== false);
        const chatId =
          request.chat === undefined ? undefined : resolveChat(db, request.chat, contacts).rowid;
        return fetchMessages(db, {
          query: request.query,
          chatId,
          limit: request.limit ?? 25,
          contacts,
          includeFiltered: request.unknown === true,
          afterDate: request.since === undefined ? undefined : sinceToAppleDate(request.since),
        });
      }
      case 'resolve': {
        const contacts = this.contacts(request.names !== false);
        return resolveChat(this.database(), request.chat, contacts);
      }
      case 'contacts': {
        const index = this.contacts(true);
        const reply: ContactsReply = {
          size: index.size,
          resolved: request.handles.map((handle) => ({ handle, name: index.lookup(handle) })),
        };
        return reply;
      }
      case 'status': {
        const index = this.contacts(true);
        const reply: StatusReply = {
          version: VERSION,
          protocol: PROTOCOL_VERSION,
          pid: process.pid,
          uptimeSeconds: Math.round((Date.now() - this.#startedAt) / 1000),
          database: databasePath(this.#dbPath),
          messageCount: latestRowid(this.database()),
          contactCount: index.size,
          watchers: this.#watchers.size,
        };
        return reply;
      }
    }
  }

  #subscribe(socket: Socket, request: WatchRequest): void {
    const db = this.database();
    const contacts = this.contacts(request.names !== false);
    const chatId =
      request.chat === undefined ? undefined : resolveChat(db, request.chat, contacts).rowid;
    const watermark = latestRowid(db);
    this.#lastSeen = Math.max(this.#lastSeen, watermark);

    const watcher: Watcher = { socket, request, chatId, contacts, watermark };
    this.#watchers.add(watcher);
    const drop = (): void => void this.#watchers.delete(watcher);
    socket.on('close', drop);
    socket.on('error', drop);
  }
}

/**
 * The client holds no grant and cannot be given one, so a denied read is always
 * fixed on the daemon's side. The delay is worth mentioning: the Full Disk
 * Access list took minutes to show a new entry during the spike (§9).
 */
const DENIED =
  'msgd cannot read the Messages database.\n' +
  'Grant Full Disk Access to msgd in System Settings > Privacy & Security > Full Disk Access,\n' +
  'then try again. `msg daemon status` prints the path to add, and a new entry can take a\n' +
  'minute to appear in that list.';

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
