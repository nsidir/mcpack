// Renders the app mark (green block with a prompt glyph) to src-tauri/app-icon.png.
// Then: pnpm tauri icon src-tauri/app-icon.png
import { writeFileSync } from "node:fs";
import { deflateSync } from "node:zlib";

const N = 1024;
const U = N / 16; // the mark is drawn on a 16-unit grid, same as the SVG in Shell.tsx
const px = Buffer.alloc(N * N * 4);

const clamp = (v) => Math.min(1, Math.max(0, v));
const roundRect = (x, y, half, r) => {
  const dx = Math.abs(x - 8) - (half - r);
  const dy = Math.abs(y - 8) - (half - r);
  return Math.hypot(Math.max(dx, 0), Math.max(dy, 0)) + Math.min(Math.max(dx, dy), 0) - r;
};
const segment = (x, y, [ax, ay, bx, by]) => {
  const t = clamp(((x - ax) * (bx - ax) + (y - ay) * (by - ay)) / ((bx - ax) ** 2 + (by - ay) ** 2));
  return Math.hypot(x - ax - t * (bx - ax), y - ay - t * (by - ay));
};
const strokes = [
  [4.5, 5.2, 7.3, 8],
  [7.3, 8, 4.5, 10.8],
  [8.8, 10.9, 11.7, 10.9],
];
const bg = [0x74, 0xd4, 0x84];
const fg = [0x07, 0x14, 0x0a];

for (let j = 0; j < N; j++) {
  for (let i = 0; i < N; i++) {
    const x = (i + 0.5) / U;
    const y = (j + 0.5) / U;
    const alpha = clamp(0.5 - roundRect(x, y, 7, 3.2) * U); // 1-unit margin, like OS icon grids
    const ink = clamp(0.5 - (Math.min(...strokes.map((s) => segment(x, y, s))) - 0.85) * U);
    const o = (j * N + i) * 4;
    for (let c = 0; c < 3; c++) px[o + c] = Math.round(bg[c] + (fg[c] - bg[c]) * ink);
    px[o + 3] = Math.round(alpha * 255);
  }
}

const crcTable = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
const crc = (buf) => {
  let c = 0xffffffff;
  for (const b of buf) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
};
const chunk = (type, data) => {
  const body = Buffer.concat([Buffer.from(type), data]);
  const out = Buffer.alloc(body.length + 8);
  out.writeUInt32BE(data.length, 0);
  body.copy(out, 4);
  out.writeUInt32BE(crc(body), out.length - 4);
  return out;
};

const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(N, 0);
ihdr.writeUInt32BE(N, 4);
ihdr.set([8, 6, 0, 0, 0], 8); // 8-bit RGBA
const raw = Buffer.alloc(N * (N * 4 + 1));
for (let j = 0; j < N; j++) px.copy(raw, j * (N * 4 + 1) + 1, j * N * 4, (j + 1) * N * 4);

const out = new URL("../src-tauri/app-icon.png", import.meta.url);
writeFileSync(out, Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), chunk("IHDR", ihdr), chunk("IDAT", deflateSync(raw)), chunk("IEND", Buffer.alloc(0))]));
console.log("wrote", out.pathname);
