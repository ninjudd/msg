/**
 * The config file, which exists to hold one key.
 *
 * `send = true` in `~/.config/msg/config.toml` is the first of the two gates on
 * sending: it prevents accidents and produces a refusal that names the key,
 * instead of an opaque AppleScript -1743. The second gate is the Automation
 * grant, which is the one that holds when this file does not. See
 * docs/projects/all/daemon-and-permissions.md §7.
 *
 * The daemon reads it, not the client. A check a caller performs on itself is
 * advice, not a gate.
 */

import { existsSync, readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

export function configPath(): string {
  return process.env['MSG_CONFIG'] ?? join(homedir(), '.config', 'msg', 'config.toml');
}

export interface Config {
  send: boolean;
}

/**
 * Read the config.
 *
 * Deliberately not a TOML parser. One flat `key = value` per line covers the
 * whole of what this file is allowed to say, and a dependency for that would
 * cost more than it explains. Anything unrecognised is ignored, so a file that
 * grows real TOML later still reads correctly for `send`.
 */
export function readConfig(path = configPath()): Config {
  if (!existsSync(path)) return { send: false };
  let text: string;
  try {
    text = readFileSync(path, 'utf8');
  } catch {
    return { send: false };
  }

  for (const line of text.split('\n')) {
    const match = /^\s*send\s*=\s*(true|false)\s*(?:#.*)?$/.exec(line);
    if (match !== null) return { send: match[1] === 'true' };
  }
  return { send: false };
}

/** The refusal the daemon gives when sending is switched off. */
export function disabledMessage(path = configPath()): string {
  return (
    'sending is disabled.\n' +
    `Add \`send = true\` to ${path} to enable it.\n` +
    'macOS also has to allow msgd to control Messages, under System Settings >\n' +
    'Privacy & Security > Automation.'
  );
}
