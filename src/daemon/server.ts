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
import { disabledMessage, readConfig } from './config.js';
import { sendAttachment, sendMessage } from './send.js';
import {
  encode,
  isChatGuid,
  lines,
  PROTOCOL_VERSION,
  socketPath,
  type ContactsReply,
  type Envelope,
  type Frame,
  type Request,
  type SendReply,
  type StatusReply,
  type WatchRequest,
} from './protocol.js';

/** Sending is switched off in the config, which is a different thing from failing. */
class SendDisabledError extends Error {}

/** How often to look for new messages when the filesystem says nothing. */
const POLL_MS = 2_000;

/** How long to let a burst of writes settle before querying. */
const SETTLE_MS = 100;

/** Contacts changes are rare, so the index is reloaded on a slow timer. */
const CONTACTS_TTL_MS = 10 * 60 * 1000;

/** How soon to try again when the index came back empty. */
const CONTACTS_RETRY_MS = 10_000;

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
    // A load that came back empty is retried soon rather than held for the full
    // interval: the daemon runs before its grant exists, so the first attempt is
    // expected to fail, and caching that failure makes names stay broken long
    // after the grant is given.
    const ttl =
      this.#contacts !== null && this.#contacts.index.size > 0
        ? CONTACTS_TTL_MS
        : CONTACTS_RETRY_MS;
    if (this.#contacts === null || now - this.#contacts.loadedAt > ttl) {
      const index = loadContacts();
      if (index.problems.length > 0) {
        process.stderr.write(`msgd contacts: ${index.problems.join(' | ')}\n`);
      }
      this.#contacts = { index, loadedAt: now };
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
    } catch (error) {
      // A watcher that stops receiving with a live connection looks like a
      // quiet conversation. Say so instead.
      this.#failWatchers(error);
      return;
    }
    // Rowids only climb, so one query answers "is there anything new" for
    // every watcher at once. This is the reason to have a daemon at all. The
    // question is per watcher, though: one that is still behind has messages
    // waiting even when nothing has arrived since the last tick.
    for (const watcher of this.#watchers) {
      if (watcher.watermark < latest) this.#deliver(watcher, latest);
    }
  }

  /**
   * Send everything the watcher has not seen, a batch at a time.
   *
   * Draining matters: a single batch is capped, and stopping after one would
   * strand the rest until another message happened to arrive.
   */
  #deliver(watcher: Watcher, latest: number): void {
    while (watcher.watermark < latest) {
      let messages;
      try {
        messages = fetchMessages(this.database(), {
          chatId: watcher.chatId,
          afterRowid: watcher.watermark,
          limit: WATCH_BATCH,
          includeTapbacks: watcher.request.tapbacks === true,
          includeFiltered: watcher.request.unknown === true,
          contacts: watcher.contacts,
          oldestFirst: true,
        });
      } catch (error) {
        this.#failWatcher(watcher, error);
        return;
      }

      for (const message of messages) {
        watcher.watermark = Math.max(watcher.watermark, message.rowid);
        watcher.socket.write(encode({ type: 'item', value: message }));
      }

      // Filters can hide every row in a batch, so advance past what was read
      // rather than only past what was sent, or this loop never ends.
      if (messages.length < WATCH_BATCH) {
        watcher.watermark = latest;
        return;
      }
    }
  }

  #failWatcher(watcher: Watcher, error: unknown): void {
    const denied = error instanceof AccessDeniedError;
    watcher.socket.write(
      encode({
        type: 'error',
        code: denied ? 'access-denied' : 'error',
        message: denied ? DENIED : describe(error),
      }),
    );
    watcher.socket.end();
    this.#watchers.delete(watcher);
  }

  #failWatchers(error: unknown): void {
    for (const watcher of [...this.#watchers]) this.#failWatcher(watcher, error);
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
        code: denied
          ? 'access-denied'
          : error instanceof SendDisabledError
            ? 'send-disabled'
            : 'error',
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
      case 'send': {
        // The config key is checked here rather than in the client, because a
        // check the caller runs on itself is advice rather than a gate (§7).
        if (!readConfig().send) throw new SendDisabledError(disabledMessage());

        // A guid needs no lookup, so a daemon holding Automation but not Full
        // Disk Access can still send. Matched by shape rather than by
        // punctuation: chat names contain semicolons often enough, and treating
        // one as a guid would hand a display name to AppleScript as an address.
        let { chat: guid, chat: name } = request;
        if (!isChatGuid(request.chat)) {
          const chat = resolveChat(
            this.database(),
            request.chat,
            this.contacts(request.names !== false),
          );
          guid = chat.guid;
          name = chat.name;
        }

        if (request.file !== undefined) {
          sendAttachment(guid, request.file.name, Buffer.from(request.file.base64, 'base64'));
        } else {
          const body = request.body ?? '';
          if (body.length === 0) throw new Error('nothing to send');
          sendMessage(guid, body);
        }
        const reply: SendReply = { guid, name };
        return reply;
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
          contactProblems: index.problems,
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
    // Where this watcher starts. There is no shared cursor: each watcher is
    // compared against the current maximum on every tick, so one subscribing
    // mid-burst cannot be skipped by another's progress.
    const watermark = latestRowid(db);

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
