// A spoken conversation with Moli: listen → hear → think → speak → listen…
// Moli stops talking when interrupted, hangs up after a silence or a goodbye.

import { ask, transcribe, speakBrowser } from '../moli.svelte.js';
import { note } from '../home.svelte.js';
import { i18n, t } from '../../../lib/i18n.svelte.js';
import { openMic, RATE } from './capture.js';
import { toWav } from './pcm.js';
import { Vad } from './vad.js';
import * as player from './player.js';
import { sentences, isGoodbye, mentionsApproval, fetchSpeech } from './speech.js';

const PREROLL = 15; // 300 ms kept before the start of speech
const MAX_FRAMES = 50 * 20; // 20 s
const IDLE_MS = 8000;

export const talk = $state({
  /** 'off' | 'listening' | 'hearing' | 'thinking' | 'speaking' */
  state: 'off',
  /** last thing heard / being said, for the captions */
  heard: '',
  saying: '',
  level: 0,
  interruptible: true,
});

let close = null;
let vad = null;
let preroll = [];
let utterance = [];
let idle = null;
let misses = 0;
let tick = 0;
let surface = 'bubble';
let speakAbort = null;

const hush = () => window.speechSynthesis?.cancel();

/** Must run inside the tap (unlocks audio on iOS). */
export async function startTalk(where = 'bubble') {
  if (talk.state !== 'off') return;
  surface = where;
  const ctx = player.unlock();
  vad = new Vad();
  misses = 0;
  preroll = [];
  talk.heard = talk.saying = '';
  talk.state = 'listening';
  try {
    close = await openMic(ctx, onFrame);
  } catch {
    talk.state = 'off';
    note(t('moli.voix.micro_indisponible'), 'error');
    return;
  }
  if (talk.state === 'off') return close?.(); // hung up while the mic opened
  listen();
  document.addEventListener('visibilitychange', onHidden);
}

export function stopTalk() {
  if (talk.state === 'off') return;
  clearTimeout(idle);
  speakAbort?.abort();
  player.stop();
  hush();
  close?.();
  close = null;
  utterance = [];
  talk.state = 'off';
  talk.level = 0;
  document.removeEventListener('visibilitychange', onHidden);
}

function onHidden() {
  if (document.hidden) stopTalk();
}

function listen() {
  talk.state = 'listening';
  talk.saying = '';
  vad.strict = false;
  vad.reset();
  clearTimeout(idle);
  idle = setTimeout(stopTalk, IDLE_MS);
}

function onFrame(frame, rms) {
  if (talk.state === 'off') return;
  if (++tick % 3 === 0) talk.level = talk.state === 'speaking' ? player.outputLevel() : Math.min(1, rms * 8);
  preroll.push(frame);
  if (preroll.length > PREROLL) preroll.shift();
  if (talk.state === 'thinking') return;
  const event = vad.push(rms);
  if (talk.state === 'speaking') {
    if (event === 'start' && talk.interruptible) {
      speakAbort?.abort();
      player.stop();
      hush();
      hearFromPreroll();
    }
    return;
  }
  if (talk.state === 'listening' && event === 'start') return hearFromPreroll();
  if (talk.state === 'hearing') {
    utterance.push(frame);
    if (event === 'end' || utterance.length >= MAX_FRAMES) finish();
  }
}

function hearFromPreroll() {
  clearTimeout(idle);
  talk.state = 'hearing';
  vad.strict = false;
  utterance = [...preroll];
}

async function finish() {
  talk.state = 'thinking';
  const wav = toWav(utterance, RATE);
  utterance = [];
  const text = await transcribe(wav);
  if (talk.state !== 'thinking') return; // hung up meanwhile
  if (!text) return ++misses >= 2 ? stopTalk() : listen();
  misses = 0;
  talk.heard = text;
  if (isGoodbye(text, i18n.language)) return stopTalk();
  const answer = await ask(text, surface, true, { silent: true });
  if (talk.state !== 'thinking') return;
  if (!answer) return stopTalk();
  let reply = answer.reply;
  if (answer.actions.some((a) => a.status === 'held') && !mentionsApproval(reply, i18n.language)) reply += ` ${t('moli.voix.valider_ecran')}`;
  // Asked to stop, in any words: the last answer (if any), then hang up.
  if (answer.end) {
    if (reply) await say(reply);
    return stopTalk();
  }
  await say(reply);
  if (talk.state === 'speaking') listen();
}

async function say(text) {
  talk.state = 'speaking';
  vad.strict = true;
  vad.reset();
  speakAbort = new AbortController();
  const signal = speakAbort.signal;
  const parts = sentences(text);
  const audio = parts.map((s) => fetchSpeech(s, signal).catch(() => null));
  for (let i = 0; i < parts.length; i++) {
    if (talk.state !== 'speaking') return;
    talk.saying = parts[i];
    const res = await audio[i];
    if (talk.state !== 'speaking') return;
    if (!res) return browserSays(parts.slice(i).join(' '));
    // The cloud voice plays while it is made (the first words a second sooner).
    if (!(await player.playSpeech(res).catch(() => false)) && talk.state === 'speaking') return browserSays(parts.slice(i).join(' '));
  }
}

function browserSays(text) {
  return new Promise((resolve) => {
    speakBrowser(text);
    const wait = () => (talk.state === 'speaking' && window.speechSynthesis?.speaking ? setTimeout(wait, 200) : resolve());
    setTimeout(wait, 300);
  });
}
