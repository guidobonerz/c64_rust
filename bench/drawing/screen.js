// Reads the text screen out of a mimic snapshot (384x272 PNG, C64 upper
// case character set): node screen.js <chargen.bin> <snapshot.png>...
// Each 8x8 cell is matched against the character ROM.
const fs = require('fs');
const zlib = require('zlib');

function decodePng(file) {
  const buf = fs.readFileSync(file);
  let pos = 8, width = 0, height = 0, bpp = 0;
  const idat = [];
  while (pos < buf.length) {
    const len = buf.readUInt32BE(pos);
    const type = buf.toString('latin1', pos + 4, pos + 8);
    const data = buf.subarray(pos + 8, pos + 8 + len);
    if (type === 'IHDR') {
      width = data.readUInt32BE(0);
      height = data.readUInt32BE(4);
      if (data[8] !== 8) throw new Error('bit depth ' + data[8]);
      bpp = { 0: 1, 2: 3, 4: 2, 6: 4 }[data[9]];
      if (!bpp) throw new Error('colour type ' + data[9]);
    } else if (type === 'IDAT') idat.push(data);
    pos += 12 + len;
  }
  const raw = zlib.inflateSync(Buffer.concat(idat));
  const stride = width * bpp;
  const px = Buffer.alloc(stride * height);
  for (let y = 0; y < height; y++) {
    const f = raw[y * (stride + 1)];
    for (let x = 0; x < stride; x++) {
      const v = raw[y * (stride + 1) + 1 + x];
      const a = x >= bpp ? px[y * stride + x - bpp] : 0;
      const b = y > 0 ? px[(y - 1) * stride + x] : 0;
      const c = x >= bpp && y > 0 ? px[(y - 1) * stride + x - bpp] : 0;
      let p = 0;
      if (f === 1) p = a;
      else if (f === 2) p = b;
      else if (f === 3) p = (a + b) >> 1;
      else if (f === 4) {
        const e = a + b - c;
        const pa = Math.abs(e - a), pb = Math.abs(e - b), pc = Math.abs(e - c);
        p = pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
      }
      px[y * stride + x] = (v + p) & 255;
    }
  }
  return { width, height, bpp, px, stride };
}

const [chargenFile, ...files] = process.argv.slice(2);
const chargen = fs.readFileSync(chargenFile);
const glyphs = new Map();
// 128-255 are the reversed characters (the cursor): read as the plain ones.
for (let c = 0; c < 256; c++) {
  const key = chargen.subarray(c * 8, c * 8 + 8).toString('hex');
  if (!glyphs.has(key)) glyphs.set(key, c & 127);
}
const ascii = c => (c === 32 ? ' ' : c < 32 ? String.fromCharCode(c + 64) : c < 64 ? String.fromCharCode(c) : '?');

for (const file of files) {
  const img = decodePng(file);
  const at = (x, y) => {
    const i = y * img.stride + x * img.bpp;
    return img.px[i] << 16 | img.px[i + 1] << 8 | img.px[i + 2];
  };
  const read = (ox, oy) => {
    const bg = at(ox + 319, oy + 199); // the last cell is empty
    const lines = [];
    let unknown = 0, text = 0;
    for (let row = 0; row < 25; row++) {
      let line = '';
      for (let col = 0; col < 40; col++) {
        const bytes = [];
        for (let y = 0; y < 8; y++) {
          let b = 0;
          for (let x = 0; x < 8; x++) b = (b << 1) | (at(ox + col * 8 + x, oy + row * 8 + y) !== bg ? 1 : 0);
          bytes.push(b);
        }
        const c = glyphs.get(Buffer.from(bytes).toString('hex'));
        if (c === undefined) unknown++;
        else if (c !== 32 && c < 64) text++;
        line += c === undefined ? '#' : ascii(c);
      }
      lines.push(line.trimEnd());
    }
    return { lines, unknown, score: text - 1000 * unknown };
  };
  // The 320x200 text window is somewhere inside the border: take the
  // position where every cell is a character and most of them are text.
  let best = null;
  for (let oy = 0; oy <= img.height - 200; oy++) {
    for (let ox = 0; ox <= img.width - 320; ox++) {
      const r = read(ox, oy);
      if (!best || r.score > best.score) best = r;
    }
  }
  console.log('== ' + file + (best.unknown ? ` (${best.unknown} cells not recognised)` : ''));
  for (const line of best.lines) if (line) console.log(line);
}
