import { describe, expect, it } from 'vitest';
import { parseIdentities } from './identity.js';

/** The shape `security find-identity -p codesigning` prints. */
const OUTPUT = `  1) 78AFD4F9A0302C18475498366167A08B6B3C291C "Apple Development: Someone (ABCDE12345)"
  7) 33473595F6D32D06069490C093920A918039E023 "msg dev" (CSSMERR_TP_NOT_TRUSTED)
     2 identities found
`;

describe('parseIdentities', () => {
  it('reads the names out', () => {
    expect(parseIdentities(OUTPUT)).toEqual([
      'Apple Development: Someone (ABCDE12345)',
      'msg dev',
    ]);
  });

  it('keeps an untrusted identity, which still signs', () => {
    // codesign exits 0 with one of these, so refusing it here would send the
    // build off to create a second certificate it does not need.
    expect(parseIdentities(OUTPUT)).toContain('msg dev');
  });

  it('is empty when nothing is listed', () => {
    expect(parseIdentities('     0 identities found\n')).toEqual([]);
  });
});
