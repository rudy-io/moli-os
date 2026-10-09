// The microphone as 20 ms frames of 16 kHz audio, with their level.
//
// iOS Safari: the AudioContext must be the one unlocked by the tap (a new
// one created after `await getUserMedia` may stay suspended forever, and
// `resume()` then never settles), and WebKit only renders nodes that reach
// the destination: the tap goes out through a muted gain.

import { Resampler, level } from './pcm.js';

export const RATE = 16000;
const FRAME = RATE / 50;

const WORKLET = `registerProcessor('moli-tap', class extends AudioWorkletProcessor {
  process(inputs) { const ch = inputs[0]?.[0]; if (ch) this.port.postMessage(ch.slice(0)); return true; }
});`;

/** Contexts that already know the tap (it registers once per context). */
const ready = new WeakMap();

function loadTap(ctx) {
  if (!ready.has(ctx)) {
    const url = URL.createObjectURL(new Blob([WORKLET], { type: 'application/javascript' }));
    const loading = ctx.audioWorklet.addModule(url).finally(() => URL.revokeObjectURL(url));
    loading.catch(() => ready.delete(ctx));
    ready.set(ctx, loading);
  }
  return ready.get(ctx);
}

/** Opens the mic on `ctx` (the tap's unlocked context); `onFrame(frame: Float32Array,
 *  rms: number)` 50 times a second. Returns `close()` (the context stays open). */
export async function openMic(ctx, onFrame) {
  if (ctx.state !== 'running') ctx.resume().catch(() => {});
  const stream = await navigator.mediaDevices.getUserMedia({
    audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true, channelCount: 1 },
  });
  try {
    await loadTap(ctx);
    const source = ctx.createMediaStreamSource(stream);
    const tap = new AudioWorkletNode(ctx, 'moli-tap');
    const mute = ctx.createGain();
    mute.gain.value = 0;
    const resampler = new Resampler(ctx.sampleRate, RATE);
    let pending = new Float32Array(0);
    tap.port.onmessage = (e) => {
      const more = resampler.push(e.data);
      const all = new Float32Array(pending.length + more.length);
      all.set(pending);
      all.set(more, pending.length);
      let at = 0;
      for (; at + FRAME <= all.length; at += FRAME) {
        const frame = all.slice(at, at + FRAME);
        onFrame(frame, level(frame));
      }
      pending = all.slice(at);
    };
    source.connect(tap);
    tap.connect(mute);
    mute.connect(ctx.destination);
    return () => {
      tap.port.onmessage = null;
      source.disconnect();
      tap.disconnect();
      mute.disconnect();
      stream.getTracks().forEach((t) => t.stop());
    };
  } catch (err) {
    stream.getTracks().forEach((t) => t.stop());
    throw err;
  }
}
