/** Sending messages through Messages.app. */

import { execFileSync } from 'node:child_process';

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

export function sendFile(chatGuid: string, filePath: string): void {
  runScript(SEND_FILE, [chatGuid, filePath]);
}
