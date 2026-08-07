import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { readConfig } from './config.js';

let directory: string;

beforeAll(() => {
  directory = mkdtempSync(join(tmpdir(), 'msg-config-'));
});

afterAll(() => {
  rmSync(directory, { recursive: true, force: true });
});

function withConfig(contents: string): string {
  const path = join(directory, `config-${String(contents.length)}.toml`);
  writeFileSync(path, contents);
  return path;
}

describe('readConfig', () => {
  it('is off when the file does not exist', () => {
    expect(readConfig(join(directory, 'absent.toml')).send).toBe(false);
  });

  it('is off when the file exists but says nothing', () => {
    expect(readConfig(withConfig('# nothing here\n')).send).toBe(false);
  });

  it('reads send = true', () => {
    expect(readConfig(withConfig('send = true\n')).send).toBe(true);
  });

  it('reads send = false', () => {
    expect(readConfig(withConfig('send = false\n')).send).toBe(false);
  });

  it('tolerates whitespace, comments, and unrelated keys', () => {
    const path = withConfig('# a comment\nnames = false\n\n   send   =   true   # yes\n');
    expect(readConfig(path).send).toBe(true);
  });

  it('does not treat a lookalike key as the switch', () => {
    expect(readConfig(withConfig('sendmail = true\n')).send).toBe(false);
    expect(readConfig(withConfig('resend = true\n')).send).toBe(false);
  });

  it('refuses anything that is not a bare boolean', () => {
    expect(readConfig(withConfig('send = "true"\n')).send).toBe(false);
    expect(readConfig(withConfig('send = 1\n')).send).toBe(false);
  });
});
