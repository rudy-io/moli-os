// Is someone speaking? Energy against an adaptive noise floor, per 20 ms frame.

export class Vad {
  // endMs: 700 ms of silence ends a sentence (a pause inside one is shorter).
  constructor({ factor = 3, minLevel = 0.012, startMs = 150, endMs = 700, frameMs = 20 } = {}) {
    Object.assign(this, { factor, minLevel, frameMs });
    this.startFrames = Math.ceil(startMs / frameMs);
    this.strictFrames = Math.ceil(300 / frameMs);
    this.endFrames = Math.ceil(endMs / frameMs);
    /** While Moli speaks: louder (×2) and longer (300 ms) speech, floor frozen (it would learn the echo). */
    this.strict = false;
    this.floor = 0.004;
    this.reset();
  }

  reset() {
    this.speaking = false;
    this.loud = 0;
    this.quiet = 0;
  }

  get threshold() {
    const base = Math.max(this.floor * this.factor, this.minLevel);
    return this.strict ? base * 2 : base;
  }

  /** @returns {'start' | 'end' | null} */
  push(rms) {
    const loud = rms > this.threshold;
    if (!this.speaking) {
      if (!loud && !this.strict) this.floor = this.floor * 0.97 + rms * 0.03;
      this.loud = loud ? this.loud + 1 : 0;
      if (this.loud >= (this.strict ? this.strictFrames : this.startFrames)) {
        this.speaking = true;
        this.quiet = 0;
        return 'start';
      }
      return null;
    }
    this.quiet = loud ? 0 : this.quiet + 1;
    if (this.quiet >= this.endFrames) {
      this.speaking = false;
      this.loud = 0;
      return 'end';
    }
    return null;
  }
}
