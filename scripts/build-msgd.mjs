#!/usr/bin/env node
/**
 * Build `msgd` as a Single Executable Application.
 *
 * The daemon is its own executable so that TCC's client is `msgd` rather than
 * `node`, and so that the code holding Full Disk Access cannot be swapped out
 * from under the grant: a copy of node pointed at a script would run whatever
 * the plist or the script file happened to say. See
 * docs/projects/all/daemon-and-permissions.md §4.
 */

import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

/** Fixed by Node; postject looks for this string to find where the blob goes. */
const FUSE = 'NODE_SEA_FUSE_fce680ab2cc467b6e072b8b5df1996b2';

const IDENTIFIER = 'com.ninjudd.msgd';

const root = fileURLToPath(new URL('..', import.meta.url));
const out = join(root, 'build');
const binary = join(out, 'msgd');

/**
 * An ad-hoc signature is matched by cdhash, so every rebuild invalidates the
 * Full Disk Access grant. A stable identity anchors the requirement to the
 * certificate instead, and rebuilds keep the grant (§4).
 */
const identity = process.env['MSG_SIGN_IDENTITY'] ?? '-';

function run(command, args) {
  execFileSync(command, args, { stdio: ['ignore', 'ignore', 'inherit'] });
}

function bin(name) {
  return join(root, 'node_modules', '.bin', name);
}

mkdirSync(out, { recursive: true });

// A SEA's require() reaches built-in modules only, so the daemon has to arrive
// as one CommonJS file with nothing left to resolve at runtime.
await build({
  entryPoints: [join(root, 'src', 'msgd.ts')],
  outfile: join(out, 'msgd.cjs'),
  bundle: true,
  platform: 'node',
  format: 'cjs',
  target: 'node24',
  logLevel: 'warning',
});

writeFileSync(
  join(out, 'sea-config.json'),
  `${JSON.stringify(
    {
      main: join(out, 'msgd.cjs'),
      output: join(out, 'sea-prep.blob'),
      disableExperimentalSEAWarning: true,
    },
    null,
    2,
  )}\n`,
);

run(process.execPath, ['--experimental-sea-config', join(out, 'sea-config.json')]);

// launchd may hold the previous build open, so replace rather than overwrite.
rmSync(binary, { force: true });
copyFileSync(process.execPath, binary);
run('codesign', ['--remove-signature', binary]);
run(bin('postject'), [
  binary,
  'NODE_SEA_BLOB',
  join(out, 'sea-prep.blob'),
  '--sentinel-fuse',
  FUSE,
  '--macho-segment-name',
  'NODE_SEA',
]);
// Without --identifier the signing identifier defaults to the filename, and a
// rename would void the grant.
run('codesign', ['--force', '--sign', identity, '--identifier', IDENTIFIER, binary]);

const megabytes = (statSync(binary).size / 1024 / 1024).toFixed(0);
process.stdout.write(`built ${binary} (${megabytes}MB, signed ${identity})\n`);
if (identity === '-') {
  process.stdout.write(
    'Signed ad-hoc: every rebuild changes the cdhash, so Full Disk Access has to be\n' +
      'granted again. Set MSG_SIGN_IDENTITY to a Code Signing certificate to keep it.\n',
  );
}
