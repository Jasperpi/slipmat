// Waveforms. The engine sends four bytes per 256 frames of song (lows, mids,
// highs, peak). The whole song is drawn once into tiles; each frame just
// copies the visible part, so scrolling costs almost nothing.

export const SRC = 44100;
export const HOP = 256;
const TILE = 2048;
export const LOW = '#d9412b';
export const MID = '#ff9f43';
export const HIGH = '#fff1dc';
// Bass is drawn outermost, then mids, then highs innermost.
const SHAPE = [1.0, 0.72, 0.46];

export class WaveData {
  constructor(key) {
    this.key = key;
    this.bytes = new Uint8Array(0);
    this.cols = 0;
    this.done = false;
    this.tiles = [];
    this.tileH = 0;
    this.drawn = 0;
    this.version = 0;
  }

  append(chunk) {
    if (!chunk.length) return;
    const next = new Uint8Array(this.bytes.length + chunk.length);
    next.set(this.bytes);
    next.set(chunk, this.bytes.length);
    this.bytes = next;
    this.cols = this.bytes.length >> 2;
    this.version++;
  }

  // Each song is scaled to its own loud parts, so quiet masters still read.
  norm() {
    if (this.normAt === this.cols && this.normValue) return this.normValue;
    const b = this.bytes;
    const step = Math.max(1, Math.floor(this.cols / 4000));
    const vals = [];
    for (let i = 0; i < this.cols; i += step) vals.push(Math.max(b[i * 4], b[i * 4 + 1], b[i * 4 + 2]));
    vals.sort((x, y) => x - y);
    const p = vals[Math.floor(vals.length * 0.97)] || 255;
    this.normAt = this.cols;
    this.normValue = Math.min(4, 255 / Math.max(p, 24));
    return this.normValue;
  }

  // Make sure tiles exist (at height h) for every column received.
  paint(h) {
    // Repaint everything once the whole song is in (final scaling).
    const key = `${h}:${this.done}`;
    if (key !== this.paintKey) {
      this.paintKey = key;
      this.tiles = [];
      this.tileH = h;
      this.drawn = 0;
      this.normAt = -1;
    }
    const b = this.bytes;
    const mid = h / 2;
    const scale = (h / 2 - 1) / 255 * this.norm();
    while (this.drawn < this.cols) {
      const t = Math.floor(this.drawn / TILE);
      if (!this.tiles[t]) {
        const c = document.createElement('canvas');
        c.width = TILE;
        c.height = h;
        this.tiles[t] = c;
      }
      const ctx = this.tiles[t].getContext('2d');
      const end = Math.min(this.cols, (t + 1) * TILE);
      for (const [band, color] of [[0, LOW], [1, MID], [2, HIGH]]) {
        ctx.fillStyle = color;
        for (let i = this.drawn; i < end; i++) {
          const a = Math.min(mid - 1, b[i * 4 + band] * scale * SHAPE[band]);
          if (a > 0.5) ctx.fillRect(i - t * TILE, mid - a, 1, a * 2);
        }
      }
      this.drawn = end;
    }
  }

  // Draw columns [c0, c0 + span) into the destination rect.
  blit(ctx, c0, span, dx, dy, dw, dh) {
    const per = dw / span;
    for (let t = Math.max(0, Math.floor(c0 / TILE)); t * TILE < c0 + span && t < this.tiles.length; t++) {
      const tile = this.tiles[t];
      if (!tile) continue;
      const s0 = Math.max(c0, t * TILE);
      const s1 = Math.min(c0 + span, (t + 1) * TILE, this.drawn);
      if (s1 <= s0) continue;
      ctx.drawImage(tile, s0 - t * TILE, 0, s1 - s0, tile.height, dx + (s0 - c0) * per, dy, (s1 - s0) * per, dh);
    }
  }

  // A small whole-song picture for the overview strip.
  overview(width, height) {
    const c = document.createElement('canvas');
    c.width = Math.max(1, width);
    c.height = Math.max(1, height);
    const ctx = c.getContext('2d');
    if (!this.cols) return c;
    const b = this.bytes;
    const perPx = this.expected / width;
    const mid = height / 2;
    const scale = (height / 2 - 1) / 255 * this.norm();
    const peaks = [new Float32Array(width), new Float32Array(width), new Float32Array(width)];
    for (let x = 0; x < width; x++) {
      const a = Math.floor(x * perPx), z = Math.min(this.cols, Math.floor((x + 1) * perPx) + 1);
      for (let i = a; i < z; i++) {
        for (let band = 0; band < 3; band++) {
          const v = b[i * 4 + band];
          if (v > peaks[band][x]) peaks[band][x] = v;
        }
      }
    }
    for (const [band, color] of [[0, LOW], [1, MID], [2, HIGH]]) {
      ctx.fillStyle = color;
      for (let x = 0; x < width; x++) {
        const a = Math.min(mid - 0.5, peaks[band][x] * scale * SHAPE[band]);
        if (a > 0.3) ctx.fillRect(x, mid - a, 1, a * 2);
      }
    }
    return c;
  }
}

export const CUE_COLORS = ['#e5484d', '#f76b15', '#ffc53d', '#46a758', '#12a594', '#0090ff', '#8e4ec6', '#d6409f'];

// Stem colours, matching the stem buttons: vocals, melody, bass, drums.
export const STEM_COLORS = ['#f472b6', '#facc15', '#60a5fa', '#fb923c'];
// Drawn from the middle out: bass, melody, vocals, drums.
const STEM_ORDER = [2, 1, 0, 3];

// The stem waveform: four layers stacked from the centre line outward, one
// canvas set per stem, so a muted stem can be drawn faded without repainting.
export class StemWave {
  constructor(key) {
    this.key = key;
    this.bytes = new Uint8Array(0);
    this.cols = 0;
    this.done = false;
    this.layers = [[], [], [], []];
    this.drawn = 0;
    this.tileH = 0;
    this.normAt = -1;
  }

  append(chunk) {
    if (!chunk.length) return;
    const next = new Uint8Array(this.bytes.length + chunk.length);
    next.set(this.bytes);
    next.set(chunk, this.bytes.length);
    this.bytes = next;
    this.cols = this.bytes.length >> 2;
  }

  norm() {
    if (this.normAt === this.cols && this.normValue) return this.normValue;
    const b = this.bytes;
    const step = Math.max(1, Math.floor(this.cols / 4000));
    const vals = [];
    for (let i = 0; i < this.cols; i += step) vals.push(b[i * 4] + b[i * 4 + 1] + b[i * 4 + 2] + b[i * 4 + 3]);
    vals.sort((x, y) => x - y);
    const p = vals[Math.floor(vals.length * 0.97)] || 255;
    this.normAt = this.cols;
    this.normValue = 255 / Math.max(p, 40);
    return this.normValue;
  }

  paint(h) {
    const key = `${h}:${this.done}`;
    if (key !== this.paintKey) {
      this.paintKey = key;
      this.layers = [[], [], [], []];
      this.drawn = 0;
      this.tileH = h;
      this.normAt = -1;
    }
    const b = this.bytes;
    const mid = h / 2;
    const scale = (h / 2 - 1) / 255 * this.norm();
    while (this.drawn < this.cols) {
      const t = Math.floor(this.drawn / TILE);
      const end = Math.min(this.cols, (t + 1) * TILE);
      for (let s = 0; s < 4; s++) {
        if (!this.layers[s][t]) {
          const c = document.createElement('canvas');
          c.width = TILE;
          c.height = h;
          this.layers[s][t] = c;
        }
      }
      const ctxs = this.layers.map((l) => l[t].getContext('2d'));
      ctxs.forEach((ctx, s) => { ctx.fillStyle = STEM_COLORS[s]; });
      for (let i = this.drawn; i < end; i++) {
        let inner = 0;
        for (const s of STEM_ORDER) {
          const outer = Math.min(mid - 1, inner + b[i * 4 + s] * scale);
          if (outer - inner > 0.3) {
            const x = i - t * TILE;
            ctxs[s].fillRect(x, mid - outer, 1, outer - inner);
            ctxs[s].fillRect(x, mid + inner, 1, outer - inner);
          }
          inner = outer;
        }
      }
      this.drawn = end;
    }
  }

  // Draw columns [c0, c0 + span); `alpha[s]` fades each stem (muted ones).
  blit(ctx, c0, span, dx, dy, dw, dh, alpha) {
    const per = dw / span;
    for (let s = 0; s < 4; s++) {
      ctx.globalAlpha = alpha[s];
      const tiles = this.layers[s];
      for (let t = Math.max(0, Math.floor(c0 / TILE)); t * TILE < c0 + span && t < tiles.length; t++) {
        const tile = tiles[t];
        if (!tile) continue;
        const s0 = Math.max(c0, t * TILE);
        const s1 = Math.min(c0 + span, (t + 1) * TILE, this.drawn);
        if (s1 <= s0) continue;
        ctx.drawImage(tile, s0 - t * TILE, 0, s1 - s0, tile.height, dx + (s0 - c0) * per, dy, (s1 - s0) * per, dh);
      }
    }
    ctx.globalAlpha = 1;
  }
}
