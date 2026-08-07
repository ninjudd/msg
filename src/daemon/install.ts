/**
 * Installing the daemon: a copied binary, a launchd agent, and the one step
 * that cannot be automated.
 *
 * There is no API for granting Full Disk Access — `tccutil` only removes — so
 * the install ends by telling the user what to switch on, and the daemon's
 * first failed read is what puts it in the list to be switched
 * (docs/projects/all/daemon-and-permissions.md §9).
 */

import { execFileSync, spawnSync } from 'node:child_process';
import { chmodSync, copyFileSync, existsSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { LABEL, socketPath, stateDirectory } from './protocol.js';

export function binaryPath(): string {
  return join(homedir(), '.local', 'libexec', 'msgd');
}

export function plistPath(): string {
  return join(homedir(), 'Library', 'LaunchAgents', `${LABEL}.plist`);
}

export function logPath(): string {
  return join(stateDirectory(), 'msgd.log');
}

/** Where `pnpm build:msgd` leaves the signed executable. */
export function builtBinary(): string {
  return fileURLToPath(new URL('../../build/msgd', import.meta.url));
}

function domain(): string {
  return `gui/${String(process.getuid?.() ?? 0)}`;
}

/**
 * The agent is user-owned, which is safe only because the daemon is a single
 * executable application: pointing this plist somewhere else runs a binary
 * that holds no grant (§4).
 */
export function plist(binary: string, log: string): string {
  return `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>${LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>${binary}</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ProcessType</key><string>Background</string>
  <key>StandardErrorPath</key><string>${log}</string>
</dict>
</plist>
`;
}

function launchctl(args: string[]): { ok: boolean; output: string } {
  try {
    const output = execFileSync('launchctl', args, {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    return { ok: true, output };
  } catch (error) {
    return { ok: false, output: (error as { stderr?: string }).stderr ?? '' };
  }
}

export function isLoaded(): boolean {
  return launchctl(['print', `${domain()}/${LABEL}`]).ok;
}

export interface Installed {
  binary: string;
  plist: string;
  socket: string;
  log: string;
}

export function install(source = builtBinary()): Installed {
  if (!existsSync(source)) {
    throw new Error(
      `no daemon binary at ${source}\nBuild it first with \`pnpm build:msgd\`.`,
    );
  }

  const binary = binaryPath();
  mkdirSync(dirname(binary), { recursive: true });
  // launchd holds the running binary open, so replacing it in place fails.
  if (existsSync(binary)) rmSync(binary);
  copyFileSync(source, binary);
  chmodSync(binary, 0o755);

  const log = logPath();
  mkdirSync(stateDirectory(), { recursive: true, mode: 0o700 });

  const agent = plistPath();
  mkdirSync(dirname(agent), { recursive: true });
  writeFileSync(agent, plist(binary, log));

  // Booting out first makes install idempotent, and picks up a changed plist.
  // bootout returns before the job is actually gone, and bootstrapping into a
  // half-torn-down service fails with a bare "Input/output error", so wait for
  // the service to disappear before putting it back.
  launchctl(['bootout', `${domain()}/${LABEL}`]);
  for (let attempt = 0; attempt < 50 && isLoaded(); attempt += 1) {
    execFileSync('/bin/sleep', ['0.1']);
  }

  const started = launchctl(['bootstrap', domain(), agent]);
  if (!started.ok) {
    throw new Error(`launchctl could not start ${LABEL}: ${started.output.trim()}`);
  }

  return { binary, plist: agent, socket: socketPath(), log };
}

/**
 * Read `codesign -dv` output.
 *
 * A self-signed certificate produces no `Authority` line, so the presence of a
 * signature is what distinguishes it from ad-hoc — and the field is
 * `Signature size=`, not `Signature=`.
 */
export function describeSignature(text: string): string {
  if (/flags=[^\s]*adhoc/.test(text)) return 'ad-hoc';
  const authority = /Authority=(.+)/.exec(text);
  if (authority?.[1] !== undefined) return authority[1].trim();
  return /^Signature size=/m.test(text) ? 'signed' : 'unsigned';
}

/**
 * How the binary is signed, which decides whether its grant survives a rebuild.
 * `codesign -dv` reports on stderr, hence spawnSync rather than execFileSync.
 */
export function signatureOf(binary: string): string {
  const result = spawnSync('codesign', ['-dv', binary], { encoding: 'utf8' });
  return describeSignature(`${result.stdout ?? ''}${result.stderr ?? ''}`);
}

/**
 * Put the pane in front of the user. There is no API for granting Full Disk
 * Access — only `tccutil` for removing one — so the last step of an install is
 * always a human at System Settings (§9).
 */
export function openFullDiskAccess(): void {
  spawnSync('open', ['x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles']);
}

export function uninstall(): { removed: string[]; grantRemains: boolean } {
  const removed: string[] = [];
  launchctl(['bootout', `${domain()}/${LABEL}`]);

  for (const path of [plistPath(), binaryPath(), socketPath()]) {
    if (existsSync(path)) {
      rmSync(path);
      removed.push(path);
    }
  }
  // Deleting the binary does not withdraw its grant; the entry outlives it (§9).
  return { removed, grantRemains: true };
}
