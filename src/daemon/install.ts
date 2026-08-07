/**
 * Installing the daemon: a copied bundle, a launchd agent, and the one step
 * that cannot be automated.
 *
 * There is no API for granting Full Disk Access — `tccutil` only removes — so
 * the install ends by telling the user what to switch on, and the daemon's
 * first failed read is what puts it in the list to be switched
 * (docs/projects/all/daemon-and-permissions.md §9).
 */

import { execFileSync, spawnSync } from 'node:child_process';
import { chmodSync, cpSync, existsSync, mkdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { LABEL, socketPath, stateDirectory } from './protocol.js';

/**
 * The installed bundle.
 *
 * A bundle rather than a bare executable, because TCC keys a grant by bundle
 * identifier when it can resolve one and by executable path when it cannot —
 * and a path-keyed grant cannot be switched off. System Settings only ever
 * creates and deletes those rows; the toggle authenticates and then does
 * nothing (§13). It is not in /Applications because nobody launches it.
 */
export function bundlePath(): string {
  return join(homedir(), '.local', 'libexec', 'msgd.app');
}

/** What launchd runs. TCC resolves it back to the bundle above. */
export function binaryPath(): string {
  return join(bundlePath(), 'Contents', 'MacOS', 'msgd');
}

/** Where the daemon lived before it was bundled, removed on install. */
function legacyBinaryPath(): string {
  return join(homedir(), '.local', 'libexec', 'msgd');
}

export function plistPath(): string {
  return join(homedir(), 'Library', 'LaunchAgents', `${LABEL}.plist`);
}

export function logPath(): string {
  return join(stateDirectory(), 'msgd.log');
}

/** Where `pnpm build:msgd` leaves the signed bundle. */
export function builtBundle(): string {
  return fileURLToPath(new URL('../../build/msgd.app', import.meta.url));
}

function domain(): string {
  return `gui/${String(process.getuid?.() ?? 0)}`;
}

/**
 * Settings the daemon reads for itself.
 *
 * A launchd job inherits nothing from the shell that installed it, so anything
 * set here has to be written into the plist or it silently does not apply. The
 * failure is confusing rather than loud: installing with `MSG_SOCKET` set gives
 * a CLI looking at one path and a daemon listening on another.
 */
const DAEMON_ENVIRONMENT = [
  'MSG_SOCKET',
  'MSG_STATE_DIR',
  'MSG_CONFIG',
  'MSG_CONTACTS_SOURCE',
] as const;

/**
 * `MSG_DB` is deliberately absent.
 *
 * It is documented as `--db` by another name, and the CLI answers it locally
 * rather than asking the daemon, so carrying it here could never help the
 * documented path — it could only outlive the shell that set it. Installing
 * while pointed at a fixture and later unsetting the variable would leave a
 * daemon still pinned to that fixture, answering a CLI that has no idea, which
 * is the worst shape a wrong answer can take. Run `msgd` directly with `MSG_DB`
 * set to serve a fixture.
 */

export function daemonEnvironment(
  environment: NodeJS.ProcessEnv = process.env,
): Record<string, string> {
  const carried: Record<string, string> = {};
  for (const name of DAEMON_ENVIRONMENT) {
    const value = environment[name];
    if (value !== undefined && value.length > 0) carried[name] = value;
  }
  return carried;
}

function xmlText(value: string): string {
  return value.replace(/[&<>]/g, (character) =>
    character === '&' ? '&amp;' : character === '<' ? '&lt;' : '&gt;',
  );
}

/**
 * The agent is user-owned, which is safe only because the daemon is a single
 * executable application: pointing this plist somewhere else runs a binary
 * that holds no grant (§4).
 */
export function plist(
  binary: string,
  log: string,
  environment: Record<string, string> = {},
): string {
  const entries = Object.entries(environment)
    .map(([name, value]) => `    <key>${name}</key><string>${xmlText(value)}</string>`)
    .join('\n');
  const block =
    entries.length > 0 ? `  <key>EnvironmentVariables</key>\n  <dict>\n${entries}\n  </dict>\n` : '';

  return `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>${LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>${xmlText(binary)}</string>
  </array>
${block}  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ProcessType</key><string>Background</string>
  <key>StandardErrorPath</key><string>${xmlText(log)}</string>
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
  bundle: string;
  binary: string;
  plist: string;
  socket: string;
  log: string;
  /** Whether an unbundled daemon from an older install was removed. */
  replacedLegacy: boolean;
  /** What was carried into the job from the installing shell's environment. */
  environment: Record<string, string>;
}

export function install(source = builtBundle()): Installed {
  if (!existsSync(join(source, 'Contents', 'MacOS', 'msgd'))) {
    throw new Error(
      `no daemon bundle at ${source}\nBuild it first with \`pnpm build:msgd\`.`,
    );
  }

  const bundle = bundlePath();
  const binary = binaryPath();
  mkdirSync(dirname(bundle), { recursive: true });
  // launchd holds the running binary open, so replacing it in place fails. A
  // leftover _CodeSignature would also make the new bundle fail to validate.
  rmSync(bundle, { recursive: true, force: true });
  cpSync(source, bundle, { recursive: true });
  chmodSync(binary, 0o755);

  // An install from before the bundle left an executable where the bundle now
  // goes beside it. It holds its own grants and would keep running if anything
  // still pointed at it, so it does not get to linger.
  const legacy = legacyBinaryPath();
  const replacedLegacy = existsSync(legacy) && statSync(legacy).isFile();
  if (replacedLegacy) rmSync(legacy);

  const log = logPath();
  mkdirSync(stateDirectory(), { recursive: true, mode: 0o700 });

  const environment = daemonEnvironment();
  const agent = plistPath();
  mkdirSync(dirname(agent), { recursive: true });
  writeFileSync(agent, plist(binary, log, environment));

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

  return { bundle, binary, plist: agent, socket: socketPath(), log, replacedLegacy, environment };
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
 * How the bundle is signed, which decides whether its grant survives a rebuild.
 *
 * `--verbose=2` because plain `-dv` omits the Authority line entirely, so
 * everything came back as a bare "signed" with no way to tell which certificate
 * the grant is anchored to. It reports on stderr, hence spawnSync.
 */
export function signatureOf(target: string): string {
  const result = spawnSync('codesign', ['-dv', '--verbose=2', target], { encoding: 'utf8' });
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

/** The pane holding the switch that decides whether the daemon may send (§13). */
export function openAutomation(): void {
  spawnSync('open', ['x-apple.systempreferences:com.apple.preference.security?Privacy_Automation']);
}

export function uninstall(): { removed: string[]; grantRemains: boolean } {
  const removed: string[] = [];
  launchctl(['bootout', `${domain()}/${LABEL}`]);

  for (const path of [plistPath(), bundlePath(), legacyBinaryPath(), socketPath()]) {
    if (existsSync(path)) {
      rmSync(path, { recursive: true, force: true });
      removed.push(path);
    }
  }
  // Deleting the bundle does not withdraw its grants; the entries outlive it (§9).
  return { removed, grantRemains: true };
}
