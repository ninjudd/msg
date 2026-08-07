#!/usr/bin/env node
import { setTimeout as sleep } from 'node:timers/promises';
import { Command } from 'commander';
import { sinceToAppleDate } from './apple.js';
import {
  AccessDeniedError,
  fetchChats,
  fetchMessages,
  latestRowid,
  openDatabase,
  resolveChat,
} from './db.js';
import { sendFile, sendMessage } from './commands/send.js';
import { EMPTY_INDEX, loadContacts, type ContactIndex } from './contacts.js';
import { renderChats, renderMessages, toJson } from './format.js';

const program = new Command();

function fail(error: unknown): never {
  const message = (error as Error).message;
  process.stderr.write(`${message}\n`);
  process.exit(error instanceof AccessDeniedError ? 2 : 1);
}

function parseCount(value: string): number {
  const count = Number(value);
  if (!Number.isInteger(count) || count <= 0) {
    throw new Error(`expected a positive integer, got ${value}`);
  }
  return count;
}

program
  .name('msg')
  .description('read and send iMessages from the command line')
  .version('0.1.0')
  .option('--db <path>', 'path to chat.db (defaults to the Messages database)')
  .option('--no-names', 'show raw handles instead of looking up contact names');

function database() {
  return openDatabase(program.opts<{ db?: string }>().db);
}

let contactIndex: ContactIndex | null = null;

/** Load Contacts once per run, and only when names are wanted. */
function contacts(): ContactIndex {
  if (program.opts<{ names: boolean }>().names === false) return EMPTY_INDEX;
  contactIndex ??= loadContacts();
  return contactIndex;
}

program
  .command('chats')
  .description('list conversations by most recent activity')
  .argument('[query]', 'filter by name, handle, or identifier')
  .option('-n, --limit <count>', 'how many to show', parseCount, 30)
  .option('--json', 'emit JSON')
  .action((query: string | undefined, opts: { limit: number; json?: boolean }) => {
    try {
      const chats = fetchChats(database(), query, opts.limit, contacts());
      process.stdout.write(opts.json === true ? toJson(chats) : renderChats(chats));
    } catch (error) {
      fail(error);
    }
  });

program
  .command('read')
  .description('print a conversation')
  .argument('<chat>', 'chat rowid, handle, or name substring')
  .option('-n, --limit <count>', 'how many messages', parseCount, 50)
  .option('--since <when>', 'only messages after a duration like 2h or a date')
  .option('--tapbacks', 'include reactions')
  .option('--json', 'emit JSON')
  .action(
    (
      spec: string,
      opts: { limit: number; since?: string; tapbacks?: boolean; json?: boolean },
    ) => {
      try {
        const db = database();
        const index = contacts();
        const chat = resolveChat(db, spec, index);
        const messages = fetchMessages(db, {
          chatId: chat.rowid,
          limit: opts.limit,
          includeTapbacks: opts.tapbacks,
          contacts: index,
          afterDate: opts.since === undefined ? undefined : sinceToAppleDate(opts.since),
        });
        if (opts.json === true) {
          process.stdout.write(toJson({ chat, messages }));
        } else {
          process.stdout.write(`${chat.name}\n\n${renderMessages(messages)}`);
        }
      } catch (error) {
        fail(error);
      }
    },
  );

program
  .command('search')
  .description('search message bodies')
  .argument('<query>', 'text to look for')
  .option('-n, --limit <count>', 'how many matches', parseCount, 25)
  .option('-c, --chat <chat>', 'restrict to one conversation')
  .option('--since <when>', 'only messages after a duration like 30d or a date')
  .option('--json', 'emit JSON')
  .action(
    (
      query: string,
      opts: { limit: number; chat?: string; since?: string; json?: boolean },
    ) => {
      try {
        const db = database();
        const index = contacts();
        const chatId =
          opts.chat === undefined ? undefined : resolveChat(db, opts.chat, index).rowid;
        const messages = fetchMessages(db, {
          query,
          chatId,
          limit: opts.limit,
          contacts: index,
          afterDate: opts.since === undefined ? undefined : sinceToAppleDate(opts.since),
        });
        process.stdout.write(
          opts.json === true ? toJson(messages) : renderMessages(messages, true),
        );
      } catch (error) {
        fail(error);
      }
    },
  );

program
  .command('watch')
  .description('follow new messages as they arrive')
  .option('-c, --chat <chat>', 'restrict to one conversation')
  .option('--interval <seconds>', 'how often to poll', parseCount, 3)
  .option('--tapbacks', 'include reactions')
  .option('--json', 'emit JSON lines')
  .action(async (opts: { chat?: string; interval: number; tapbacks?: boolean; json?: boolean }) => {
    try {
      const db = database();
      const index = contacts();
      const chatId =
        opts.chat === undefined ? undefined : resolveChat(db, opts.chat, index).rowid;
      let watermark = latestRowid(db);

      for (;;) {
        const messages = fetchMessages(db, {
          chatId,
          afterRowid: watermark,
          limit: 200,
          includeTapbacks: opts.tapbacks,
          contacts: index,
        });
        for (const message of messages) {
          watermark = Math.max(watermark, message.rowid);
          process.stdout.write(
            opts.json === true
              ? `${JSON.stringify(message)}\n`
              : renderMessages([message], chatId === undefined),
          );
        }
        await sleep(opts.interval * 1000);
      }
    } catch (error) {
      fail(error);
    }
  });

program
  .command('contacts')
  .description('look up the name behind a handle')
  .argument('[handles...]', 'phone numbers or email addresses')
  .option('--json', 'emit JSON')
  .action((handles: string[], opts: { json?: boolean }) => {
    try {
      const index = loadContacts();
      if (handles.length === 0) {
        process.stdout.write(`${index.size} handles known from Contacts\n`);
        return;
      }
      const resolved = handles.map((handle) => ({ handle, name: index.lookup(handle) }));
      if (opts.json === true) {
        process.stdout.write(toJson(resolved));
        return;
      }
      for (const { handle, name } of resolved) {
        process.stdout.write(`${handle}\t${name ?? '(unknown)'}\n`);
      }
    } catch (error) {
      fail(error);
    }
  });

program
  .command('send')
  .description('send a message to a conversation')
  .argument('<chat>', 'chat rowid, handle, or name substring')
  .argument('[body...]', 'message text')
  .option('-f, --file <path>', 'send a file instead of text')
  .option('--dry-run', 'show what would be sent without sending')
  .action((spec: string, body: string[], opts: { file?: string; dryRun?: boolean }) => {
    try {
      const db = database();
      const chat = resolveChat(db, spec, contacts());
      const text = body.join(' ');

      if (opts.file === undefined && text.length === 0) {
        throw new Error('nothing to send, provide message text or --file');
      }
      const what = opts.file ?? text;

      if (opts.dryRun === true) {
        process.stdout.write(`would send to ${chat.name}: ${what}\n`);
        return;
      }

      if (opts.file === undefined) {
        sendMessage(chat.guid, text);
      } else {
        sendFile(chat.guid, opts.file);
      }
      process.stdout.write(`sent to ${chat.name}: ${what}\n`);
    } catch (error) {
      fail(error);
    }
  });

program.parseAsync(process.argv).catch(fail);
