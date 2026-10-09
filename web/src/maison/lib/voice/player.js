// Moli's voice through one AudioContext, unlocked by a tap (iOS needs it).

let ctx = null;
let analyser = null;
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
    current = { source, resolve };
    source.onended = () => {
      if (current?.source === source) current = null;
      resolve(true);
    };
    source.start();
  });
}

export function stop() {
  const c = current;
  current = null;
  if (!c) return;
  c.source.onended = null;
  try {
    c.source.stop();
  } catch {
    /* already stopped */
  }
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
