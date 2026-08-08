/**
 * Driving Messages.app over Apple Events.
 *
 * Only the daemon does this. Apple Events are gated by Automation, which is a
 * separate TCC service from Full Disk Access, so `msgd` can be allowed to send
 * while being refused the database, or the reverse. That separation is the
 * reason sending belongs here rather than in the CLI, where the only gate would
 * be the process's opinion of itself (§7).
 *
 * macOS refuses an Apple Event from a client with no
 * `NSAppleEventsUsageDescription`, without even prompting, so this only works
 * because the daemon ships as a bundle carrying one in its `Info.plist`: see
 * scripts/build-msgd.mjs.
 */

import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, join } from 'node:path';

/**
 * Arguments are passed to the script rather than interpolated into it, so
 * quotes and backslashes in the body need no escaping.
 */
const SEND_TEXT = `
on run {chatGuid, body}
  tell application "Messages"
    send body to chat id chatGuid
  end tell
end run
`;

const SEND_FILE = `
on run {chatGuid, filePath}
  tell application "Messages"
    send POSIX file filePath to chat id chatGuid
  end tell
end run
`;

/** Long enough for Messages to have taken the file, short enough to not litter. */
const ATTACHMENT_LIFETIME_MS = 60_000;

function runScript(script: string, args: string[]): void {
  try {
    execFileSync('osascript', ['-e', script, ...args], { stdio: 'pipe' });
  } catch (error) {
    const stderr = (error as { stderr?: Buffer }).stderr?.toString().trim();
    throw new Error(stderr !== undefined && stderr.length > 0 ? stderr : (error as Error).message);
  }
}

export function sendMessage(chatGuid: string, body: string): void {
  runScript(SEND_TEXT, [chatGuid, body]);
}

/**
 * Ask Messages how many conversations it has, which sends nothing.
 *
 * This exists so the Automation permission can be established and inspected
 * without texting anyone. macOS creates the entry in Privacy & Security >
 * Automation when a client first asks — so before this, the only way to get an
 * entry you could switch off was to send a real message to a real person.
 *
 * It has to be an event that TCC actually gates. Standard Suite events like
 * `get name` are answered without any grant at all, so probing with one reports
 * "allowed" on a machine where sending is refused, and creates no entry.
 * Reaching the app's own data is what triggers the check; a count reveals
 * nothing about who anyone is talking to.
 */
export function checkAutomation(): { allowed: boolean; detail: string } {
  try {
    const count = execFileSync(
      'osascript',
      ['-e', 'tell application "Messages" to count of chats'],
      { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] },
    ).trim();
    return { allowed: true, detail: `${count} conversations visible to Messages` };
  } catch (error) {
    const stderr = (error as { stderr?: Buffer }).stderr?.toString().trim() ?? '';
    return { allowed: false, detail: stderr.length > 0 ? stderr : (error as Error).message };
  }
}

/**
 * Send an attachment the client handed over as bytes.
 *
 * The daemon never takes a path. It holds Full Disk Access, so accepting one
 * here would turn `send` into "read any file on the disk and text it to
 * someone" — the exact shape §6 exists to prevent. The client reads the file
 * with its own permissions; the daemon only writes what it is given, into a
 * directory it owns.
 */
export function sendAttachment(chatGuid: string, name: string, data: Buffer): void {
  const directory = mkdtempSync(join(tmpdir(), 'msgd-'));
  const safe = basename(name).replace(/^\.+/, '');
  const path = join(directory, safe.length > 0 ? safe : 'attachment');
  writeFileSync(path, data, { mode: 0o600 });

  try {
    runScript(SEND_FILE, [chatGuid, path]);
  } finally {
    // Messages reads the file after the event returns, so it cannot be removed
    // immediately.
    const later = setTimeout(() => rmSync(directory, { recursive: true, force: true }),
      ATTACHMENT_LIFETIME_MS);
    later.unref();
  }
}
