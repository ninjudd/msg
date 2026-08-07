import { describe, expect, it } from 'vitest';
import { addInfoPlistSection, findSection } from './macho.js';

const LC_SEGMENT_64 = 0x19;
const LC_UUID = 0x1b;

/**
 * A minimal Mach-O: one `__TEXT` segment holding one section, followed by a
 * second load command whose survival proves the shift arithmetic is right.
 */
function synthetic(): Buffer {
  const file = Buffer.alloc(8192);
  const segmentSize = 72 + 80;
  const uuidSize = 24;

  file.writeUInt32LE(0xfeedfacf, 0); // magic
  file.writeUInt32LE(0x0100000c, 4); // cputype arm64
  file.writeUInt32LE(2, 12); // filetype MH_EXECUTE
  file.writeUInt32LE(2, 16); // ncmds
  file.writeUInt32LE(segmentSize + uuidSize, 20); // sizeofcmds

  const segment = 32;
  file.writeUInt32LE(LC_SEGMENT_64, segment);
  file.writeUInt32LE(segmentSize, segment + 4);
  file.write('__TEXT', segment + 8);
  file.writeBigUInt64LE(0x1_0000_0000n, segment + 24); // vmaddr
  file.writeBigUInt64LE(0x4000n, segment + 32); // vmsize
  file.writeBigUInt64LE(0n, segment + 40); // fileoff
  file.writeBigUInt64LE(8192n, segment + 48); // filesize
  file.writeUInt32LE(1, segment + 64); // nsects

  const section = segment + 72;
  file.write('__text', section);
  file.write('__TEXT', section + 16);
  file.writeBigUInt64LE(0x1_0000_1000n, section + 32); // addr
  file.writeBigUInt64LE(16n, section + 40); // size
  file.writeUInt32LE(4096, section + 48); // offset

  const uuid = segment + segmentSize;
  file.writeUInt32LE(LC_UUID, uuid);
  file.writeUInt32LE(uuidSize, uuid + 4);
  file.fill(0xab, uuid + 8, uuid + 24);

  return file;
}

const PLIST = Buffer.from('<plist><dict><key>Marker</key></dict></plist>', 'utf8');

describe('addInfoPlistSection', () => {
  it('adds a readable __TEXT,__info_plist section', () => {
    const out = addInfoPlistSection(synthetic(), PLIST);
    const located = findSection(out, '__TEXT', '__info_plist');
    expect(located).not.toBeNull();
    expect(located?.size).toBe(PLIST.length);
    expect(
      out.subarray(located?.offset, (located?.offset ?? 0) + PLIST.length).toString('utf8'),
    ).toBe(PLIST.toString('utf8'));
  });

  it('keeps the payload inside the segment it claims to be in', () => {
    const out = addInfoPlistSection(synthetic(), PLIST);
    const located = findSection(out, '__TEXT', '__info_plist');
    const sectionHeader = 32 + 72 + 80;
    const addr = out.readBigUInt64LE(sectionHeader + 32);
    // __TEXT maps from file offset 0, so the address is vmaddr plus the offset.
    expect(addr).toBe(0x1_0000_0000n + BigInt(located?.offset ?? 0));
  });

  it('grows the header and the segment by exactly one section', () => {
    const before = synthetic();
    const out = addInfoPlistSection(before, PLIST);
    expect(out.readUInt32LE(20)).toBe(before.readUInt32LE(20) + 80);
    expect(out.readUInt32LE(32 + 4)).toBe(before.readUInt32LE(32 + 4) + 80);
    expect(out.readUInt32LE(32 + 64)).toBe(2);
    expect(out.length).toBe(before.length);
  });

  it('shifts the load commands that follow rather than overwriting them', () => {
    const out = addInfoPlistSection(synthetic(), PLIST);
    const moved = 32 + 72 + 80 + 80;
    expect(out.readUInt32LE(moved)).toBe(LC_UUID);
    expect(out.subarray(moved + 8, moved + 24).every((byte) => byte === 0xab)).toBe(true);
  });

  it('leaves the existing section header alone', () => {
    const before = synthetic();
    const out = addInfoPlistSection(before, PLIST);
    expect(out.subarray(32 + 72, 32 + 152)).toEqual(before.subarray(32 + 72, 32 + 152));
  });

  it('refuses to add a second one', () => {
    const once = addInfoPlistSection(synthetic(), PLIST);
    expect(() => addInfoPlistSection(once, PLIST)).toThrow(/already present/);
  });

  it('refuses when the padding cannot hold the payload', () => {
    expect(() => addInfoPlistSection(synthetic(), Buffer.alloc(5000))).toThrow(
      /not enough padding/,
    );
  });

  it('rejects a universal binary', () => {
    const fat = Buffer.alloc(64);
    fat.writeUInt32BE(0xcafebabe, 0);
    expect(() => addInfoPlistSection(fat, PLIST)).toThrow(/Mach-O/);
  });
});
