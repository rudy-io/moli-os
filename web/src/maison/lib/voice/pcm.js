// Microphone audio as the server wants it: 16 kHz mono, 16-bit WAV.

/** Linear-interpolation resampler that keeps its phase between blocks. */
export class Resampler {
  constructor(from, to) {
    this.step = from / to;
    this.pos = 0;
  }

  /** @param {Float32Array} input @returns {Float32Array} */
  push(input) {
    const out = [];
    while (this.pos < input.length) {
      const i = Math.floor(this.pos);
      const frac = this.pos - i;
      const a = input[i];
      const b = i + 1 < input.length ? input[i + 1] : a;
      out.push(a + (b - a) * frac);
      this.pos += this.step;
    }
    this.pos -= input.length;
    return Float32Array.from(out);
  }
}

/** RMS of a frame. */
export function level(frame) {
  if (!frame.length) return 0;
  let sum = 0;
  for (const s of frame) sum += s * s;
  return Math.sqrt(sum / frame.length);
}

/** Float32 frames → a WAV file (ArrayBuffer). */
export function toWav(frames, rate) {
  const n = frames.reduce((a, f) => a + f.length, 0);
  const buf = new ArrayBuffer(44 + n * 2);
  const v = new DataView(buf);
  const tag = (o, s) => [...s].forEach((c, i) => v.setUint8(o + i, c.charCodeAt(0)));
  tag(0, 'RIFF');
  v.setUint32(4, 36 + n * 2, true);
  tag(8, 'WAVEfmt ');
  v.setUint32(16, 16, true);
  v.setUint16(20, 1, true);
  v.setUint16(22, 1, true);
  v.setUint32(24, rate, true);
  v.setUint32(28, rate * 2, true);
  v.setUint16(32, 2, true);
  v.setUint16(34, 16, true);
  tag(36, 'data');
  v.setUint32(40, n * 2, true);
  let o = 44;
  for (const f of frames)
    for (const s of f) {
      const c = Math.max(-1, Math.min(1, s));
      v.setInt16(o, c < 0 ? c * 32768 : c * 32767, true);
      o += 2;
    }
  return buf;
}
