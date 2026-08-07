import { describe, expect, it } from 'vitest';
import { EMPTY_INDEX, handleKey, nameHandles, type ContactIndex } from './contacts.js';

describe('handleKey', () => {
  it('reduces every stored phone format to the same key', () => {
    const formats = [
      '+13105551234',
      '(310) 555-1234',
      '310-555-1234',
      '3105551234',
      '+1 (310) 555-1234',
      '1 (310) 555-1234',
      '310.555.1234',
      '1-310-555-1234',
    ];
    const keys = new Set(formats.map((f) => handleKey(f)));
    expect(keys).toEqual(new Set(['3105551234']));
  });

  it('lowercases email addresses', () => {
    expect(handleKey('Dana@Example.COM')).toBe('dana@example.com');
    expect(handleKey('  dana@example.com  ')).toBe('dana@example.com');
  });

  it('keeps short codes intact', () => {
    expect(handleKey('22000')).toBe('22000');
  });

  it('matches international numbers on their final digits', () => {
    expect(handleKey('+442071234567')).toBe('2071234567');
  });

  it('returns null for empty or digitless handles', () => {
    expect(handleKey('')).toBeNull();
    expect(handleKey('   ')).toBeNull();
    expect(handleKey('---')).toBeNull();
  });
});

describe('nameHandles', () => {
  const index: ContactIndex = {
    size: 2,
    problems: [],
    lookup: (handle) => {
      const key = handle === null ? null : handleKey(handle);
      if (key === '3105551234') return 'Dana Reyes';
      if (key === '4155559876') return 'Sam Oyelaran';
      return null;
    },
  };

  it('names every handle it recognizes', () => {
    expect(nameHandles(index, '+13105551234,+14155559876')).toBe('Dana Reyes, Sam Oyelaran');
  });

  it('leaves unknown handles as they were', () => {
    expect(nameHandles(index, '+13105551234,+19998887777')).toBe('Dana Reyes, +19998887777');
  });

  it('passes null through', () => {
    expect(nameHandles(index, null)).toBeNull();
  });

  it('is a no-op against the empty index', () => {
    expect(nameHandles(EMPTY_INDEX, '+13105551234')).toBe('+13105551234');
    expect(EMPTY_INDEX.lookup('+13105551234')).toBeNull();
  });
});
