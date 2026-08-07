/** The agent launchd has to parse, checked with the parser launchd uses. */

import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { LABEL } from './protocol.js';
import {
  binaryPath,
  builtBundle,
  bundlePath,
  daemonEnvironment,
  describeSignature,
  plist,
} from './install.js';

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

describe('bundle layout', () => {
  // TCC resolves an executable back to the bundle above it by walking up from
  // Contents/MacOS. Anywhere else and it finds nothing, falls back to keying the
  // grant by path, and the Automation switch stops working (§13).
  it('runs the executable from inside Contents/MacOS', () => {
    expect(binaryPath()).toBe(join(bundlePath(), 'Contents', 'MacOS', 'msgd'));
    expect(bundlePath().endsWith('.app')).toBe(true);
  });

  it('installs from the bundle the build produces', () => {
    expect(builtBundle().endsWith('/build/msgd.app')).toBe(true);
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

describe('daemon environment', () => {
  it('carries only the settings the daemon itself reads', () => {
    expect(
      daemonEnvironment({
        MSG_SOCKET: '/tmp/msgd.sock',
        MSG_CONFIG: '/tmp/config.toml',
        MSG_SIGN_IDENTITY: 'msg dev',
        PATH: '/usr/bin',
        MSG_EMPTY: '',
      }),
    ).toEqual({ MSG_SOCKET: '/tmp/msgd.sock', MSG_CONFIG: '/tmp/config.toml' });
  });

  it('never carries MSG_DB, which would outlive the shell that set it', () => {
    // The CLI answers MSG_DB locally, so persisting it could only produce a
    // daemon pinned to a fixture that a later shell knows nothing about.
    expect(daemonEnvironment({ MSG_DB: '/tmp/fixture.db' })).toEqual({});
    expect(
      daemonEnvironment({ MSG_DB: '/tmp/fixture.db', MSG_SOCKET: '/tmp/msgd.sock' }),
    ).toEqual({ MSG_SOCKET: '/tmp/msgd.sock' });
  });

  it('writes them into the job, since launchd inherits nothing', () => {
    const path = join(directory, 'environment.plist');
    writeFileSync(path, plist('/opt/msgd', '/tmp/msgd.log', { MSG_SOCKET: '/tmp/msgd.sock' }));
    const parsed = JSON.parse(
      execFileSync('plutil', ['-convert', 'json', '-o', '-', path], { encoding: 'utf8' }),
    ) as { EnvironmentVariables: Record<string, string> };
    expect(parsed.EnvironmentVariables).toEqual({ MSG_SOCKET: '/tmp/msgd.sock' });
  });

  it('omits the key entirely when there is nothing to carry', () => {
    const path = join(directory, 'bare.plist');
    writeFileSync(path, plist('/opt/msgd', '/tmp/msgd.log'));
    const parsed = JSON.parse(
      execFileSync('plutil', ['-convert', 'json', '-o', '-', path], { encoding: 'utf8' }),
    ) as Record<string, unknown>;
    expect(parsed['EnvironmentVariables']).toBeUndefined();
  });

  it('escapes values rather than producing invalid XML', () => {
    const path = join(directory, 'escaped.plist');
    writeFileSync(path, plist('/opt/msgd', '/tmp/msgd.log', { MSG_DB: '/tmp/a&b<c>.db' }));
    expect(() => execFileSync('plutil', ['-lint', path], { stdio: 'ignore' })).not.toThrow();
    const parsed = JSON.parse(
      execFileSync('plutil', ['-convert', 'json', '-o', '-', path], { encoding: 'utf8' }),
    ) as { EnvironmentVariables: Record<string, string> };
    expect(parsed.EnvironmentVariables['MSG_DB']).toBe('/tmp/a&b<c>.db');
  });
});
