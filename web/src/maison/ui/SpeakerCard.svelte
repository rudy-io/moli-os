<script>
  import { value, act, num, nameOf, device } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import { pointOf, canWrite, numericRange, quantize } from '../lib/capabilities.js';

  /** A speaker (Sonos…): what plays, play/pause, volume, mute, and the
   *  level an announcement is said at when the speaker lets it be set.
   *  Each control shows only where its point can be written, and what plays
   *  only where the speaker tells it: a receiver that only speaks (AirPlay)
   *  has no play/pause, no mute, and never says it is stopped. */
  let { id, name = null } = $props();

  const d = $derived(device(id));
  const tells = $derived(pointOf(d, 'playing') || pointOf(d, 'state'));
  const playing = $derived(value(id, 'playing') === true);
  const title = $derived(value(id, 'title'));
  const artist = $derived(value(id, 'artist'));
  const volume = $derived(value(id, 'volume'));
  const muted = $derived(value(id, 'mute') === true);
  let dragging = $state(null);
  const volumeRange = $derived(numericRange(d, 'volume', 1, { min: 0, max: 100 }));
  const announceRange = $derived(numericRange(d, 'announce_volume', 5, { min: 0, max: 100 }));
  const plays = $derived(canWrite(d, 'playing'));
  const sets = $derived(canWrite(d, 'volume') && volumeRange);
  const mutes = $derived(canWrite(d, 'mute'));
  const announces = $derived(canWrite(d, 'announce_volume') && announceRange);
  const announceVolume = $derived(value(id, 'announce_volume'));
  let draggingAnnounce = $state(null);

  function setLevel(key, event) {
    if (!canWrite(d, key)) return;
    const range = key === 'volume' ? volumeRange : announceRange;
    const next = quantize(Number(event.currentTarget.value), range);
    if (next != null && next !== value(id, key)) {
      act(`${id}/${key}`, next, t(key === 'volume' ? 'salon.volume' : 'salon.enceinte.volume_annonce'));
    }
  }
</script>

<section class="card sound">
  <div class="card-head">
    <h2><Icon name="speaker" size={18} />{nameOf(id, name)}</h2>
    {#if tells}
      <span class="chip {playing ? 'warm' : ''}">{playing ? t('salon.enceinte.lecture') : value(id, 'state') === 'paused_playback' ? t('salon.enceinte.en_pause') : t('salon.enceinte.arret')}</span>
    {/if}
  </div>
  {#if title}
    <p class="track"><b>{title}</b>{#if artist}<span class="muted"> · {artist}</span>{/if}</p>
  {:else if tells}
    <p class="muted">{playing ? t('salon.enceinte.son_tele') : t('salon.enceinte.rien')}</p>
  {/if}
  {#if plays || sets || mutes}
    <div class="row">
      {#if plays}
        <button class="round" onclick={() => plays && act(`${id}/playing`, !playing, t('salon.lecture'))} aria-label={playing ? t('salon.pause') : t('salon.lecture')}>
          <Icon name={playing ? 'pause' : 'play'} size={26} />
        </button>
      {/if}
      {#if sets}
        <div class="vol">
          <Icon name="volume-minus" size={18} />
          <input
            type="range"
            min={volumeRange.first}
            max={volumeRange.last}
            step={volumeRange.step}
            value={dragging ?? volume ?? volumeRange.first}
            oninput={(e) => (dragging = Number(e.currentTarget.value))}
            onchange={(e) => {
              dragging = null;
              setLevel('volume', e);
            }}
            aria-label={t('salon.volume')} />
          <Icon name="volume-plus" size={18} />
          <b class="num">{num(dragging ?? volume)}</b>
        </div>
      {/if}
      {#if mutes}
        <button class="round soft" class:active={muted} onclick={() => mutes && act(`${id}/mute`, !muted, t('salon.sourdine'))} aria-label={t('salon.couper_son')}>
          <Icon name={muted ? 'volume-off' : 'volume-high'} size={22} />
        </button>
      {/if}
    </div>
  {/if}
  {#if announces}
    <label class="announce">
      <span>{t('salon.enceinte.volume_annonce')}</span>
      <input
        type="range"
        min={announceRange.first}
        max={announceRange.last}
        step={announceRange.step}
        value={draggingAnnounce ?? announceVolume ?? announceRange.first}
        oninput={(e) => (draggingAnnounce = Number(e.currentTarget.value))}
        onchange={(e) => {
          draggingAnnounce = null;
          setLevel('announce_volume', e);
        }} />
      <b class="num">{num(draggingAnnounce ?? announceVolume)}</b>
    </label>
  {/if}
</section>

<style>
  .track b {
    font-weight: 650;
  }

  .row {
    display: flex;
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
    flex: 1;
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

  .announce {
    display: grid;
    grid-template-columns: minmax(0, 11ch) 1fr auto;
    align-items: center;
    gap: 10px;
    margin-top: 12px;
    color: var(--ink-3);
    font-size: 0.9rem;
  }

  .announce span {
    min-width: 0;
    overflow-wrap: break-word;
  }

  .announce input {
    min-width: 0;
    accent-color: var(--ink);
    height: 24px;
  }

  .announce b {
    width: 28px;
    text-align: right;
    color: var(--ink);
  }
</style>
