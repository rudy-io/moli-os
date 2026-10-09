// Moli's voice through one AudioContext, unlocked by a tap (iOS needs it).

let ctx = null;
let analyser = null;
/** What plays now: its sources (one, or the pieces of a stream) and who waits for its end. */
let current = null;

/** Call synchronously inside the tap that starts a conversation: the
 *  context it returns is the one the microphone must use too (iOS). */
export function unlock() {
  if (!ctx) {
    ctx = new AudioContext();
    analyser = Object.assign(ctx.createAnalyser(), { fftSize: 512 });
    analyser.connect(ctx.destination);
  }
  if (ctx.state === 'suspended') ctx.resume();
  const silent = ctx.createBufferSource();
  silent.buffer = ctx.createBuffer(1, 1, 22050);
  silent.connect(ctx.destination);
  silent.start();
  return ctx;
}

/** Plays MP3/WAV bytes; resolves `true` when finished, `false` if stopped. */
export async function play(bytes) {
  if (!ctx) unlock();
  const buffer = await ctx.decodeAudioData(bytes);
  stop();
  return new Promise((resolve) => {
    const source = ctx.createBufferSource();
    source.buffer = buffer;
    source.connect(analyser);
    current = { sources: new Set([source]), resolve };
    source.onended = () => {
      if (current?.sources.has(source)) current = null;
      resolve(true);
    };
    source.start();
  });
}

/** A sentence as the server sends it: streamed PCM is played as it comes,
 *  anything else whole. Resolves `true` when finished, `false` if stopped. */
export async function playSpeech(res) {
  const type = res.headers.get('content-type') ?? '';
  if (type.startsWith('audio/pcm') && res.body) return playStream(res.body.getReader(), rateOf(type));
  return play(await res.arrayBuffer());
}

/** `audio/pcm;rate=24000` → 24000. */
export function rateOf(type) {
  const rate = Number(/rate=(\d+)/.exec(type)?.[1]);
  return rate > 0 ? rate : 24000;
}

/** 16-bit little-endian PCM bytes → samples in -1..1; an odd last byte waits for the next chunk. */
export function samples(bytes, carry) {
  const all = carry?.length ? concat(carry, bytes) : bytes;
  const even = all.length - (all.length % 2);
  const view = new DataView(all.buffer, all.byteOffset, even);
  const out = new Float32Array(even / 2);
  for (let i = 0; i < out.length; i++) out[i] = view.getInt16(i * 2, true) / 32768;
  return { out, carry: all.slice(even) };
}

function concat(a, b) {
  const all = new Uint8Array(a.length + b.length);
  all.set(a);
  all.set(b, a.length);
  return all;
}

// A little head start, so the first pieces play back to back.
const LEAD = 0.06;

async function playStream(reader, rate) {
  if (!ctx) unlock();
  stop();
  let resolve;
  const done = new Promise((r) => (resolve = r));
  const me = { sources: new Set(), resolve, reader };
  current = me;
  let at = 0;
  let carry = null;
  let last = null;
  try {
    for (;;) {
      const { done: ended, value } = await reader.read();
      if (current !== me) return false;
      if (ended) break;
      const piece = samples(value, carry);
      carry = piece.carry;
      if (!piece.out.length) continue;
      const buffer = ctx.createBuffer(1, piece.out.length, rate);
      buffer.getChannelData(0).set(piece.out);
      const source = ctx.createBufferSource();
      source.buffer = buffer;
      source.connect(analyser);
      at = Math.max(at, ctx.currentTime + LEAD);
      source.start(at);
      at += buffer.duration;
      me.sources.add(source);
      source.onended = () => me.sources.delete(source);
      last = source;
    }
  } catch {
    // Cut short (network, server): what came is played.
    if (current !== me) return false;
  }
  if (!last || ctx.currentTime >= at) {
    if (current === me) current = null;
    return true;
  }
  last.onended = () => {
    me.sources.delete(last);
    if (current === me) current = null;
    resolve(true);
  };
  return done;
}

export function stop() {
  const c = current;
  current = null;
  if (!c) return;
  for (const source of c.sources) {
    source.onended = null;
    try {
      source.stop();
    } catch {
      /* already stopped */
    }
  }
  c.reader?.cancel().catch(() => {});
  c.resolve(false);
}

export const playing = () => current !== null;

/** Output level 0..1, for the orb. */
export function outputLevel() {
  if (!analyser || !current) return 0;
  const data = new Uint8Array(analyser.fftSize);
  analyser.getByteTimeDomainData(data);
  let sum = 0;
  for (const v of data) sum += ((v - 128) / 128) ** 2;
  return Math.min(1, Math.sqrt(sum / data.length) * 4);
}
