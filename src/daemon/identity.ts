/**
 * The code signing identity `msgd` is signed with, created on first use.
 *
 * An ad-hoc signature is matched by cdhash, so every rebuild invalidates the
 * daemon's Full Disk Access grant and it has to be added again by hand. A
 * stable identity anchors the requirement to a certificate instead, and the
 * grant survives rebuilds (§4). Nothing here talks to Apple: `codesign` is an
 * offline operation and a self-signed certificate needs no developer account.
 *
 * Two things this deliberately does not do:
 *
 * - **No `add-trusted-cert`.** Signing works with an untrusted self-signed
 *   certificate — measured, `codesign` exits 0 while `security find-identity`
 *   still reports `CSSMERR_TP_NOT_TRUSTED`. Trust settings would need
 *   authorisation and buy nothing here.
 * - **No `set-key-partition-list`.** Leaving it unset means macOS asks before
 *   `codesign` uses the key. That prompt is the only thing stopping local code
 *   from silently signing a replacement daemon and inheriting its grant, which
 *   would walk back the scope reduction the daemon exists for (§6). Answering
 *   it with "Always Allow" trades that away.
 */

import { randomBytes } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { join } from 'node:path';

export const IDENTITY_NAME = 'msg dev';

/**
 * LibreSSL, not whatever is on PATH. OpenSSL 3 writes PKCS#12 with defaults the
 * macOS Security framework rejects with "MAC verification failed", which reads
 * as a wrong password and is not one.
 */
const OPENSSL = '/usr/bin/openssl';

const CERTIFICATE_DAYS = '3650';

function run(command: string, args: string[]): string {
  return execFileSync(command, args, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] });
}

/** Identity names `codesign` can see, valid or not. */
export function parseIdentities(output: string): string[] {
  const names: string[] = [];
  for (const line of output.split('\n')) {
    const match = /^\s*\d+\)\s+[0-9A-F]+\s+"(.+?)"/.exec(line);
    if (match?.[1] !== undefined) names.push(match[1]);
  }
  return names;
}

export function findIdentity(name = IDENTITY_NAME): string | null {
  let output: string;
  try {
    // Not `-v`: a self-signed certificate is not "valid", and signs anyway.
    output = run('security', ['find-identity', '-p', 'codesigning']);
  } catch {
    return null;
  }
  return parseIdentities(output).includes(name) ? name : null;
}

function loginKeychain(): string {
  try {
    const output = run('security', ['default-keychain', '-d', 'user']).trim();
    const quoted = /"(.+)"/.exec(output);
    if (quoted?.[1] !== undefined) return quoted[1];
  } catch {
    // Fall through to the conventional path.
  }
  return join(homedir(), 'Library', 'Keychains', 'login.keychain-db');
}

function createIdentity(name: string): void {
  const directory = mkdtempSync(join(tmpdir(), 'msg-identity-'));
  // The passphrase only protects the file between these two commands.
  const passphrase = randomBytes(16).toString('hex');
  try {
    const config = join(directory, 'openssl.cnf');
    writeFileSync(
      config,
      `[req]\ndistinguished_name = dn\nx509_extensions = v3\nprompt = no\n` +
        `[dn]\nCN = ${name}\n` +
        `[v3]\nbasicConstraints = critical,CA:false\n` +
        `keyUsage = critical,digitalSignature\n` +
        `extendedKeyUsage = critical,codeSigning\n`,
    );
    const key = join(directory, 'key.pem');
    const certificate = join(directory, 'cert.pem');
    const bundle = join(directory, 'identity.p12');

    run(OPENSSL, [
      'req', '-x509', '-newkey', 'rsa:2048', '-sha256', '-days', CERTIFICATE_DAYS,
      '-nodes', '-keyout', key, '-out', certificate, '-config', config,
    ]);
    run(OPENSSL, [
      'pkcs12', '-export', '-inkey', key, '-in', certificate, '-name', name,
      '-keypbe', 'PBE-SHA1-3DES', '-certpbe', 'PBE-SHA1-3DES', '-macalg', 'sha1',
      '-out', bundle, '-passout', `pass:${passphrase}`,
    ]);
    // -T lets codesign reach the key without naming every other tool.
    run('security', [
      'import', bundle, '-k', loginKeychain(), '-P', passphrase, '-T', '/usr/bin/codesign',
    ]);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

export interface Identity {
  name: string;
  created: boolean;
}

/** The signing identity, creating it the first time. */
export function ensureIdentity(name = IDENTITY_NAME): Identity {
  if (findIdentity(name) !== null) return { name, created: false };
  createIdentity(name);
  if (findIdentity(name) === null) {
    throw new Error(`created a certificate named ${name} but codesign cannot see it`);
  }
  return { name, created: true };
}
