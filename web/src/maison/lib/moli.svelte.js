// Moli's conversation, shared by the bubble (any page) and Moli's own page:
// a question asked in the bubble goes on in the big view.

import { hub, decide } from '../../lib/hub.svelte.js';
import { i18n, locale, t } from '../../lib/i18n.svelte.js';
import { home, note, value, isOn, reloadLayout } from './home.svelte.js';
import { openMic, RATE } from './voice/capture.js';
import { toWav } from './voice/pcm.js';
import { playSpeech, unlock } from './voice/player.js';
import { sentences, fetchSpeech } from './voice/speech.js';

export const moli = $state({
  /** @type {{ role: 'user' | 'assistant', content: string, cards?: any[], actions?: any[], spoken?: boolean, error?: boolean }[]} */
  messages: [],
  busy: false,
  /** { ready, listen, model } once known */
  status: null,
  /** 'idle' | 'recording' | 'transcribing' */
  voice: 'idle',
  /** index of the answer whose cards the big view shows */
  focus: null,
  /** local outcome of approvals decided from the conversation: request id → 'approved' | 'denied' */
  decided: {},
});

let statusLoaded = false;

export async function loadStatus() {
  if (statusLoaded) return;
  statusLoaded = true;
  try {
    const res = await fetch('/api/assistant');
    moli.status = res.ok ? await res.json() : { ready: false };
  } catch {
    moli.status = { ready: false };
  }
}

/** The Moli app can hand its microphone to the page only from the build
 *  that declares it (`MoliNative.mic`); before, iOS would refuse or close it. */
const nativeWithoutMic = () => typeof window !== 'undefined' && !!window.MoliNative && !window.MoliNative.mic;

/** The recorder needs a secure page (HTTPS or localhost), and a browser or app that lends the mic. */
export const canRecord = () =>
  typeof window !== 'undefined' &&
  window.isSecureContext &&
  !nativeWithoutMic() &&
  !!navigator.mediaDevices?.getUserMedia &&
  typeof AudioWorkletNode !== 'undefined';

/** Why the voice is unavailable here, in words ('' when it works). */
export function voiceUnavailable() {
  if (canRecord()) return '';
  if (nativeWithoutMic()) return t('moli.voix.version_native');
  if (typeof window !== 'undefined' && !window.isSecureContext) return t('moli.voix.acces_securise');
  return t('moli.voix.navigateur');
}

export function reset() {
  moli.messages = [];
  moli.focus = null;
}

/** The turn asked from this screen and not yet answered: « stop » names it,
 *  so it never stops somebody else's. */
let current = null;

/** Asks Moli. `surface`: 'bubble' or 'page'. `spoken`: the answer will be
 *  heard (read aloud here unless `silent`: the conversation speaks it).
 *  Resolves to `{ reply, actions }`, or null on failure. */
export async function ask(text, surface = 'bubble', spoken = false, { silent = false } = {}) {
  const content = text.trim();
  if (!content || moli.busy) return null;
  moli.messages.push({ role: 'user', content, spoken });
  moli.busy = true;
  // Not crypto.randomUUID: it only exists over HTTPS, and a house is often plain HTTP.
  current = Date.now().toString(36) + Math.random().toString(36).slice(2, 10);
  try {
    const res = await fetch('/api/assistant', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        id: current,
        surface,
        spoken,
        messages: moli.messages.filter((m) => !m.error).map(({ role, content }) => ({ role, content })),
      }),
    });
    const body = await res.json().catch(() => ({}));
    if (!res.ok) throw new Error(humanError(res.status, body.error));
    // Stopped: nothing to say. The question left unanswered goes, no empty bubble.
    if (body.interrupted) {
      moli.messages.pop();
      return null;
    }
    moli.messages.push({
      role: 'assistant',
      content: body.reply,
      cards: body.cards ?? [],
      actions: body.actions ?? [],
      proposals: (body.proposals ?? []).map((p) => ({ ...p, state: 'pending' })),
    });
    moli.focus = moli.messages.length - 1;
    if (spoken && !silent) speak(body.reply);
    return { reply: body.reply, actions: body.actions ?? [] };
  } catch (err) {
    moli.messages.push({ role: 'assistant', content: err.message, error: true });
    return null;
  } finally {
    moli.busy = false;
    current = null;
  }
}

/** Stops the turn asked from this screen. True if one was stopped. */
export async function arreter() {
  if (!current) return false;
  try {
    const res = await fetch('/api/assistant/stop', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ id: current }),
    });
    const body = await res.json().catch(() => ({}));
    return body.stopped === true;
  } catch {
    return false;
  }
}

function humanError(status, message) {
  // Out of service for a reason the server words (a contract it does not speak): say it.
  if (status === 503) return moli.status?.refused ?? t('moli.erreur.cle');
  if (status === 429) return t('moli.erreur.patience');
  if (status === 502) return t('moli.erreur.cerveau');
  return message ?? t('moli.erreur.autre');
}

// ---- voice ---------------------------------------------------------------------

let closeMic = null;
let frames = [];
let stopTimer = null;
let recordingSurface = 'bubble';

/** One spoken question (hold-to-talk), recorded as WAV like the conversation. */
export async function startRecording(surface) {
  if (!canRecord() || moli.voice !== 'idle') return;
  const ctx = unlock();
  recordingSurface = surface;
  try {
    frames = [];
    closeMic = await openMic(ctx, (f) => frames.push(f));
    moli.voice = 'recording';
    // A question, not a speech: stop by itself.
    stopTimer = setTimeout(stopRecording, 20_000);
  } catch {
    moli.voice = 'idle';
    note(t('moli.voix.micro_indisponible'), 'error');
  }
}

export async function stopRecording() {
  if (moli.voice !== 'recording') return;
  clearTimeout(stopTimer);
  closeMic?.();
  closeMic = null;
  moli.voice = 'transcribing';
  const text = await transcribe(toWav(frames, RATE));
  frames = [];
  moli.voice = 'idle';
  if (text) await ask(text, recordingSurface, true);
  else note(t('moli.voix.rien_entendu'));
}

/** WAV → text, or '' (nothing heard, or a failure already said). */
export async function transcribe(wav) {
  try {
    const res = await fetch('/api/assistant/listen', { method: 'POST', headers: { 'content-type': 'audio/wav' }, body: wav });
    const body = await res.json().catch(() => ({}));
    if (!res.ok) throw new Error(body.error ?? res.statusText);
    return body.text ?? '';
  } catch {
    note(t('moli.voix.pas_compris'), 'error');
    return '';
  }
}

/** Reads an answer aloud: Moli's own voice, the browser's as a last resort. */
export async function speak(text) {
  if (!text) return;
  try {
    for (const s of sentences(text)) if (!(await playSpeech(await fetchSpeech(s)))) return;
  } catch {
    speakBrowser(text);
  }
}

export function speakBrowser(text) {
  try {
    if (!('speechSynthesis' in window) || !text) return;
    const u = new SpeechSynthesisUtterance(text);
    u.lang = locale();
    const voice = speechSynthesis.getVoices().find((v) => v.lang?.startsWith(i18n.language));
    if (voice) u.voice = voice;
    speechSynthesis.cancel();
    speechSynthesis.speak(u);
  } catch {
    /* silent is fine */
  }
}

// ---- held orders ---------------------------------------------------------------

/** Where an order Moli gave stands now. */
export function actionState(a) {
  if (a.status !== 'held') return a.status;
  if (moli.decided[a.request]) return moli.decided[a.request];
  return hub.approvals.some((r) => r.id === a.request) ? 'held' : 'settled';
}

/** Approves (or not) an order the guard held: straight away in a human
 *  session, else through « c'est moi » (the PIN sheet). */
export async function settle(a, approve, label) {
  if (!approve) {
    await decide({ id: a.request, point: a.point }, false);
    moli.decided[a.request] = 'denied';
    return;
  }
  if (hub.session?.human) {
    await decide({ id: a.request, point: a.point }, true);
    moli.decided[a.request] = 'approved';
    return;
  }
  home.held = { id: a.request, point: a.point, reason: a.reason, label, onApproved: () => (moli.decided[a.request] = 'approved') };
}

// ---- what Moli proposes ----------------------------------------------------------

/** A person confirms or cancels a change Moli proposed (rename, move, hide…).
 *  Nothing changes until then. Without a human session the house asks for its
 *  code first, as for an order it held. */
export async function settleProposal(p, approve) {
  const send = async () => {
    if (p.state === 'busy') return; // a second click while the first is on its way
    p.state = 'busy';
    try {
      const res = await fetch(`/api/assistant/proposals/${p.id}`, {
        method: 'POST',
        headers: { 'content-type': 'application/json', 'x-moli-origin': 'ui' },
        body: JSON.stringify({ approve }),
      });
      const body = await res.json().catch(() => ({}));
      if (res.status === 410) {
        p.state = 'gone';
        return;
      }
      if (res.status === 403) {
        // The session lapsed: the house asks for its code again, the proposal still waits.
        p.state = 'pending';
        home.held = { label: p.text, reason: 'configuration', custom: send, onCancel: () => {} };
        return;
      }
      if (!res.ok) {
        p.state = 'error';
        p.error = body.error ?? res.statusText;
        return;
      }
      p.state = approve ? 'done' : 'cancelled';
      p.said = body.said;
      if (approve) await reloadLayout();
    } catch (err) {
      p.state = 'error';
      p.error = err.message;
    }
  };
  if (!approve && !hub.session?.human) {
    p.state = 'cancelled'; // nobody confirms it: it lapses by itself
    return;
  }
  if (hub.session?.human) {
    await send();
    return;
  }
  home.held = { label: p.text, reason: 'configuration', custom: send, onCancel: () => {} };
}

// ---- what to ask -----------------------------------------------------------------

/** A few questions that make sense now, from the hour and the house. */
export function suggestions(max = 5) {
  const h = new Date(home.now).getHours();
  const cfg = home.config ?? {};
  const out = [];
  const lit = (cfg.favorites?.lights ?? []).filter((l) => [l.id, ...(l.also ?? [])].some((i) => isOn(i))).length;
  if (value(cfg.outdoor?.pool, 'action_required') === true) out.push(t('moli.idee.piscine'));
  if (h >= 18 && h < 23) out.push(t('moli.idee.cinema'));
  if (h >= 21 || h < 5) out.push(lit ? t('moli.idee.eteindre_bas') : t('moli.idee.tout_ferme'));
  if (h >= 5 && h < 11) out.push(t('moli.idee.meteo'));
  out.push(t('moli.idee.conso'));
  if (h >= 17) out.push(t('moli.idee.automatisme'));
  if (lit > 2) out.push(t('moli.idee.lumieres'));
  out.push(t('moli.idee.passage'));
  out.push(t('moli.idee.cameras'));
  out.push(t('moli.idee.salon'));
  if (h >= 11 && h < 18) out.push(t('moli.idee.conso_mois'));
  return [...new Set(out)].slice(0, max);
}
