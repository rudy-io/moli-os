<script>
  import { moli, ask, canRecord, voiceUnavailable, startRecording, stopRecording } from '../lib/moli.svelte.js';
  import { note } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from '../ui/Icon.svelte';
  import { talk, startTalk, stopTalk } from '../lib/voice/conversation.svelte.js';

  const CAPTION = { listening: 'moli.composer.ecoute', hearing: 'moli.composer.ecoute', thinking: 'moli.composer.reflexion' };

  /** Where a question is typed, or spoken. */
  let { surface = 'bubble', big = false, autofocus = false, placeholder = null } = $props();

  let text = $state('');
  let field;

  const voiceOk = $derived(moli.status?.listen && canRecord());
  // Never a dead button: on a phone there is no tooltip, so a tap says why.
  const voiceHint = $derived(moli.status?.listen ? voiceUnavailable() : '');

  function talkNow() {
    if (!voiceOk) return note(voiceHint || t('moli.voix.pas_prete'), 'error');
    startTalk(surface);
  }
  $effect(() => {
    if (autofocus) field?.focus();
  });

  function send(e) {
    e?.preventDefault();
    if (!text.trim() || moli.busy) return;
    const q = text;
    text = '';
    ask(q, surface);
  }

  function mic() {
    if (!voiceOk) return note(voiceHint || t('moli.voix.pas_prete'), 'error');
    if (moli.voice === 'recording') stopRecording();
    else startRecording(surface);
  }
</script>

{#if talk.state !== 'off'}
  <div class="composer talking" class:big role="status" aria-live="polite">
    <span class="caption">{talk.state === 'speaking' ? talk.saying : t(CAPTION[talk.state])}</span>
    <button type="button" class="hangup" onclick={stopTalk} aria-label={t('moli.composer.raccrocher')} title={t('moli.composer.raccrocher')}>
      <Icon name="hangup" size={big ? 24 : 20} />
    </button>
  </div>
{:else}
<form class="composer" class:big onsubmit={send}>
  <input
    bind:this={field}
    bind:value={text}
    placeholder={placeholder ?? t('moli.composer.placeholder')}
    aria-label={t('moli.composer.question')}
    autocomplete="off"
    enterkeyhint="send"
    disabled={!moli.status?.ready} />
  {#if moli.status?.listen && moli.status?.speak}
    <button
      type="button"
      class="talk"
      class:off={!voiceOk}
      onclick={talkNow}
      disabled={moli.busy || moli.voice !== 'idle'}
      title={voiceHint || t('moli.composer.parler_avec')}
      aria-label={t('moli.composer.parler_avec')}>
      <Icon name="talk" size={big ? 24 : 20} />
    </button>
  {/if}
  {#if moli.status?.listen}
    <button
      type="button"
      class="mic"
      class:rec={moli.voice === 'recording'}
      class:off={!voiceOk}
      onclick={mic}
      disabled={moli.busy || moli.voice === 'transcribing'}
      title={voiceHint || (moli.voice === 'recording' ? t('moli.composer.arreter') : t('moli.composer.dicter'))}
      aria-label={moli.voice === 'recording' ? t('moli.composer.arreter_ecoute') : t('moli.composer.parler_a')}>
      <Icon name="mic" size={big ? 24 : 20} />
    </button>
  {/if}
  <button type="submit" class="send" disabled={!text.trim() || moli.busy} aria-label={t('moli.composer.envoyer')}>
    <Icon name="send" size={big ? 22 : 18} />
  </button>
</form>
{/if}

<style>
  .composer {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 6px 6px 18px;
    border-radius: 999px;
    background: var(--surface);
    box-shadow: var(--shadow), inset 0 0 0 1px var(--line);
  }

  .composer:focus-within {
    box-shadow: var(--shadow-lift), inset 0 0 0 2px color-mix(in srgb, var(--cool) 45%, transparent);
  }

  input {
    flex: 1;
    min-width: 0;
    border: 0;
    background: none;
    font: inherit;
    font-size: 16px;
    color: var(--ink);
    padding: 10px 0;
    outline: none;
  }

  input:focus-visible {
    outline: none;
  }

  .big {
    padding: 8px 8px 8px 26px;
  }

  .big input {
    font-size: 20px;
    padding: 14px 0;
  }

  button {
    flex: none;
    width: 44px;
    height: 44px;
    border-radius: 50%;
    border: 0;
    display: grid;
    place-items: center;
    transition: transform 0.15s var(--ease), background 0.2s;
  }

  .big button {
    width: 56px;
    height: 56px;
  }

  .send {
    background: var(--ink);
    color: var(--bg);
  }

  .send:disabled {
    opacity: 0.25;
  }

  .mic {
    background: var(--surface-2);
    color: var(--ink-2);
  }

  .mic.rec {
    background: var(--alert);
    color: #fff;
    animation: pulse 1.2s ease-in-out infinite;
  }

  .talk {
    background: color-mix(in srgb, var(--cool) 18%, var(--surface-2));
    color: var(--ink);
  }

  .talk:disabled,
  .off {
    opacity: 0.4;
  }

  .talking {
    padding-left: 22px;
  }

  .caption {
    flex: 1;
    min-width: 0;
    /* A long sentence must not widen the bubble (its min-content is the whole line). */
    contain: inline-size;
    font-weight: 600;
    color: var(--ink-2);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .hangup {
    background: var(--alert);
    color: #fff;
  }

  .mic:disabled {
    opacity: 0.4;
  }

  @keyframes pulse {
    50% {
      transform: scale(1.08);
      box-shadow: 0 0 0 8px color-mix(in srgb, var(--alert) 25%, transparent);
    }
  }
</style>
