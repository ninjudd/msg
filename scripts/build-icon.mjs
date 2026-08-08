#!/usr/bin/env node
/**
 * Draw the daemon's icon: a message bubble with a shell prompt in it.
 *
 * **This is not part of the build.** It writes `assets/msgd.icns`, which is
 * committed, and `pnpm build:msgd` only copies that file. Run it by hand to
 * change the icon. Keeping it out of the build is deliberate: an icon generator
 * written in JavaScript would be the one thing still requiring node after
 * rust-rewrite.md, which would be an absurd reason to keep a runtime.
 *
 * It is drawn from signed distance fields, which give exact antialiasing at
 * every size — mattering more than usual here, because where this icon actually
 * appears is the Full Disk Access and Automation lists in System Settings, at 16
 * and 32 points. Everything is laid out in a unit square and scaled per size.
 */

import { execFileSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { deflateSync } from 'node:zlib';

const root = fileURLToPath(new URL('..', import.meta.url));
const icns = join(root, 'assets', 'msgd.icns');

/** Apple's proportions: an 824pt rounded rect on a 1024pt canvas. */
const PLATE = { inset: 0.0977, radius: 0.1811 };

const TERMINAL_TOP = [0x2b, 0x2d, 0x35];
const TERMINAL_BOTTOM = [0x17, 0x18, 0x1d];
const BUBBLE_TOP = [0x4a, 0xe2, 0x74];
const BUBBLE_BOTTOM = [0x21, 0xa8, 0x4a];
const GLYPH = [0x14, 0x15, 0x1a];

const clamp = (value, low, high) => (value < low ? low : value > high ? high : value);
const mix = (a, b, t) => a.map((channel, index) => Math.round(channel + (b[index] - channel) * t));

/** Distance to a box centred at (cx, cy) with half-extents (hx, hy), corners rounded by r. */
function roundedBox(x, y, cx, cy, hx, hy, r) {
  const qx = Math.abs(x - cx) - hx + r;
  const qy = Math.abs(y - cy) - hy + r;
  const outside = Math.hypot(Math.max(qx, 0), Math.max(qy, 0));
  return Math.min(Math.max(qx, qy), 0) + outside - r;
}

/** Distance to a capsule: the segment ab, thickened by r. Round caps come free. */
function capsule(x, y, ax, ay, bx, by, r) {
  const px = x - ax;
  const py = y - ay;
  const dx = bx - ax;
  const dy = by - ay;
  const t = clamp((px * dx + py * dy) / (dx * dx + dy * dy), 0, 1);
  return Math.hypot(px - dx * t, py - dy * t) - r;
}

/** Distance to the triangle abc, negative inside. */
function triangle(x, y, ax, ay, bx, by, cx, cy) {
  const edges = [
    [bx - ax, by - ay, x - ax, y - ay],
    [cx - bx, cy - by, x - bx, y - by],
    [ax - cx, ay - cy, x - cx, y - cy],
  ];
  const winding = Math.sign((bx - ax) * (ay - cy) - (by - ay) * (ax - cx));

  let squared = Infinity;
  let side = Infinity;
  for (const [ex, ey, vx, vy] of edges) {
    const t = clamp((vx * ex + vy * ey) / (ex * ex + ey * ey), 0, 1);
    squared = Math.min(squared, (vx - ex * t) ** 2 + (vy - ey * t) ** 2);
    side = Math.min(side, winding * (vx * ey - vy * ex));
  }
  return -Math.sqrt(squared) * Math.sign(side);
}

/**
 * The bubble's tail: a rounded wedge off the bottom-left corner.
 *
 * Its first two points sit inside the bubble so the union has no seam, and only
 * the third sticks out. Subtracting a circle from the corner instead — the
 * obvious approach — produced a stubby foot rather than a tail.
 */
function tail(x, y) {
  return triangle(x, y, 0.285, 0.545, 0.395, 0.608, 0.255, 0.672) - 0.028;
}

function bubble(x, y) {
  return Math.min(roundedBox(x, y, 0.5, 0.44, 0.26, 0.185, 0.093), tail(x, y));
}

/**
 * `>_`, sized so the chevron survives being four pixels wide.
 *
 * The cursor sits on the chevron's baseline rather than its centre. One notch
 * higher and the pair reads as `>-`.
 */
function prompt(x, y) {
  const stroke = 0.033;
  return Math.min(
    capsule(x, y, 0.345, 0.345, 0.458, 0.44, stroke),
    capsule(x, y, 0.458, 0.44, 0.345, 0.535, stroke),
    roundedBox(x, y, 0.6, 0.535, 0.082, 0.027, 0.025),
  );
}

function render(size) {
  const pixels = Buffer.alloc(size * size * 4);
  // One pixel in unit coordinates: the width over which an edge fades.
  const edge = 1 / size;

  for (let row = 0; row < size; row += 1) {
    for (let column = 0; column < size; column += 1) {
      const x = (column + 0.5) / size;
      const y = (row + 0.5) / size;

      const plate = roundedBox(x, y, 0.5, 0.5, 0.5 - PLATE.inset, 0.5 - PLATE.inset, PLATE.radius);
      let color = mix(TERMINAL_TOP, TERMINAL_BOTTOM, y);
      let alpha = clamp(0.5 - plate / edge, 0, 1);

      const onBubble = clamp(0.5 - bubble(x, y) / edge, 0, 1);
      if (onBubble > 0) {
        const shade = mix(BUBBLE_TOP, BUBBLE_BOTTOM, clamp((y - 0.25) / 0.42, 0, 1));
        color = mix(color, shade, onBubble);
        alpha = Math.max(alpha, onBubble);
      }

      const onPrompt = clamp(0.5 - prompt(x, y) / edge, 0, 1);
      if (onPrompt > 0) color = mix(color, GLYPH, onPrompt);

      const at = (row * size + column) * 4;
      pixels[at] = color[0];
      pixels[at + 1] = color[1];
      pixels[at + 2] = color[2];
      pixels[at + 3] = Math.round(alpha * 255);
    }
  }
  return pixels;
}

const CRC_TABLE = Array.from({ length: 256 }, (_, index) => {
  let c = index;
  for (let bit = 0; bit < 8; bit += 1) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});

function crc32(buffer) {
  let c = 0xffffffff;
  for (const byte of buffer) c = CRC_TABLE[(c ^ byte) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type, 'ascii'), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body));
  return Buffer.concat([length, body, crc]);
}

function png(size, pixels) {
  const header = Buffer.alloc(13);
  header.writeUInt32BE(size, 0);
  header.writeUInt32BE(size, 4);
  header[8] = 8; // bit depth
  header[9] = 6; // truecolour with alpha
  header[10] = 0;
  header[11] = 0;
  header[12] = 0;

  // Every scanline carries a leading filter byte, here always 0 (none).
  const stride = size * 4;
  const raw = Buffer.alloc((stride + 1) * size);
  for (let row = 0; row < size; row += 1) {
    pixels.copy(raw, row * (stride + 1) + 1, row * stride, (row + 1) * stride);
  }

  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', header),
    chunk('IDAT', deflateSync(raw, { level: 9 })),
    chunk('IEND', Buffer.alloc(0)),
  ]);
}

/** The names `iconutil` expects; a size serves two of them at different scales. */
const VARIANTS = [
  [16, 'icon_16x16.png'],
  [32, 'icon_16x16@2x.png'],
  [32, 'icon_32x32.png'],
  [64, 'icon_32x32@2x.png'],
  [128, 'icon_128x128.png'],
  [256, 'icon_128x128@2x.png'],
  [256, 'icon_256x256.png'],
  [512, 'icon_256x256@2x.png'],
  [512, 'icon_512x512.png'],
  [1024, 'icon_512x512@2x.png'],
];

// `iconutil` reads a directory of PNGs; nothing needs it afterwards.
const iconset = join(mkdtempSync(join(tmpdir(), 'msgd-icon-')), 'msgd.iconset');
mkdirSync(iconset, { recursive: true });
mkdirSync(join(root, 'assets'), { recursive: true });

const drawn = new Map();
for (const [size, name] of VARIANTS) {
  if (!drawn.has(size)) drawn.set(size, png(size, render(size)));
  writeFileSync(join(iconset, name), drawn.get(size));
}

execFileSync('iconutil', ['--convert', 'icns', iconset, '--output', icns]);
rmSync(join(iconset, '..'), { recursive: true, force: true });
process.stdout.write(`wrote ${icns}\n`);
