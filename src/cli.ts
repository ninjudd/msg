#!/usr/bin/env node
import { Command } from 'commander';
import { sendFile, sendMessage } from './commands/send.js';
import { DaemonError, socketPath } from './daemon/protocol.js';
import {
  binaryPath,
  builtBinary,
  install,
  isLoaded,
  logPath,
  plistPath,
  uninstall,
} from './daemon/install.js';
import { Daemon } from './daemon/server.js';
import { AccessDeniedError } from './db.js';
import { renderChats, renderMessages, toJson } from './format.js';
import { daemonStatus, openSource, type Source } from './source.js';
import { VERSION } from './version.js';

const program = new Command();

/** Exit 2 is the documented status for "the data is there, the grant is not". */
function isDenied(error: unknown): boolean {
  return (
    error instanceof AccessDeniedError ||
    (error instanceof DaemonError && error.code === 'access-denied')
  );
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function fail(error: unknown): never {
  process.stderr.write(`${describe(error)}\n`);
  process.exit(isDenied(error) ? 2 : 1);
}

function parseCount(value: string): number {
  const count = Number(value);
  if (!Number.isInteger(count) || count <= 0) {
    throw new Error(`expected a positive integer, got ${value}`);
  }
  return count;
}

interface GlobalOptions {
  db?: string | undefined;
  names: boolean;
  unknown?: boolean | undefined;
}

program
  .name('msg')
  .description('read and send iMessages from the command line')
  .version(VERSION)
  .option('--db <path>', 'path to chat.db (defaults to the Messages database)')
  .option('--no-names', 'show raw handles instead of looking up contact names')
  .option('--unknown', 'include conversations Messages filters as unknown senders');

function globals(): GlobalOptions {
  return program.opts<GlobalOptions>();
}

function wantNames(): boolean {
  return globals().names !== false;
}

function includeFiltered(): boolean {
  return globals().unknown === true;
}

/**
 * Run one command against the daemon, or against the database when no daemon
 * is listening. `--db` names a path and so is always answered locally, which
 * keeps fixtures working without widening what the daemon will answer.
 */
async function withSource(body: (source: Source) => Promise<void>): Promise<void> {
  const source = await openSource({ db: globals().db });
  try {
    await body(source);
  } catch (error) {
    fail(error);
  } finally {
    source.close();
  }
}

program
  .command('chats')
  .description('list conversations by most recent activity')
  .argument('[query]', 'filter by name, handle, or identifier')
  .option('-n, --limit <count>', 'how many to show', parseCount, 30)
  .option('--json', 'emit JSON')
  .action(async (query: string | undefined, opts: { limit: number; json?: boolean }) => {
    await withSource(async (source) => {
      const chats = await source.chats({
        query,
        limit: opts.limit,
        unknown: includeFiltered(),
        names: wantNames(),
      });
      process.stdout.write(opts.json === true ? toJson(chats) : renderChats(chats));
    });
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
    async (
      spec: string,
      opts: { limit: number; since?: string; tapbacks?: boolean; json?: boolean },
    ) => {
      await withSource(async (source) => {
        const { chat, messages } = await source.read({
          chat: spec,
          limit: opts.limit,
          since: opts.since,
          tapbacks: opts.tapbacks === true,
          names: wantNames(),
        });
        if (opts.json === true) {
          process.stdout.write(toJson({ chat, messages }));
        } else {
          process.stdout.write(`${chat.name}\n\n${renderMessages(messages)}`);
        }
      });
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
    async (
      query: string,
      opts: { limit: number; chat?: string; since?: string; json?: boolean },
    ) => {
      await withSource(async (source) => {
        const messages = await source.search({
          query,
          chat: opts.chat,
          limit: opts.limit,
          since: opts.since,
          unknown: includeFiltered(),
          names: wantNames(),
        });
        process.stdout.write(
          opts.json === true ? toJson(messages) : renderMessages(messages, true),
        );
      });
    },
  );

program
  .command('watch')
  .description('follow new messages as they arrive')
  .option('-c, --chat <chat>', 'restrict to one conversation')
  .option('--interval <seconds>', 'how often to poll without a daemon', parseCount, 3)
  .option('--tapbacks', 'include reactions')
  .option('--json', 'emit JSON lines')
  .action(
    async (opts: { chat?: string; interval: number; tapbacks?: boolean; json?: boolean }) => {
      await withSource(async (source) => {
        await source.watch(
          {
            chat: opts.chat,
            tapbacks: opts.tapbacks === true,
            unknown: includeFiltered(),
            names: wantNames(),
            interval: opts.interval,
          },
          (message) => {
            process.stdout.write(
              opts.json === true
                ? `${JSON.stringify(message)}\n`
                : renderMessages([message], opts.chat === undefined),
            );
          },
        );
      });
    },
  );

program
  .command('contacts')
  .description('look up the name behind a handle')
  .argument('[handles...]', 'phone numbers or email addresses')
  .option('--json', 'emit JSON')
  .action(async (handles: string[], opts: { json?: boolean }) => {
    await withSource(async (source) => {
      const reply = await source.contacts(handles);
      if (handles.length === 0) {
        process.stdout.write(`${reply.size} handles known from Contacts\n`);
        return;
      }
      if (opts.json === true) {
        process.stdout.write(toJson(reply.resolved));
        return;
      }
      for (const { handle, name } of reply.resolved) {
        process.stdout.write(`${handle}\t${name ?? '(unknown)'}\n`);
      }
    });
  });

program
  .command('send')
  .description('send a message to a conversation')
  .argument('<chat>', 'chat rowid, handle, or name substring')
  .argument('[body...]', 'message text')
  .option('-f, --file <path>', 'send a file instead of text')
  .option('--dry-run', 'show what would be sent without sending')
  .action(async (spec: string, body: string[], opts: { file?: string; dryRun?: boolean }) => {
    await withSource(async (source) => {
      const chat = await source.resolve(spec, wantNames());
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
    });
  });

const daemon = program
  .command('daemon')
  .description('manage the background reader that holds Full Disk Access');

daemon
  .command('install')
  .description('install and start the launchd agent')
  .option('--from <path>', 'binary to install', builtBinary())
  .action((opts: { from: string }) => {
    try {
      const installed = install(opts.from);
      process.stdout.write(
        `installed ${installed.binary}\n` +
          `started ${installed.plist}\n\n` +
          'One step left, and it cannot be automated: add the daemon to\n' +
          'System Settings > Privacy & Security > Full Disk Access.\n\n' +
          `  ${installed.binary}\n\n` +
          'It appears in that list because it has already tried to read and been\n' +
          'refused, so give it a minute if it is not there yet. Then switch it on\n' +
          'and run `msg daemon status`.\n',
      );
    } catch (error) {
      fail(error);
    }
  });

daemon
  .command('uninstall')
  .description('stop and remove the launchd agent')
  .action(() => {
    try {
      const { removed } = uninstall();
      for (const path of removed) process.stdout.write(`removed ${path}\n`);
      if (removed.length === 0) process.stdout.write('nothing to remove\n');
      process.stdout.write(
        '\nThe Full Disk Access grant outlives the binary. Remove it in System Settings,\n' +
          'or with:\n\n  sudo tccutil reset SystemPolicyAllFiles com.ninjudd.msgd\n',
      );
    } catch (error) {
      fail(error);
    }
  });

daemon
  .command('status')
  .description('report whether the daemon is installed, running, and granted')
  .option('--json', 'emit JSON')
  .action(async (opts: { json?: boolean }) => {
    const loaded = isLoaded();
    let status = null;
    let problem: unknown = null;
    try {
      status = await daemonStatus();
    } catch (error) {
      problem = error;
    }
    const message = problem === null ? null : describe(problem);

    if (opts.json === true) {
      process.stdout.write(toJson({ loaded, binary: binaryPath(), status, error: message }));
      return;
    }

    const lines = [
      `agent      ${loaded ? 'loaded' : 'not loaded'} (${plistPath()})`,
      `binary     ${binaryPath()}`,
      `socket     ${socketPath()}${status === null ? ' (not listening)' : ''}`,
      `log        ${logPath()}`,
    ];
    if (status !== null) {
      lines.push(
        `version    ${status.version} (protocol ${String(status.protocol)}), pid ${String(status.pid)}, up ${String(status.uptimeSeconds)}s`,
        `database   ${status.database}`,
        `messages   ${String(status.messageCount)}`,
        `contacts   ${String(status.contactCount)} handles`,
        `watchers   ${String(status.watchers)}`,
      );
    }
    process.stdout.write(`${lines.join('\n')}\n`);

    if (message !== null) {
      process.stderr.write(`\n${message}\n`);
      process.exit(isDenied(problem) ? 2 : 1);
    }
    if (!loaded) {
      process.stderr.write('\nNo agent installed. Run `msg daemon install`.\n');
    }
  });

daemon
  .command('run')
  .description('run the daemon in the foreground, for debugging')
  .action(async () => {
    const server = new Daemon({ dbPath: globals().db });
    try {
      const path = await server.listen();
      process.stderr.write(`msgd ${VERSION} listening on ${path}\n`);
    } catch (error) {
      fail(error);
    }
  });

program.parseAsync(process.argv).catch(fail);
