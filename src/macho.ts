/**
 * Just enough Mach-O to give a bare executable an embedded `Info.plist`.
 *
 * macOS refuses an Apple Event from a client with no
 * `NSAppleEventsUsageDescription` — it has nothing to put in the prompt, so it
 * denies with -1743 and does not even record an entry to switch on. A `.app`
 * bundle is one way to carry that string; a `__TEXT,__info_plist` section is
 * the other, and it is what `/usr/bin/osascript` and
 * `/usr/libexec/sshd-keygen-wrapper` do. Neither `postject` nor any tool on a
 * stock machine can add that section to a copy of `node`, so this does.
 *
 * Build tooling rather than runtime code, kept here to be typechecked and
 * tested alongside everything else. See
 * docs/projects/all/daemon-and-permissions.md §7.
 */

const MH_MAGIC_64 = 0xfeedfacf;
const LC_SEGMENT_64 = 0x19;

const HEADER_SIZE = 32;
const SEGMENT_COMMAND_SIZE = 72;
const SECTION_SIZE = 80;

/** Offsets within mach_header_64. */
const HEADER_NCMDS = 16;
const HEADER_SIZEOFCMDS = 20;

/** Offsets within segment_command_64. */
const SEGMENT_CMDSIZE = 4;
const SEGMENT_NAME = 8;
const SEGMENT_VMADDR = 24;
const SEGMENT_FILEOFF = 40;
const SEGMENT_NSECTS = 64;

/** Offsets within section_64. */
const SECTION_NAME = 0;
const SECTION_SEGNAME = 16;
const SECTION_ADDR = 32;
const SECTION_SIZE_FIELD = 40;
const SECTION_OFFSET = 48;
const SECTION_ALIGN = 52;

/** Alignment for the payload, matching the `align` field written below. */
const PAYLOAD_ALIGN = 16;

/**
 * Room left between the load commands and the payload.
 *
 * `codesign` appends `LC_CODE_SIGNATURE` after signing, which is written just
 * past the existing load commands. A payload placed there is silently
 * overwritten, and the only symptom is `codesign --verify` reporting an invalid
 * Info.plist. The payload therefore goes at the far end of the padding.
 */
const LOAD_COMMAND_SLACK = 256;

export interface SectionLocation {
  offset: number;
  size: number;
}

function readName(binary: Buffer, at: number): string {
  return binary.subarray(at, at + 16).toString('utf8').replace(/\0+$/, '');
}

function writeName(binary: Buffer, at: number, name: string): void {
  binary.fill(0, at, at + 16);
  binary.write(name, at, 'utf8');
}

interface Segment {
  commandOffset: number;
  name: string;
  vmaddr: bigint;
  fileoff: bigint;
  nsects: number;
  sections: Array<{ offset: number; name: string; fileOffset: number; size: bigint }>;
}

function parse(binary: Buffer): { ncmds: number; sizeofcmds: number; segments: Segment[] } {
  if (binary.readUInt32LE(0) !== MH_MAGIC_64) {
    throw new Error('not a 64-bit little-endian Mach-O (a universal binary needs lipo first)');
  }
  const ncmds = binary.readUInt32LE(HEADER_NCMDS);
  const sizeofcmds = binary.readUInt32LE(HEADER_SIZEOFCMDS);

  const segments: Segment[] = [];
  let cursor = HEADER_SIZE;
  for (let index = 0; index < ncmds; index += 1) {
    const cmd = binary.readUInt32LE(cursor);
    const cmdsize = binary.readUInt32LE(cursor + 4);
    if (cmd === LC_SEGMENT_64) {
      const nsects = binary.readUInt32LE(cursor + SEGMENT_NSECTS);
      const sections = [];
      for (let n = 0; n < nsects; n += 1) {
        const at = cursor + SEGMENT_COMMAND_SIZE + n * SECTION_SIZE;
        sections.push({
          offset: at,
          name: readName(binary, at + SECTION_NAME),
          fileOffset: binary.readUInt32LE(at + SECTION_OFFSET),
          size: binary.readBigUInt64LE(at + SECTION_SIZE_FIELD),
        });
      }
      segments.push({
        commandOffset: cursor,
        name: readName(binary, cursor + SEGMENT_NAME),
        vmaddr: binary.readBigUInt64LE(cursor + SEGMENT_VMADDR),
        fileoff: binary.readBigUInt64LE(cursor + SEGMENT_FILEOFF),
        nsects,
        sections,
      });
    }
    cursor += cmdsize;
  }
  return { ncmds, sizeofcmds, segments };
}

/** Find a section by segment and section name, or null when it is absent. */
export function findSection(
  binary: Buffer,
  segmentName: string,
  sectionName: string,
): SectionLocation | null {
  const { segments } = parse(binary);
  for (const segment of segments) {
    if (segment.name !== segmentName) continue;
    for (const section of segment.sections) {
      if (section.name === sectionName) {
        return { offset: section.fileOffset, size: Number(section.size) };
      }
    }
  }
  return null;
}

/**
 * Add `__TEXT,__info_plist` carrying `plist`.
 *
 * The section header goes at the end of `__TEXT`'s section list, which shifts
 * every load command after it, and the payload goes in the padding between the
 * end of the load commands and the first section's data. Both live inside
 * `__TEXT`'s existing file range, so no offset outside the header moves and the
 * file does not change length. Sign the result afterwards: this invalidates any
 * signature already on it.
 */
export function addInfoPlistSection(binary: Buffer, plist: Buffer): Buffer {
  const parsed = parse(binary);
  const text = parsed.segments.find((segment) => segment.name === '__TEXT');
  if (text === undefined) throw new Error('no __TEXT segment');
  if (text.fileoff !== 0n) throw new Error('__TEXT does not start at file offset 0');
  if (findSection(binary, '__TEXT', '__info_plist') !== null) {
    throw new Error('__TEXT,__info_plist already present');
  }

  // Everything from here to the first section's data is padding we can use.
  const contentStart = Math.min(
    ...parsed.segments
      .flatMap((segment) => segment.sections)
      .filter((section) => section.size > 0n && section.fileOffset > 0)
      .map((section) => section.fileOffset),
  );
  const loadEnd = HEADER_SIZE + parsed.sizeofcmds;
  const newLoadEnd = loadEnd + SECTION_SIZE;
  // As late in the padding as it will sit, to stay clear of load commands that
  // grow after this runs.
  const payloadOffset =
    Math.floor((contentStart - plist.length) / PAYLOAD_ALIGN) * PAYLOAD_ALIGN;
  if (payloadOffset < newLoadEnd + LOAD_COMMAND_SLACK) {
    throw new Error(
      `not enough padding for an embedded Info.plist: ${String(plist.length)} bytes wanted ` +
        `between the load commands ending at ${String(newLoadEnd)} and the first section at ` +
        String(contentStart),
    );
  }

  const out = Buffer.from(binary);
  const insertAt = text.commandOffset + SEGMENT_COMMAND_SIZE + text.nsects * SECTION_SIZE;

  // Shift the load commands that follow __TEXT's section list to make room.
  out.copyWithin(insertAt + SECTION_SIZE, insertAt, loadEnd);

  const section = Buffer.alloc(SECTION_SIZE);
  writeName(section, SECTION_NAME, '__info_plist');
  writeName(section, SECTION_SEGNAME, '__TEXT');
  section.writeBigUInt64LE(text.vmaddr + BigInt(payloadOffset), SECTION_ADDR);
  section.writeBigUInt64LE(BigInt(plist.length), SECTION_SIZE_FIELD);
  section.writeUInt32LE(payloadOffset, SECTION_OFFSET);
  section.writeUInt32LE(Math.log2(PAYLOAD_ALIGN), SECTION_ALIGN);
  section.copy(out, insertAt);

  out.writeUInt32LE(
    out.readUInt32LE(text.commandOffset + SEGMENT_CMDSIZE) + SECTION_SIZE,
    text.commandOffset + SEGMENT_CMDSIZE,
  );
  out.writeUInt32LE(text.nsects + 1, text.commandOffset + SEGMENT_NSECTS);
  out.writeUInt32LE(parsed.sizeofcmds + SECTION_SIZE, HEADER_SIZEOFCMDS);

  out.fill(0, payloadOffset, payloadOffset + plist.length);
  plist.copy(out, payloadOffset);
  return out;
}
