#!/usr/bin/env node
/**
 * The daemon process, started by launchd and resident for as long as the user
 * is logged in. It holds the Full Disk Access grant; the CLI holds none.
 */

import { Daemon } from './daemon/server.js';
import { VERSION } from './version.js';

function log(message: string): void {
  process.stderr.write(`msgd ${new Date().toISOString()} ${message}\n`);
}

async function main(): Promise<void> {
  const daemon = new Daemon();

  // Touch the database before anyone asks. Under launchd the *failure* is the
  // point: a denied access is what creates this binary's entry in the Full
  // Disk Access list, and there is no other way to make it appear there to be
  // switched on (docs/projects/all/daemon-and-permissions.md §9).
  try {
    daemon.database();
    log('database readable');
  } catch (error) {
    const [first] = (error as Error).message.split('\n');
    log(`database unreadable: ${first ?? 'unknown error'}`);
  }

  const path = await daemon.listen();
  log(`msgd ${VERSION} listening on ${path} as pid ${process.pid}`);

  for (const signal of ['SIGTERM', 'SIGINT'] as const) {
    process.on(signal, () => {
      log(`${signal}, shutting down`);
      void daemon.close().then(() => process.exit(0));
    });
  }
}

void main().catch((error: unknown) => {
  log(`fatal: ${error instanceof Error ? error.message : String(error)}`);
  process.exit(1);
});
