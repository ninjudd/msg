/** The agent launchd has to parse, checked with the parser launchd uses. */

import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { LABEL } from './protocol.js';
import { plist } from './install.js';

let directory: string;

beforeAll(() => {
  directory = mkdtempSync(join(tmpdir(), 'msg-plist-'));
});

afterAll(() => {
  rmSync(directory, { recursive: true, force: true });
});

describe('plist', () => {
  it('is valid property list XML', () => {
    const path = join(directory, 'agent.plist');
    writeFileSync(path, plist('/usr/local/libexec/msgd', '/tmp/msgd.log'));
    expect(() => execFileSync('plutil', ['-lint', path], { stdio: 'ignore' })).not.toThrow();
  });

  it('runs the binary it was given, under the label the grant is keyed to', () => {
    const path = join(directory, 'parsed.plist');
    writeFileSync(path, plist('/opt/msgd', '/tmp/msgd.log'));
    const parsed = JSON.parse(
      execFileSync('plutil', ['-convert', 'json', '-o', '-', path], { encoding: 'utf8' }),
    ) as { Label: string; ProgramArguments: string[]; KeepAlive: boolean };

    expect(parsed.Label).toBe(LABEL);
    expect(parsed.ProgramArguments).toEqual(['/opt/msgd']);
    // Resident rather than socket-activated: launch_activate_socket(3) is not
    // reachable from Node (daemon-and-permissions.md §3).
    expect(parsed.KeepAlive).toBe(true);
  });
});
