/** Apple-specific encodings used by the Messages database. */

/** Seconds between the Unix epoch and Apple's 2001-01-01 epoch. */
const APPLE_EPOCH_OFFSET_SECONDS = 978_307_200n;
const APPLE_EPOCH_OFFSET_MS = APPLE_EPOCH_OFFSET_SECONDS * 1_000n;

/**
 * Dates recorded before the 2011 schema change are in seconds; everything
 * since is in nanoseconds. Values above this are nanoseconds.
 */
const NANOSECOND_THRESHOLD = 1_000_000_000_000n;

const NANOSECONDS_PER_MS = 1_000_000n;

/** Convert an Apple timestamp to a Date. */
export function fromAppleDate(value: bigint | null): Date | null {
  if (value === null || value === 0n) return null;
  const ms =
    absBigInt(value) > NANOSECOND_THRESHOLD
      ? value / NANOSECONDS_PER_MS
      : value * 1_000n;
  return new Date(Number(ms + APPLE_EPOCH_OFFSET_MS));
}

/** Convert a Date to Apple's nanosecond timestamp. */
export function toAppleDate(date: Date): bigint {
  return (BigInt(date.getTime()) - APPLE_EPOCH_OFFSET_MS) * NANOSECONDS_PER_MS;
}

function absBigInt(value: bigint): bigint {
  return value < 0n ? -value : value;
}

const DURATION_PATTERN = /^(\d+(?:\.\d+)?)([mhdw])$/;

const DURATION_MS: Record<string, number> = {
  m: 60_000,
  h: 3_600_000,
  d: 86_400_000,
  w: 604_800_000,
};

/** Parse a duration like `2h` or `7d`, or an ISO date, into an Apple timestamp. */
export function sinceToAppleDate(spec: string): bigint {
  const match = DURATION_PATTERN.exec(spec.trim());
  if (match) {
    const [, amount, unit] = match;
    const scale = DURATION_MS[unit as string];
    if (amount !== undefined && scale !== undefined) {
      return toAppleDate(new Date(Date.now() - Number(amount) * scale));
    }
  }
  const parsed = new Date(spec);
  if (Number.isNaN(parsed.getTime())) {
    throw new Error(`cannot parse time ${spec}, expected something like 2h, 7d, or 2026-08-01`);
  }
  return toAppleDate(parsed);
}

/** Marker bytes that open an NSArchiver typedstream. */
const STREAM_HEADER = Buffer.from([0x04, 0x0b, ...Buffer.from('streamtyped')]);

/** Type marker introducing a C string in a typedstream. */
const STRING_MARKER = 0x2b;

const INT_16 = 0x81;
const INT_32 = 0x82;
const INT_64 = 0x83;

interface ReadInt {
  value: number;
  next: number;
}

/**
 * Read a typedstream integer. Values are a single signed byte unless escaped
 * by a width marker.
 */
function readInt(data: Buffer, offset: number): ReadInt | null {
  if (offset >= data.length) return null;
  const marker = data[offset];
  if (marker === undefined) return null;

  if (marker === INT_16) {
    if (offset + 3 > data.length) return null;
    return { value: data.readUInt16LE(offset + 1), next: offset + 3 };
  }
  if (marker === INT_32) {
    if (offset + 5 > data.length) return null;
    return { value: data.readUInt32LE(offset + 1), next: offset + 5 };
  }
  if (marker === INT_64) {
    if (offset + 9 > data.length) return null;
    return { value: Number(data.readBigUInt64LE(offset + 1)), next: offset + 9 };
  }
  return { value: data.readInt8(offset), next: offset + 1 };
}

/**
 * Extract the message text from an archived NSAttributedString.
 *
 * Modern macOS leaves message.text NULL and stores the body here. The string
 * contents follow the NSString class name and a 0x2b type marker.
 */
export function decodeAttributedBody(blob: Uint8Array | null): string | null {
  if (blob === null || blob.length === 0) return null;
  const data = Buffer.from(blob.buffer, blob.byteOffset, blob.byteLength);
  if (!data.subarray(0, STREAM_HEADER.length).equals(STREAM_HEADER)) return null;

  let start = data.indexOf('NSString');
  if (start === -1) start = data.indexOf('NSMutableString');
  if (start === -1) return null;

  const marker = data.indexOf(STRING_MARKER, start);
  if (marker === -1) return null;

  const length = readInt(data, marker + 1);
  if (length === null || length.value <= 0) return null;
  if (length.next + length.value > data.length) return null;

  return data.subarray(length.next, length.next + length.value).toString('utf8');
}

/** Return a message's text, falling back to the archived body. */
export function messageBody(text: string | null, attributedBody: Uint8Array | null): string | null {
  return text ?? decodeAttributedBody(attributedBody);
}
