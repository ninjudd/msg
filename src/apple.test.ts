import { describe, expect, it } from 'vitest';
import {
  decodeAttributedBody,
  fromAppleDate,
  messageBody,
  sinceToAppleDate,
  toAppleDate,
} from './apple.js';

/** Build a typedstream blob shaped like the one Messages stores. */
function typedStream(text: string): Uint8Array {
  const body = Buffer.from(text, 'utf8');
  const length =
    body.length < 0x80
      ? Buffer.from([body.length])
      : Buffer.concat([Buffer.from([0x81]), lengthAsUInt16(body.length)]);
  return Buffer.concat([
    Buffer.from([0x04, 0x0b]),
    Buffer.from('streamtyped'),
    Buffer.from([0x81, 0xe8, 0x03, 0x84, 0x01, 0x40, 0x84, 0x84, 0x84]),
    Buffer.from('NSString'),
    Buffer.from([0x01, 0x94, 0x84, 0x01, 0x2b]),
    length,
    body,
  ]);
}

function lengthAsUInt16(value: number): Buffer {
  const buffer = Buffer.alloc(2);
  buffer.writeUInt16LE(value);
  return buffer;
}

describe('apple dates', () => {
  it('reads nanosecond timestamps', () => {
    // 2026-08-06T00:00:00Z is 807_667_200 seconds after the Apple epoch.
    const date = fromAppleDate(807_667_200n * 1_000_000_000n);
    expect(date?.toISOString()).toBe('2026-08-06T00:00:00.000Z');
  });

  it('reads legacy second timestamps', () => {
    const date = fromAppleDate(807_667_200n);
    expect(date?.toISOString()).toBe('2026-08-06T00:00:00.000Z');
  });

  it('survives values beyond Number.MAX_SAFE_INTEGER', () => {
    const raw = 807_667_200n * 1_000_000_000n;
    expect(raw > BigInt(Number.MAX_SAFE_INTEGER)).toBe(true);
    expect(fromAppleDate(raw)).not.toBeNull();
  });

  it('round-trips through toAppleDate', () => {
    const original = new Date('2026-03-01T12:34:56.000Z');
    expect(fromAppleDate(toAppleDate(original))?.toISOString()).toBe(original.toISOString());
  });

  it('treats null and zero as no date', () => {
    expect(fromAppleDate(null)).toBeNull();
    expect(fromAppleDate(0n)).toBeNull();
  });
});

describe('since parsing', () => {
  it('accepts durations', () => {
    const twoHours = fromAppleDate(sinceToAppleDate('2h'));
    const expected = Date.now() - 2 * 3_600_000;
    expect(Math.abs((twoHours?.getTime() ?? 0) - expected)).toBeLessThan(5_000);
  });

  it('accepts ISO dates', () => {
    expect(fromAppleDate(sinceToAppleDate('2026-01-15'))?.toISOString()).toBe(
      new Date('2026-01-15').toISOString(),
    );
  });

  it('rejects nonsense', () => {
    expect(() => sinceToAppleDate('soonish')).toThrow(/cannot parse time/);
  });
});

describe('attributedBody', () => {
  it('decodes a short string', () => {
    expect(decodeAttributedBody(typedStream('hey are you around'))).toBe('hey are you around');
  });

  it('decodes a string longer than one length byte', () => {
    const long = 'x'.repeat(500);
    expect(decodeAttributedBody(typedStream(long))).toBe(long);
  });

  it('decodes multi-byte characters', () => {
    expect(decodeAttributedBody(typedStream('café ☕️'))).toBe('café ☕️');
  });

  it('returns null for a blob that is not a typedstream', () => {
    expect(decodeAttributedBody(Buffer.from('not an archive'))).toBeNull();
  });

  it('returns null for empty input', () => {
    expect(decodeAttributedBody(null)).toBeNull();
    expect(decodeAttributedBody(new Uint8Array())).toBeNull();
  });
});

describe('messageBody', () => {
  it('prefers the text column', () => {
    expect(messageBody('plain text', typedStream('archived'))).toBe('plain text');
  });

  it('falls back to the archived body', () => {
    expect(messageBody(null, typedStream('archived'))).toBe('archived');
  });

  it('returns null when both are missing', () => {
    expect(messageBody(null, null)).toBeNull();
  });
});
