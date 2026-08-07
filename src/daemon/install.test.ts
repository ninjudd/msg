/** The agent launchd has to parse, checked with the parser launchd uses. */

import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { LABEL } from './protocol.js';
import { describeSignature, plist } from './install.js';

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

describe('describeSignature', () => {
  // A self-signed certificate produces no Authority line, and the field is
  // `Signature size=`, not `Signature=` — reading it wrong reported a properly
  // signed daemon as unsigned.
  const selfSigned = [
    'Identifier=com.ninjudd.msgd',
    'CodeDirectory v=20400 size=234857 flags=0x0(none) hashes=7334+2 location=embedded',
    'Signature size=1660',
    'Info.plist entries=4',
  ].join('\n');

  it('recognises a signature with no Authority line', () => {
    expect(describeSignature(selfSigned)).toBe('signed');
  });

  it('calls out ad-hoc, since its grant dies on the next rebuild', () => {
    expect(
      describeSignature('CodeDirectory v=20400 size=1 flags=0x2(adhoc) hashes=1+2\nSignature size=1'),
    ).toBe('ad-hoc');
  });

  it('prefers the authority when there is one', () => {
    expect(describeSignature('Signature size=9\nAuthority=Apple Development: Someone\n')).toBe(
      'Apple Development: Someone',
    );
  });

  it('reports an unsigned binary as unsigned', () => {
    expect(describeSignature('Identifier=x\nFormat=Mach-O thin (arm64)\n')).toBe('unsigned');
  });
});
