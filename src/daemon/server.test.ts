/**
 * The daemon end to end, over a real unix socket, against a fixture database.
 *
 * Every request asks for `names: false`, so nothing here reads Contacts or any
 * other database belonging to whoever is running the tests.
 */

import { connect } from 'node:net';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import { DatabaseSync } from 'node:sqlite';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { toAppleDate } from '../apple.js';
import type { Chat, Message } from '../db.js';
import { connectDaemon, request, stream } from './client.js';
import { encode, PROTOCOL_VERSION, type Request, type StatusReply } from './protocol.js';
import { Daemon } from './server.js';

const SCHEMA = `
  CREATE TABLE handle (rowid INTEGER PRIMARY KEY, id TEXT);
  CREATE TABLE chat (
    rowid INTEGER PRIMARY KEY, guid TEXT, chat_identifier TEXT,
    display_name TEXT, is_filtered INTEGER DEFAULT 0
  );
  CREATE TABLE message (
    rowid INTEGER PRIMARY KEY, guid TEXT, text TEXT, attributedBody BLOB,
    is_from_me INTEGER DEFAULT 0, handle_id INTEGER,
    associated_message_type INTEGER DEFAULT 0, date INTEGER, service TEXT
  );
  CREATE TABLE chat_message_join (chat_id INTEGER, message_id INTEGER);
  CREATE TABLE chat_handle_join (chat_id INTEGER, handle_id INTEGER);
`;

/** 2026-01-15T17:30:00Z, plus a minute per message. */
const START = Date.UTC(2026, 0, 15, 17, 30);

function at(minutes: number): bigint {
  return toAppleDate(new Date(START + minutes * 60_000));
}

function buildFixture(path: string): void {
  const db = new DatabaseSync(path);
  db.exec(SCHEMA);

  db.exec(`
    INSERT INTO handle (rowid, id) VALUES (1, '+13105551234'), (2, 'someone@example.com');
    INSERT INTO chat (rowid, guid, chat_identifier, display_name, is_filtered) VALUES
      (1, 'iMessage;-;+13105551234', '+13105551234', '', 0),
      (2, 'iMessage;+;chat9', 'chat9', 'Ship Room', 0),
      (3, 'SMS;-;+18885550000', '+18885550000', '', 1);
    INSERT INTO chat_handle_join (chat_id, handle_id) VALUES (1, 1), (2, 1), (2, 2), (3, 1);
  `);

  const message = db.prepare(
    `INSERT INTO message (rowid, guid, text, is_from_me, handle_id, associated_message_type, date, service)
     VALUES (?, ?, ?, ?, ?, ?, ?, 'iMessage')`,
  );
  const join = db.prepare('INSERT INTO chat_message_join (chat_id, message_id) VALUES (?, ?)');

  const rows: Array<[number, string, string, number, number, number, bigint, number]> = [
    [1, 'm1', 'are you around later', 0, 1, 0, at(0), 1],
    [2, 'm2', 'after 6, yeah', 1, 1, 0, at(1), 1],
    [3, 'm3', 'deploy is green', 0, 2, 0, at(2), 2],
    [4, 'm4', 'your code is 123456', 0, 1, 0, at(3), 3],
    [5, 'm5', 'Liked "after 6, yeah"', 0, 1, 2000, at(4), 1],
  ];
  for (const [rowid, guid, text, fromMe, handle, associated, date, chat] of rows) {
    message.run(rowid, guid, text, fromMe, handle, associated, date);
    join.run(chat, rowid);
  }
  db.close();
}

let directory: string;
let databasePath: string;
let socketPath: string;
let daemon: Daemon;

beforeAll(async () => {
  directory = mkdtempSync(join(tmpdir(), 'msg-daemon-'));
  databasePath = join(directory, 'chat.db');
  socketPath = join(directory, 'msgd.sock');
  buildFixture(databasePath);
  daemon = new Daemon({ dbPath: databasePath });
  await daemon.listen(socketPath);
});

afterAll(async () => {
  await daemon.close();
  rmSync(directory, { recursive: true, force: true });
});

async function ask(message: Request): Promise<unknown> {
  const socket = await connectDaemon(socketPath);
  if (socket === null) throw new Error('daemon not listening');
  return request(socket, message);
}

describe('chats', () => {
  it('lists conversations and hides the filtered ones', async () => {
    const chats = (await ask({ cmd: 'chats', names: false })) as Chat[];
    // Ordered by most recent activity, and chat 3 is the filtered one.
    expect(chats.map((chat) => chat.name)).toEqual(['+13105551234', 'Ship Room']);
  });

  it('includes filtered conversations when asked', async () => {
    const chats = (await ask({ cmd: 'chats', names: false, unknown: true })) as Chat[];
    expect(chats).toHaveLength(3);
    expect(chats.some((chat) => chat.isFiltered)).toBe(true);
  });

  it('filters by name', async () => {
    const chats = (await ask({ cmd: 'chats', query: 'Ship', names: false })) as Chat[];
    expect(chats.map((chat) => chat.rowid)).toEqual([2]);
  });
});

describe('read', () => {
  it('returns a conversation oldest first, without tapbacks', async () => {
    const value = (await ask({ cmd: 'read', chat: '1', names: false })) as {
      chat: Chat;
      messages: Message[];
    };
    expect(value.chat.rowid).toBe(1);
    expect(value.messages.map((message) => message.body)).toEqual([
      'are you around later',
      'after 6, yeah',
    ]);
    expect(value.messages[1]?.sender).toBe('me');
  });

  it('includes tapbacks when asked', async () => {
    const value = (await ask({ cmd: 'read', chat: '1', names: false, tapbacks: true })) as {
      messages: Message[];
    };
    expect(value.messages).toHaveLength(3);
    expect(value.messages[2]?.isTapback).toBe(true);
  });

  it('reaches a filtered conversation when it is named outright', async () => {
    const value = (await ask({ cmd: 'read', chat: '3', names: false })) as { messages: Message[] };
    expect(value.messages).toHaveLength(1);
  });

  it('dates survive the wire as dates', async () => {
    const socket = await connectDaemon(socketPath);
    const { messages } = (await request(socket!, { cmd: 'read', chat: '1', names: false })) as {
      messages: Message[];
    };
    // The daemon serialises to ISO strings; the client is expected to revive them.
    expect(typeof messages[0]?.date).toBe('string');
    expect(new Date(messages[0]?.date as unknown as string).getUTCFullYear()).toBe(2026);
  });
});

describe('search', () => {
  it('matches message bodies', async () => {
    const messages = (await ask({ cmd: 'search', query: 'deploy', names: false })) as Message[];
    expect(messages.map((message) => message.body)).toEqual(['deploy is green']);
  });

  it('skips filtered conversations unless asked', async () => {
    const hidden = (await ask({ cmd: 'search', query: '123456', names: false })) as Message[];
    expect(hidden).toHaveLength(0);
    const shown = (await ask({
      cmd: 'search',
      query: '123456',
      names: false,
      unknown: true,
    })) as Message[];
    expect(shown).toHaveLength(1);
  });
});

describe('resolve', () => {
  it('names one conversation for send to address', async () => {
    const chat = (await ask({ cmd: 'resolve', chat: 'Ship Room', names: false })) as Chat;
    expect(chat.guid).toBe('iMessage;+;chat9');
  });

  it('reports an ambiguous name rather than guessing', async () => {
    await expect(ask({ cmd: 'resolve', chat: 'nothing-matches', names: false })).rejects.toThrow(
      /no chat matching/,
    );
  });
});

describe('status', () => {
  it('reports the database it is reading', async () => {
    const status = (await ask({ cmd: 'status' })) as StatusReply;
    expect(status.database).toBe(databasePath);
    expect(status.messageCount).toBe(5);
    expect(status.protocol).toBe(PROTOCOL_VERSION);
  });
});

describe('protocol', () => {
  it('refuses a version it does not speak', async () => {
    const reply = await new Promise<string>((resolve, reject) => {
      const socket = connect(socketPath, () => {
        socket.write(encode({ cmd: 'status', v: 99 } as never));
      });
      const reader = createInterface({ input: socket, crlfDelay: Infinity });
      reader.once('line', (line: string) => {
        socket.destroy();
        resolve(line);
      });
      socket.once('error', reject);
    });
    const frame = JSON.parse(reply) as { type: string; code: string };
    expect(frame.type).toBe('error');
    expect(frame.code).toBe('version');
  });
});

describe('watch', () => {
  it('streams a message that arrives after the client subscribed', async () => {
    const socket = await connectDaemon(socketPath);
    expect(socket).not.toBeNull();
    const messages = stream(socket!, { cmd: 'watch', names: false });

    // Subscribing is what records the watermark, so the write has to come after
    // the first frame is being awaited.
    const arrived = (async () => {
      for await (const message of messages) return message as Message;
      return null;
    })();

    await new Promise((resolve) => setTimeout(resolve, 50));
    const writer = new DatabaseSync(databasePath);
    writer.exec(
      `INSERT INTO message (rowid, guid, text, is_from_me, handle_id, date, service)
       VALUES (6, 'm6', 'just landed', 0, 1, ${String(at(5))}, 'iMessage');
       INSERT INTO chat_message_join (chat_id, message_id) VALUES (1, 6);`,
    );
    writer.close();

    const message = await arrived;
    expect(message?.body).toBe('just landed');
  }, 10_000);
});
