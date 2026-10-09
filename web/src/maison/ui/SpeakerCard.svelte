<script>
  import { value, act, num, nameOf } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** A speaker (Sonos…): what plays, play/pause, volume, mute. */
  let { id, name = null } = $props();

  const playing = $derived(value(id, 'playing') === true);
  const title = $derived(value(id, 'title'));
  const artist = $derived(value(id, 'artist'));
  const volume = $derived(value(id, 'volume'));
  const muted = $derived(value(id, 'mute') === true);
  let dragging = $state(null);
</script>

<section class="card sound">
  <div class="card-head">
    <h2><Icon name="speaker" size={18} />{nameOf(id, name)}</h2>
    <span class="chip {playing ? 'warm' : ''}">{playing ? t('salon.enceinte.lecture') : value(id, 'state') === 'paused_playback' ? t('salon.enceinte.en_pause') : t('salon.enceinte.arret')}</span>
  </div>
  {#if title}
    <p class="track"><b>{title}</b>{#if artist}<span class="muted"> · {artist}</span>{/if}</p>
  {:else}
    <p class="muted">{playing ? t('salon.enceinte.son_tele') : t('salon.enceinte.rien')}</p>
  {/if}
  <div class="row">
    <button class="round" onclick={() => act(`${id}/playing`, !playing, t('salon.lecture'))} aria-label={playing ? t('salon.pause') : t('salon.lecture')}>
      <Icon name={playing ? 'pause' : 'play'} size={26} />
    </button>
    <div class="vol">
      <Icon name="volume-minus" size={18} />
      <input
        type="range"
        min="0"
        max="100"
        value={dragging ?? volume ?? 0}
        oninput={(e) => (dragging = Number(e.currentTarget.value))}
        onchange={(e) => {
          const v = Number(e.currentTarget.value);
          dragging = null;
          act(`${id}/volume`, v, t('salon.volume'));
        }}
        aria-label={t('salon.volume')} />
      <Icon name="volume-plus" size={18} />
      <b class="num">{num(dragging ?? volume)}</b>
    </div>
    <button class="round soft" class:active={muted} onclick={() => act(`${id}/mute`, !muted, t('salon.sourdine'))} aria-label={t('salon.couper_son')}>
      <Icon name={muted ? 'volume-off' : 'volume-high'} size={22} />
    </button>
  </div>
</section>

<style>
  .track b {
    font-weight: 650;
  }

  .row {
    display: grid;
    grid-template-columns: auto 1fr auto;
    align-items: center;
    gap: 14px;
    margin-top: 14px;
  }

  .round {
    width: 56px;
    height: 56px;
    border-radius: 50%;
    border: 0;
    background: var(--ink);
    color: var(--bg);
    display: grid;
    place-items: center;
  }

  .round.soft {
    width: 48px;
    height: 48px;
    background: var(--surface-2);
    color: var(--ink-2);
  }

  .round.soft.active {
    color: var(--alert);
  }

  .vol {
    display: flex;
    align-items: center;
    gap: 10px;
    color: var(--ink-3);
    min-width: 0;
  }

  .vol input {
    flex: 1;
    min-width: 0;
    accent-color: var(--ink);
    height: 28px;
  }

  .vol b {
    width: 28px;
    text-align: right;
    color: var(--ink);
  }
</style>
