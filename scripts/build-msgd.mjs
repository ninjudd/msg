#!/usr/bin/env node
/**
 * Build `msgd` as a Single Executable Application inside an app bundle.
 *
 * The daemon is its own executable so that TCC's client is `msgd` rather than
 * `node`, and so that the code holding Full Disk Access cannot be swapped out
 * from under the grant: a copy of node pointed at a script would run whatever
 * the plist or the script file happened to say. See
 * docs/projects/all/daemon-and-permissions.md §4.
 *
 * It lives in a bundle so that TCC keys its grants by bundle identifier rather
 * than by executable path. Path-keyed grants cannot be switched off — System
 * Settings only ever creates and deletes them — so a bare executable could be
 * granted Automation and then never revoked from the pane (§13).
 */

import { execFileSync, spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';
import { ensureIdentity } from '../dist/daemon/identity.js';
import { VERSION } from '../dist/version.js';

/** Fixed by Node; postject looks for this string to find where the blob goes. */
const FUSE = 'NODE_SEA_FUSE_fce680ab2cc467b6e072b8b5df1996b2';

/**
 * One constant behind both `CFBundleIdentifier` and `codesign --identifier`, so
 * the two can never disagree. Every TCC grant the daemon holds names it.
 */
const IDENTIFIER = 'com.ninjudd.msgd';

const root = fileURLToPath(new URL('..', import.meta.url));
const out = join(root, 'build');
const app = join(out, 'msgd.app');
const binary = join(app, 'Contents', 'MacOS', 'msgd');

/**
 * An ad-hoc signature is matched by cdhash, so every rebuild invalidates the
 * Full Disk Access grant. A stable identity anchors the requirement to the
 * certificate instead, and rebuilds keep the grant (§4). One is created on
 * first build; `MSG_SIGN_IDENTITY=-` forces ad-hoc.
 */
let identity = process.env['MSG_SIGN_IDENTITY'];
if (identity === undefined) {
  const provisioned = ensureIdentity();
  identity = provisioned.name;
  if (provisioned.created) {
    process.stdout.write(
      `created a "${provisioned.name}" code signing certificate in your login keychain\n`,
    );
  }
}

function run(command, args) {
  execFileSync(command, args, { stdio: ['ignore', 'ignore', 'inherit'] });
}

function bin(name) {
  return join(root, 'node_modules', '.bin', name);
}

/**
 * `NSAppleEventsUsageDescription` is not decoration. macOS denies an Apple
 * Event from a client that has none with -1743, without even prompting, so
 * without it the daemon could never be granted Automation and sending could not
 * move off the CLI (§7).
 */
function infoPlist() {
  return `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>${IDENTIFIER}</string>
  <key>CFBundleName</key><string>msgd</string>
  <key>CFBundleExecutable</key><string>msgd</string>
  <key>CFBundleIconFile</key><string>msgd</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>LSBackgroundOnly</key><true/>
  <key>NSAppleEventsUsageDescription</key><string>msg sends and reads messages through Messages.</string>
  <key>NSContactsUsageDescription</key><string>msg shows contact names instead of phone numbers.</string>
</dict>
</plist>
`;
}

// A stale _CodeSignature from a previous build makes the bundle fail to
// validate, so start from nothing rather than writing over it.
rmSync(app, { recursive: true, force: true });
mkdirSync(join(app, 'Contents', 'MacOS'), { recursive: true });

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

writeFileSync(join(app, 'Contents', 'Info.plist'), infoPlist());

// Committed rather than generated, so the build needs nothing but a copy.
// scripts/build-icon.mjs redraws it, and is not run from here.
const icon = join(root, 'assets', 'msgd.icns');
if (!existsSync(icon)) {
  throw new Error(`no icon at ${icon}\nRedraw it with \`node scripts/build-icon.mjs\`.`);
}
mkdirSync(join(app, 'Contents', 'Resources'), { recursive: true });
copyFileSync(icon, join(app, 'Contents', 'Resources', 'msgd.icns'));

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

// Signing comes last: everything above invalidates a signature. Signing the
// bundle rather than the executable is what produces _CodeSignature, and
// --identifier fixes the name the grant is keyed to whatever the file is called.
run('codesign', ['--force', '--sign', identity, '--identifier', IDENTIFIER, app]);

// The bundle is the whole point, so check that codesign saw one. Signing a
// directory that macOS does not recognise as a bundle succeeds quietly and
// produces exactly the path-keyed grant this layout exists to avoid.
const described = spawnSync('codesign', ['-dv', app], { encoding: 'utf8' });
const report = `${described.stdout ?? ''}${described.stderr ?? ''}`;
if (!/^Format=app bundle/m.test(report)) {
  throw new Error(`codesign does not see an app bundle:\n${report}`);
}
if (!new RegExp(`^Identifier=${IDENTIFIER}$`, 'm').test(report)) {
  throw new Error(`signed with the wrong identifier:\n${report}`);
}

const megabytes = (statSync(binary).size / 1024 / 1024).toFixed(0);
process.stdout.write(`built ${app} (${megabytes}MB, signed ${identity})\n`);
if (identity === '-') {
  process.stdout.write(
    'Signed ad-hoc: every rebuild changes the cdhash, so Full Disk Access has to be\n' +
      'granted again. Unset MSG_SIGN_IDENTITY to sign with a stable certificate instead.\n',
  );
}
