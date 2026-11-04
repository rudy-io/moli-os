<script>
  import { hub, device, deviceKind, value, act, num, nameOf, reachable, savePref } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import { pointOf, canWrite, numericRange, quantize } from '../lib/capabilities.js';

  /** A speaker (Cast, Sonos, AirPlay…): cover art, title, player selector
   *  (with transfer to another speaker), previous / play-pause / next,
   *  volume, mute, and announcement volume when supported. */
  let { id = null, name = null } = $props();

  const speakers = $derived(Object.values(hub.devices).filter((d) => deviceKind(d) === 'speaker'));
  let picked = $state((() => {
    try {
      return localStorage.getItem('moli.speaker');
    } catch {
      return null;
    }
  })());

  const activeId = $derived.by(() => {
    if (id && device(id)) return id;
    if (picked && speakers.some((d) => d.id === picked)) return picked;
    const playingOne = speakers.find((d) => value(d.id, 'playing') === true);
    if (playingOne) return playingOne.id;
    const withTrack = speakers.find((d) => value(d.id, 'title') && value(d.id, 'title') !== 'Default Media Receiver');
    if (withTrack) return withTrack.id;
    return speakers[0]?.id ?? id;
  });

  const current = $derived(device(activeId));
  const online = $derived(reachable(activeId));
  const tells = $derived(pointOf(current, 'playing') || pointOf(current, 'state'));
  const playing = $derived(value(activeId, 'playing') === true);
  const state = $derived(value(activeId, 'state'));
  const rawTitle = $derived(value(activeId, 'title'));
  const title = $derived(rawTitle && rawTitle !== 'Default Media Receiver' ? rawTitle : null);
  const artist = $derived(value(activeId, 'artist'));
  const image = $derived(value(activeId, 'image'));
  const volume = $derived(value(activeId, 'volume'));
  const muted = $derived(value(activeId, 'mute') === true);

  const plays = $derived(canWrite(current, 'playing'));
  const hasSkip = $derived(canWrite(current, 'next'));
  const canPlaySearch = $derived(canWrite(current, 'play'));
  const volumeRange = $derived(numericRange(current, 'volume', 1, { min: 0, max: 100 }));
  const announceRange = $derived(numericRange(current, 'announce_volume', 5, { min: 0, max: 100 }));
  const sets = $derived(canWrite(current, 'volume') && volumeRange);
  const mutes = $derived(canWrite(current, 'mute'));
  const announces = $derived(canWrite(current, 'announce_volume') && announceRange);
  const announceVolume = $derived(value(activeId, 'announce_volume'));

  let dragging = $state(null);
  let draggingAnnounce = $state(null);
  let query = $state('');

  function setLevel(key, event) {
    if (!canWrite(current, key)) return;
    const range = key === 'volume' ? volumeRange : announceRange;
    const next = quantize(Number(event.currentTarget.value), range);
    if (next != null && next !== value(activeId, key)) {
      act(`${activeId}/${key}`, next, t(key === 'volume' ? 'salon.volume' : 'salon.enceinte.volume_annonce'));
    }
  }

  function selectSpeaker(targetId) {
    if (targetId === activeId) return;
    const prevId = activeId;
    const wasPlaying = playing;
    const trackQuery = title ? (artist ? `${title} ${artist}` : title) : null;
    picked = targetId;
    savePref('moli.speaker', targetId);
    const target = device(targetId);
    if (wasPlaying && trackQuery && canWrite(target, 'play')) {
      act(`${prevId}/playing`, false, t('salon.pause'));
      act(`${targetId}/play`, trackQuery, t('salon.enceinte.jouer_sur', { name: nameOf(targetId) }));
    }
  }

  function alsoPlayOn(targetId) {
    const trackQuery = title ? (artist ? `${title} ${artist}` : title) : null;
    if (!trackQuery) return;
    act(`${targetId}/play`, trackQuery, t('salon.enceinte.jouer_sur', { name: nameOf(targetId) }));
  }

  function submitSearch(e) {
    e.preventDefault();
    const q = query.trim();
    if (!q || !activeId) return;
    act(`${activeId}/play`, q, t('salon.enceinte.jouer_sur', { name: nameOf(activeId) }));
    query = '';
  }
</script>

{#if current}
  <section class="card sound" class:lit={playing}>
    <div class="card-head">
      <h2><Icon name="speaker" size={18} />{id ? nameOf(activeId, name) : t('salon.enceinte.multimedia')}</h2>
      {#if !online}
        <span class="chip">{t('commun.piece.injoignable')}</span>
      {:else if tells}
        <span class="chip {playing ? 'warm' : ''}">
          {playing
            ? t('salon.enceinte.lecture')
            : state === 'paused' || state === 'paused_playback'
              ? t('salon.enceinte.en_pause')
              : t('salon.enceinte.arret')}
        </span>
      {/if}
    </div>

    {#if !id && speakers.length > 1}
      <div class="speakers" role="group" aria-label={t('salon.enceinte.lecteur')}>
        {#each speakers as sp (sp.id)}
          {@const spPlaying = value(sp.id, 'playing') === true}
          <div class="sp-pill" class:on={sp.id === activeId} class:off={!reachable(sp.id)}>
            <button
              class="sp-pick"
              onclick={() => selectSpeaker(sp.id)}
              aria-pressed={sp.id === activeId}
              title={playing && sp.id !== activeId && title ? t('salon.enceinte.basculer_vers', { name: nameOf(sp.id) }) : nameOf(sp.id)}>
              <Icon name="speaker" size={14} />
              <span>{nameOf(sp.id)}</span>
              {#if spPlaying}<span class="dot" aria-hidden="true"></span>{/if}
            </button>
            {#if sp.id !== activeId && title && reachable(sp.id) && canWrite(sp, 'play')}
              <button
                class="sp-add"
                onclick={() => alsoPlayOn(sp.id)}
                title={t('salon.enceinte.ajouter_lecteur', { name: nameOf(sp.id) })}
                aria-label={t('salon.enceinte.ajouter_lecteur', { name: nameOf(sp.id) })}>
                <Icon name="plus" size={13} />
              </button>
            {/if}
          </div>
        {/each}
      </div>
    {/if}

    {#if tells || title || plays}
      <div class="now-playing">
        <div class="cover" class:spinning={playing}>
          {#if image}
            <img src={image} alt="" referrerpolicy="no-referrer" />
          {:else}
            <Icon name="speaker" size={28} />
          {/if}
        </div>
        <div class="meta">
          {#if title}
            <b class="track-title" title={title}>{title}</b>
            {#if artist}<span class="artist muted" title={artist}>{artist}</span>{/if}
          {:else if tells}
            <span class="muted">{playing ? t('salon.enceinte.son_tele') : t('salon.enceinte.rien')}</span>
          {/if}
          <span class="on-device">{nameOf(activeId, name)}</span>
        </div>
        {#if plays}
          <div class="transport">
            {#if hasSkip}
              <button
                class="round soft small-btn"
                disabled={!online || !title}
                onclick={() => act(`${activeId}/previous`, true, t('salon.tele.precedent'))}
                aria-label={t('salon.tele.precedent')}
                title={t('salon.tele.precedent')}>
                <Icon name="previous" size={20} />
              </button>
            {/if}
            <button
              class="round"
              disabled={!online}
              onclick={() => act(`${activeId}/playing`, !playing, playing ? t('salon.pause') : t('salon.lecture'))}
              aria-label={playing ? t('salon.pause') : t('salon.lecture')}
              title={playing ? t('salon.pause') : t('salon.lecture')}>
              <Icon name={playing ? 'pause' : 'play'} size={26} />
            </button>
            {#if hasSkip}
              <button
                class="round soft small-btn"
                disabled={!online || !title}
                onclick={() => act(`${activeId}/next`, true, t('salon.tele.suivant'))}
                aria-label={t('salon.tele.suivant')}
                title={t('salon.tele.suivant')}>
                <Icon name="next" size={20} />
              </button>
            {/if}
          </div>
        {/if}
      </div>
    {/if}

    {#if canPlaySearch}
      <form class="search" onsubmit={submitSearch}>
        <input
          type="text"
          bind:value={query}
          disabled={!online}
          placeholder={t('salon.enceinte.rechercher')}
          aria-label={t('salon.enceinte.rechercher')} />
        <button type="submit" disabled={!online || !query.trim()} aria-label={t('salon.tele.chercher')}>
          <Icon name="play" size={16} />
        </button>
      </form>
    {/if}

    {#if sets || mutes}
      <div class="row">
        {#if sets}
          <div class="vol">
            <Icon name="volume-minus" size={18} />
            <input
              type="range"
              min={volumeRange.first}
              max={volumeRange.last}
              step={volumeRange.step}
              disabled={!online}
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
          <button
            class="round soft small-btn"
            class:active={muted}
            disabled={!online}
            onclick={() => act(`${activeId}/mute`, !muted, t('salon.sourdine'))}
            aria-label={t('salon.couper_son')}
            title={t('salon.couper_son')}>
            <Icon name={muted ? 'volume-off' : 'volume-high'} size={20} />
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
{/if}

<style>
  .sound {
    display: grid;
    gap: 14px;
  }

  .speakers {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }

  .sp-pill {
    display: inline-flex;
    align-items: center;
    border-radius: 999px;
    background: var(--surface-2);
    color: var(--ink-3);
    overflow: hidden;
  }

  .sp-pill.on {
    background: var(--warm-soft);
    color: var(--warm-ink);
  }

  .sp-pill.off {
    opacity: 0.5;
  }

  .sp-pick {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    border: 0;
    background: transparent;
    color: inherit;
    padding: 6px 12px;
    font: inherit;
    font-size: 13px;
    font-weight: 650;
    cursor: pointer;
  }

  .sp-add {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    margin-right: 4px;
    border: 0;
    border-radius: 50%;
    background: color-mix(in srgb, var(--ink) 10%, transparent);
    color: inherit;
    cursor: pointer;
  }

  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: currentColor;
  }

  .now-playing {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    align-items: center;
    gap: 14px;
  }

  .cover {
    width: 64px;
    height: 64px;
    border-radius: 14px;
    background: var(--surface-2);
    color: var(--ink-3);
    display: grid;
    place-items: center;
    overflow: hidden;
    flex: none;
  }

  .cover img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }

  .meta {
    min-width: 0;
    display: grid;
    gap: 2px;
  }

  .track-title {
    font-size: 16px;
    font-weight: 700;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .artist {
    font-size: 13px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .on-device {
    font-size: 11px;
    font-weight: 650;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--ink-3);
    margin-top: 2px;
  }

  .transport {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .search {
    display: flex;
    gap: 8px;
  }

  .search input {
    flex: 1;
    min-width: 0;
    border: 1px solid var(--line);
    border-radius: 999px;
    padding: 8px 14px;
    font: inherit;
    font-size: 13px;
    background: var(--surface-2);
    color: var(--ink);
  }

  .search button {
    width: 36px;
    height: 36px;
    border-radius: 50%;
    border: 0;
    background: var(--ink);
    color: var(--bg);
    display: grid;
    place-items: center;
    cursor: pointer;
    flex: none;
  }

  .search button:disabled,
  .round:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .row {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .round {
    width: 48px;
    height: 48px;
    border-radius: 50%;
    border: 0;
    background: var(--ink);
    color: var(--bg);
    display: grid;
    place-items: center;
    cursor: pointer;
  }

  .round.soft {
    background: var(--surface-2);
    color: var(--ink-2);
  }

  .round.small-btn {
    width: 38px;
    height: 38px;
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
    color: var(--ink-3);
    font-size: 0.9rem;
  }

  .announce span {
    min-width: 0;
    overflow-wrap: anywhere;
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

  @media (max-width: 480px) {
    .now-playing {
      grid-template-columns: auto minmax(0, 1fr);
    }

    .transport {
      grid-column: 1 / -1;
      justify-content: center;
    }
  }
</style>
