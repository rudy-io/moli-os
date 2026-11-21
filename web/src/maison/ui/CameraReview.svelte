<script>
  import { onDestroy } from 'svelte';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import { home } from '../lib/home.svelte.js';
  import LiveVideo from './LiveVideo.svelte';
  import CameraTile from './CameraTile.svelte';
  import { dayOf, at, timeAt, bars, playable, windowFrom, windowOf, listed, labelsOf, hours, outline, readable, extract, extractName, recordedIn, toField, fromField, LEAD } from '../lib/review.js';

  /** A camera with its past (its NVR): the picture (live, or what was
   *  recorded at the chosen moment, or an event's photo) with the zones over
   *  it, the frieze of one day, and what it saw that day. */
  let { id, name, live = false, battery = false, start = null } = $props();

  const base = $derived(`/api/devices/${encodeURIComponent(id)}/review`);
  const nowS = () => Date.now() / 1000;

  // ── The day shown ─────────────────────────────────────────────
  // Today follows midnight (`home.now` ticks), even with the viewer open;
  // a past day stays the one picked.
  const today = $derived(dayOf(home.now / 1000)[0]);
  let picked = $state(null); // the start of a past day; null = today
  const span = $derived.by(() => {
    const [start, end] = dayOf(picked ?? today);
    return { after: start, before: end };
  });
  const isToday = $derived(span.after === today);
  const dayBack = $derived(Math.round((today - span.after) / 86_400)); // 0 = today, 1 = yesterday…
  let items = $state([]);
  let recorded = $state([]);
  let loading = $state(true);
  // Why the day cannot show: a refusal in the house's words (a guest, a
  // protected room: the server says them), else « Frigate does not answer »
  // with what it said kept for a hover (`Frigate: HTTP 500 for …`).
  let problem = $state(null);

  async function load() {
    const { after, before } = span;
    const asked = `${after}`;
    try {
      const res = await fetch(`${base}?after=${after}&before=${Math.min(before, Math.ceil(nowS()))}`);
      const body = await res.json().catch(() => ({}));
      if (`${span.after}` !== asked) return; // another day meanwhile
      if (res.status === 403) {
        problem = { text: body.error ?? t('revue.injoignable'), detail: '' };
        return;
      }
      if (!res.ok) throw new Error(body.error ?? res.statusText);
      items = body.items ?? [];
      recorded = body.recorded ?? [];
      problem = null;
    } catch (e) {
      problem = { text: t('revue.injoignable'), detail: e.message };
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    span;
    loading = true;
    items = [];
    recorded = [];
    load();
    // Today keeps coming: refreshed each minute.
    if (!isToday) return;
    const every = setInterval(load, 60_000);
    return () => clearInterval(every);
  });

  // ── Zones and the picture's shape ─────────────────────────────
  let zones = $state([]);
  let showZones = $state(false);
  let frame = $state({ w: 16, h: 9 });

  $effect(() => {
    fetch(`${base}/zones`)
      .then((r) => (r.ok ? r.json() : { zones: [] }))
      .then((b) => (zones = b.zones ?? []))
      .catch(() => {});
  });

  // The proportions of the picture shown (live, recording, photo, image),
  // so the zones fall on it: never an image asked just for them (a camera
  // on battery would wake for nothing).
  function sized(w, h) {
    if (w > 0 && h > 0 && (w !== frame.w || h !== frame.h)) frame = { w, h };
  }

  // ── What plays ────────────────────────────────────────────────
  // { kind: 'live' } | { kind: 'video', start, end } | { kind: 'image', event, at }
  let mode = $state({ kind: 'live' });
  // The live view failed here (away from home, a firewall): the images instead.
  let noLive = $state(false);
  let video = $state(null);
  let position = $state(null); // seconds, while a recording plays
  let notice = $state('');

  function playFrom(when) {
    notice = '';
    const from = playable(when, recorded);
    if (from === null) {
      notice = t('revue.rien_apres');
      return;
    }
    const w = windowFrom(from, nowS());
    if (!w) {
      notice = t('revue.pas_encore');
      return;
    }
    mode = { kind: 'video', ...w };
    position = w.start;
  }

  function playItem(item) {
    notice = '';
    const w = windowOf(item, nowS());
    if (!w) {
      notice = t('revue.pas_encore');
      return;
    }
    mode = { kind: 'video', ...w, item: item.id };
    position = w.start;
  }

  function showPhoto(item) {
    notice = '';
    mode = { kind: 'image', event: item.events[0], at: item.start, item: item.id };
    position = item.start;
  }

  function backToLive() {
    notice = '';
    mode = { kind: 'live' };
    position = null;
  }

  // The NVR cuts at a key frame: the video may begin a few seconds before
  // the moment asked. Those seconds are skipped, and the clock follows.
  let lead = 0;
  function onmeta() {
    if (!video || mode.kind !== 'video') return;
    sized(video.videoWidth, video.videoHeight);
    const extra = video.duration - (mode.end - mode.start);
    lead = Number.isFinite(extra) && extra > 0.5 ? extra : 0;
    if (lead) video.currentTime = lead;
  }

  function ontime() {
    if (video && mode.kind === 'video') position = mode.start - lead + video.currentTime;
  }

  // At the end of a window, the next recorded moment follows.
  function onended() {
    if (mode.kind !== 'video') return;
    const next = playable(mode.end, recorded);
    if (next !== null && next < nowS() - 5) playFrom(next);
  }

  onDestroy(() => {
    if (video) video.removeAttribute('src');
  });

  // Opened on an event (the strip of the newest): it plays at once.
  $effect(() => {
    if (start) playItem(start);
  });

  // ── An extract to save ────────────────────────────────────────
  // Offered on what is shown: the event played (its window), else a minute
  // from the moment playing; each bound moves by 10 s or to that moment.
  let cut = $state(null); // { start, end } while the panel is open

  function openCut() {
    if (cut) {
      cut = null;
      return;
    }
    if (mode.kind === 'video') {
      const from = Math.floor(position ?? mode.start);
      cut = mode.item ? { start: mode.start, end: mode.end } : { start: from, end: from + 60 };
    } else if (mode.kind === 'image') {
      const item = items.find((i) => i.id === mode.item);
      cut = (item && windowOf(item, nowS())) ?? { start: Math.floor(mode.at - LEAD), end: Math.floor(mode.at) + 30 };
    } else {
      // From the live view: the last minute, to be set to any moment.
      const now = Math.floor(nowS());
      cut = { start: now - 62, end: now - 2 };
    }
  }

  function typed(bound, value) {
    const t = fromField(value);
    if (t !== null) cut = { ...cut, [bound]: t };
  }

  function nudge(bound, seconds) {
    cut = { ...cut, [bound]: cut[bound] + seconds };
  }

  function here(bound) {
    if (position) cut = { ...cut, [bound]: Math.floor(position) };
  }

  const saved = $derived(cut ? extract(cut.start, cut.end, home.now / 1000) : null);
  const savedName = $derived(saved ? extractName(name, saved.start) : '');
  // What is recorded is known for the day shown only: elsewhere, let it try.
  const savedHas = $derived(
    !saved || saved.start < span.after || saved.end > span.before || recordedIn(saved.start, saved.end, recorded),
  );
  function lengthOf(s) {
    const m = Math.floor(s / 60);
    const r = s % 60;
    return m ? (r ? `${m} min ${r} s` : `${m} min`) : `${r} s`;
  }

  // ── The frieze ────────────────────────────────────────────────
  let frieze = $state(null);
  let hover = $state(null); // fraction under the pointer
  const shownBars = $derived(bars(recorded, span.after, span.before));
  const marks = $derived(hours(span.after, span.before));
  const nowMark = $derived(isToday ? at(home.now / 1000, span.after, span.before) : null);

  function fractionOf(e) {
    const r = frieze.getBoundingClientRect();
    return (e.clientX - r.left) / r.width;
  }

  function onfriezekey(e) {
    const step = e.shiftKey ? 600 : 60;
    if (e.key === 'ArrowRight' || e.key === 'ArrowLeft') {
      e.preventDefault();
      const from = position ?? (isToday ? nowS() - 3600 : span.after);
      playFrom(from + (e.key === 'ArrowRight' ? step : -step));
    }
  }

  // ── The list ──────────────────────────────────────────────────
  let label = $state(null);
  let more = $state(false);
  const labels = $derived(labelsOf(items));
  const shown = $derived(listed(items, label));
  const visible = $derived(more ? shown : shown.slice(0, 24));

  const hm = (s) => new Date(s * 1000).toLocaleTimeString(locale(), { hour: '2-digit', minute: '2-digit' });
  const hms = (s) => new Date(s * 1000).toLocaleTimeString(locale(), { hour: '2-digit', minute: '2-digit', second: '2-digit' });
  const dayTitle = $derived(
    dayBack === 0
      ? t('revue.aujourdhui')
      : dayBack === 1
        ? t('revue.hier')
        : new Date(span.after * 1000).toLocaleDateString(locale(), { weekday: 'long', day: 'numeric', month: 'long' }),
  );
  const labelName = (l) => {
    const key = `revue.label.${l}`;
    const s = t(key);
    return s === key ? readable(l) : s;
  };
  const LABEL_ICONS = { person: 'account', car: 'car', motorcycle: 'car', bicycle: 'walk', dog: 'dog', cat: 'dog', bird: 'leaf', package: 'package' };
  function duration(item) {
    if (item.end == null) return t('revue.en_cours');
    const s = Math.max(1, Math.round(item.end - item.start));
    return s < 60 ? t('revue.secondes', { n: s }) : t('revue.minutes', { n: Math.round(s / 60) });
  }
</script>

<div class="review">
  <div class="player" style:aspect-ratio="{frame.w} / {frame.h}">
    {#if mode.kind === 'video'}
      <!-- svelte-ignore a11y_media_has_caption -->
      <video
        bind:this={video}
        src="{base}/recording?start={mode.start}&end={mode.end}"
        autoplay
        muted
        playsinline
        controls
        onloadedmetadata={onmeta}
        ontimeupdate={ontime}
        {onended}
      ></video>
      <span class="badge past"><Icon name="history" size={14} />{position ? hms(position) : ''}</span>
    {:else if mode.kind === 'image'}
      <img
        src="{base}/events/{encodeURIComponent(mode.event)}/image"
        alt={t('revue.photo_de', { heure: hm(mode.at) })}
        onload={(e) => sized(e.currentTarget.naturalWidth, e.currentTarget.naturalHeight)}
      />
      <span class="badge past"><Icon name="camera" size={14} />{hms(mode.at)}</span>
    {:else if live && !noLive}
      <LiveVideo {id} onfail={() => (noLive = true)} onsize={sized} />
    {:else}
      <CameraTile {id} {name} refresh={battery ? 0 : 2} {battery} big onsize={sized} />
      {#if noLive}
        <p class="no-live">
          {t('salon.cameras.direct_impossible')}
          <button onclick={() => (noLive = false)}>{t('salon.cameras.essayer_direct')}</button>
        </p>
      {/if}
    {/if}

    {#if showZones && zones.length}
      <svg class="zones" viewBox="0 0 {frame.w} {frame.h}" preserveAspectRatio="xMidYMid meet" aria-hidden="true">
        {#each zones as z (z.name)}
          <polygon points={outline(z, frame.w, frame.h)} />
          <text x={z.points.reduce((s, p) => s + p[0], 0) / z.points.length * frame.w} y={z.points.reduce((s, p) => s + p[1], 0) / z.points.length * frame.h} font-size={frame.h / 24}>{readable(z.name)}</text>
        {/each}
      </svg>
    {/if}
  </div>

  <div class="tools">
    <div class="day" role="group" aria-label={t('revue.jour')}>
      <button onclick={() => (picked = dayOf(span.after - 1)[0])} aria-label={t('revue.jour_precedent')}><Icon name="chevron-left" size={20} /></button>
      <b>{dayTitle}</b>
      <button onclick={() => (picked = span.before >= today ? null : span.before)} disabled={isToday} aria-label={t('revue.jour_suivant')}><Icon name="chevron-right" size={20} /></button>
    </div>
    <div class="actions">
      {#if zones.length}
        <button class:on={showZones} aria-pressed={showZones} onclick={() => (showZones = !showZones)}>
          <Icon name="map" size={16} />{t('revue.zones')}
        </button>
      {/if}
      <button class:on={cut} aria-pressed={!!cut} aria-expanded={!!cut} onclick={openCut}>
        <Icon name="download" size={16} />{t('revue.export.exporter')}
      </button>
      {#if mode.kind !== 'live'}
        <button class="direct" onclick={backToLive}><i></i>{t('revue.direct')}</button>
      {/if}
    </div>
  </div>

  {#if cut}
    <div class="extract" role="group" aria-label={t('revue.export.titre')}>
      <b class="extract-title">{t('revue.export.titre')}</b>
      {#each [['start', t('revue.export.debut')], ['end', t('revue.export.fin')]] as [bound, word] (bound)}
        <div class="bound">
          <span class="word">{word}</span>
          <button onclick={() => nudge(bound, -10)} aria-label={t('revue.export.reculer', { borne: word })}>−10 s</button>
          <input
            class="at"
            type="datetime-local"
            step="1"
            value={toField(cut[bound])}
            onchange={(e) => typed(bound, e.currentTarget.value)}
            aria-label={word}
          />
          <button onclick={() => nudge(bound, 10)} aria-label={t('revue.export.avancer', { borne: word })}>+10 s</button>
          {#if mode.kind === 'video' && position}
            <button onclick={() => here(bound)} aria-label={t('revue.export.ici_aide', { borne: word })}>{t('revue.export.ici')}</button>
          {/if}
        </div>
      {/each}
      <div class="go">
        {#if saved && !savedHas}
          <span class="bad">{t('revue.export.rien')}</span>
        {:else if saved}
          <span class="length">{t('revue.export.duree', { duree: lengthOf(saved.end - saved.start) })}</span>
          <a class="save" href="{base}/recording?start={saved.start}&end={saved.end}&download={savedName}" download={savedName}>
            <Icon name="download" size={16} />{t('revue.export.telecharger')}
          </a>
        {:else}
          <span class="bad">{t('revue.export.invalide')}</span>
        {/if}
      </div>
    </div>
  {/if}

  <div class="frieze-wrap">
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="frieze"
      bind:this={frieze}
      role="slider"
      tabindex="0"
      aria-label={t('revue.frise', { jour: dayTitle })}
      aria-valuemin={span.after}
      aria-valuemax={span.before}
      aria-valuenow={position ?? span.after}
      aria-valuetext={position ? hms(position) : t('revue.direct')}
      onclick={(e) => playFrom(timeAt(fractionOf(e), span.after, span.before))}
      onpointermove={(e) => (hover = Math.min(1, Math.max(0, fractionOf(e))))}
      onpointerleave={() => (hover = null)}
      onkeydown={onfriezekey}
    >
      <div class="lane seen">
        {#each items as i (i.id)}
          <span
            class="item {i.severity}"
            style:left="{at(i.start, span.after, span.before) * 100}%"
            style:width="max(4px, {(at(i.end ?? nowS(), span.after, span.before) - at(i.start, span.after, span.before)) * 100}%)"
          ></span>
        {/each}
      </div>
      <div class="lane rec">
        {#each shownBars as b (b.left)}
          <span style:left="{b.left * 100}%" style:width="max(1px, {b.width * 100}%)"></span>
        {/each}
      </div>
      {#if nowMark !== null}<span class="now" style:left="{nowMark * 100}%"></span>{/if}
      {#if position && position >= span.after && position < span.before}
        <span class="head" style:left="{at(position, span.after, span.before) * 100}%"></span>
      {/if}
      {#if hover !== null}
        <span class="hover" style:left="{hover * 100}%"><small>{hm(timeAt(hover, span.after, span.before))}</small></span>
      {/if}
    </div>
    <div class="hours" aria-hidden="true">
      {#each marks as m (m.fraction)}
        <span style:left="{m.fraction * 100}%">{String(m.hour).padStart(2, '0')}h</span>
      {/each}
    </div>
    <p class="legend">
      <span><i class="k alert"></i>{t('revue.alertes')}</span>
      <span><i class="k detection"></i>{t('revue.detections')}</span>
      <span><i class="k rec"></i>{t('revue.enregistre')}</span>
      <span class="hint">{t('revue.astuce')}</span>
    </p>
    {#if notice}<p class="notice">{notice}</p>{/if}
  </div>

  <section class="seen-list">
    <div class="head-row">
      <h3>{t('revue.vu', { jour: dayTitle.toLowerCase() })}</h3>
      {#if labels.length > 1}
        <div class="filters" role="group" aria-label={t('commun.filtre.ne_voir_que')}>
          <button class:active={!label} aria-pressed={!label} onclick={() => (label = null)}>{t('commun.filtre.tout')}</button>
          {#each labels as l (l)}
            <button class:active={label === l} aria-pressed={label === l} onclick={() => (label = label === l ? null : l)}>
              <Icon name={LABEL_ICONS[l] ?? 'motion'} size={15} />{labelName(l)}
            </button>
          {/each}
        </div>
      {/if}
    </div>

    {#if loading}
      <p class="muted">…</p>
    {:else if problem}
      <p class="muted" title={problem.detail || null}>{problem.text}</p>
    {:else if !shown.length}
      <p class="muted">{t('revue.rien')}</p>
    {:else}
      <ul class="events">
        {#each visible as i (i.id)}
          <li class:current={mode.item === i.id}>
            <button class="thumb" onclick={() => playItem(i)} aria-label={t('revue.revoir', { heure: hm(i.start) })}>
              {#if i.events.length}
                <img loading="lazy" src="{base}/events/{encodeURIComponent(i.events[0])}/thumbnail" alt="" />
              {:else}
                <span class="no-thumb"><Icon name="motion" size={26} /></span>
              {/if}
              <span class="play"><Icon name="play" size={18} /></span>
            </button>
            <div class="info">
              <div class="what">
                <i class="sev {i.severity}" title={i.severity === 'alert' ? t('revue.alerte') : t('revue.detection')}></i>
                <b>{i.labels.length ? i.labels.map(labelName).join(', ') : t('revue.mouvement')}</b>
              </div>
              <span class="muted">{hm(i.start)} · {duration(i)}</span>
              {#if i.zones.length}<span class="where">{i.zones.map(readable).join(' · ')}</span>{/if}
            </div>
            {#if i.events.length}
              <button class="photo" onclick={() => showPhoto(i)} aria-label={t('revue.photo_de', { heure: hm(i.start) })}><Icon name="camera" size={18} /></button>
            {/if}
          </li>
        {/each}
      </ul>
      {#if shown.length > visible.length}
        <button class="more" onclick={() => (more = true)}>{t('revue.voir_plus', { n: shown.length - visible.length })}</button>
      {/if}
    {/if}
  </section>
</div>

<style>
  .review {
    display: grid;
    gap: 14px;
  }

  .player {
    position: relative;
    width: 100%;
    max-height: 68dvh;
    border-radius: var(--r-md);
    overflow: hidden;
    background: #000;
    display: grid;
  }

  .player > :global(*) {
    grid-area: 1 / 1;
    min-height: 0;
  }

  .no-live {
    position: absolute;
    left: 12px;
    right: 12px;
    bottom: 12px;
    margin: 0;
    padding: 8px 12px;
    border-radius: var(--r-md);
    background: color-mix(in srgb, var(--bg) 80%, transparent);
    color: var(--ink-2);
    font-size: 13px;
  }

  .no-live button {
    background: none;
    border: 0;
    padding: 0 0 0 6px;
    color: var(--cool);
    font: inherit;
    text-decoration: underline;
    cursor: pointer;
  }

  .player > :global(.live) {
    aspect-ratio: auto;
    height: 100%;
  }

  video,
  img {
    width: 100%;
    height: 100%;
    object-fit: contain;
    display: block;
  }

  .zones {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    pointer-events: none;
  }

  .zones polygon {
    fill: rgb(87 166 255 / 18%);
    stroke: #8cc4ff;
    stroke-width: 3;
    vector-effect: non-scaling-stroke;
  }

  .zones text {
    fill: #fff;
    paint-order: stroke;
    stroke: rgb(0 0 0 / 70%);
    stroke-width: 4;
    vector-effect: non-scaling-stroke;
    text-anchor: middle;
    font-weight: 650;
  }

  /* Top right: cameras write their own clock top left. */
  .badge {
    position: absolute;
    top: 10px;
    right: 10px;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 4px 10px;
    border-radius: 999px;
    background: rgb(0 0 0 / 0.6);
    color: #fff;
    font-size: 12px;
    font-weight: 650;
    font-variant-numeric: tabular-nums;
    pointer-events: none;
  }

  .tools {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
  }

  .day {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .day b {
    min-width: 12ch;
    text-align: center;
    font-size: 15px;
  }

  .day button,
  .actions button,
  .filters button,
  .more {
    border: 0;
    border-radius: 999px;
    background: rgb(255 255 255 / 11%);
    color: #eef1f7;
    font: inherit;
    font-size: 14px;
    font-weight: 600;
    cursor: pointer;
  }

  .day button {
    width: 36px;
    height: 36px;
    display: grid;
    place-items: center;
  }

  .day button:disabled {
    opacity: 0.35;
    cursor: default;
  }

  .actions {
    display: flex;
    gap: 8px;
  }

  .actions button {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 8px 14px;
  }

  .actions button.on {
    background: #8cc4ff;
    color: #0b1220;
  }

  .extract {
    display: grid;
    gap: 10px;
    padding: 14px 16px;
    border-radius: 14px;
    background: #1c2230;
  }

  .extract-title {
    font-size: 14px;
  }

  /* Both bounds in the same columns, whatever the word. */
  .bound {
    display: grid;
    grid-template-columns: 6ch auto minmax(0, 15em) auto auto;
    justify-content: start;
    align-items: center;
    gap: 8px;
  }

  .bound .word {
    color: #b7bfcd;
    font-size: 13px;
  }

  .bound .at {
    box-sizing: border-box;
    width: 100%;
    min-height: 36px;
    padding: 4px 10px;
    border: 1px solid rgb(255 255 255 / 18%);
    border-radius: 10px;
    background: rgb(255 255 255 / 6%);
    color: #eef1f7;
    color-scheme: dark;
    font: inherit;
    font-size: 14px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }

  /* A phone: the word, the field across, then the buttons. */
  @media (max-width: 560px) {
    .bound {
      display: flex;
      flex-wrap: wrap;
    }

    .bound .word {
      flex-basis: 100%;
    }

    .bound .at {
      order: 1;
      flex-basis: 100%;
    }

    .bound button {
      order: 2;
    }
  }

  .bound button,
  .save {
    border: 0;
    border-radius: 999px;
    background: rgb(255 255 255 / 11%);
    color: #eef1f7;
    font: inherit;
    font-size: 13px;
    font-weight: 600;
    padding: 7px 12px;
    cursor: pointer;
    font-variant-numeric: tabular-nums;
  }

  .go {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
  }

  .length {
    color: #b7bfcd;
    font-size: 13px;
  }

  .save {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 9px 16px;
    background: #8cc4ff;
    color: #0b1220;
    text-decoration: none;
  }

  .bad {
    color: #f5b14c;
    font-size: 13px;
  }

  .direct i {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #e5484d;
  }

  .frieze-wrap {
    display: grid;
    gap: 4px;
  }

  .frieze {
    position: relative;
    height: 46px;
    border-radius: 10px;
    background: rgb(255 255 255 / 6%);
    cursor: pointer;
    overflow: hidden;
    touch-action: pan-y;
  }

  .frieze:focus-visible {
    outline: 2px solid #8cc4ff;
    outline-offset: 2px;
  }

  .lane {
    position: absolute;
    left: 0;
    right: 0;
  }

  .lane span {
    position: absolute;
    top: 0;
    bottom: 0;
  }

  .lane.seen {
    top: 6px;
    height: 18px;
  }

  .lane.rec {
    top: 30px;
    height: 10px;
  }

  .lane.rec span {
    background: rgb(169 177 194 / 55%);
    border-radius: 2px;
  }

  .item {
    border-radius: 4px;
    box-shadow: 0 0 0 1px rgb(14 18 26 / 90%);
  }

  .item.alert,
  .k.alert,
  .sev.alert {
    background: #ff6b6b;
  }

  .item.detection,
  .k.detection,
  .sev.detection {
    background: #f5b14c;
  }

  .k.rec {
    background: rgb(169 177 194 / 55%);
  }

  .now,
  .head,
  .hover {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 2px;
    margin-left: -1px;
    pointer-events: none;
  }

  .now {
    background: rgb(255 255 255 / 35%);
  }

  .head {
    background: #8cc4ff;
    box-shadow: 0 0 0 1px rgb(0 0 0 / 50%);
  }

  .hover {
    background: rgb(255 255 255 / 70%);
  }

  .hover small {
    position: absolute;
    top: 2px;
    left: 5px;
    padding: 1px 6px;
    border-radius: 6px;
    background: rgb(0 0 0 / 75%);
    color: #fff;
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .hours {
    position: relative;
    height: 16px;
    font-size: 11px;
    color: #a9b1c2;
    font-variant-numeric: tabular-nums;
  }

  .hours span {
    position: absolute;
    transform: translateX(-50%);
  }

  .hours span:first-child {
    transform: none;
  }

  .hours span:last-child {
    transform: translateX(-100%);
  }

  .legend {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 16px;
    margin: 4px 0 0;
    font-size: 12px;
    color: #a9b1c2;
  }

  .legend span {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  .legend .hint {
    margin-left: auto;
  }

  .k {
    width: 12px;
    height: 8px;
    border-radius: 2px;
  }

  .notice {
    margin: 0;
    font-size: 13px;
    color: #f5b14c;
  }

  .head-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    margin-bottom: 10px;
  }

  .seen-list h3 {
    font-size: 13px;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: #a9b1c2;
  }

  .filters {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .filters button {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 6px 12px;
    font-size: 13px;
  }

  .filters button.active {
    background: #eef1f7;
    color: #0b1220;
  }

  .events {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 260px), 1fr));
    gap: 10px;
  }

  .events li {
    display: grid;
    grid-template-columns: 112px 1fr auto;
    align-items: center;
    gap: 12px;
    padding: 8px;
    border-radius: 12px;
    background: rgb(255 255 255 / 7%);
  }

  .events li.current {
    background: rgb(140 196 255 / 18%);
    box-shadow: inset 0 0 0 1px #8cc4ff;
  }

  .thumb {
    position: relative;
    width: 112px;
    aspect-ratio: 16 / 10;
    padding: 0;
    border: 0;
    border-radius: 8px;
    overflow: hidden;
    background: #000;
    cursor: pointer;
  }

  .thumb img {
    object-fit: cover;
  }

  .no-thumb {
    display: grid;
    place-items: center;
    height: 100%;
    color: #a9b1c2;
  }

  .play {
    position: absolute;
    right: 6px;
    bottom: 6px;
    width: 26px;
    height: 26px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: rgb(0 0 0 / 60%);
    color: #fff;
  }

  .info {
    display: grid;
    gap: 2px;
    min-width: 0;
    font-size: 14px;
  }

  .what {
    display: flex;
    align-items: center;
    gap: 7px;
  }

  .what b {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .sev {
    flex: none;
    width: 9px;
    height: 9px;
    border-radius: 50%;
  }

  .info .muted,
  .where {
    font-size: 12.5px;
    color: #a9b1c2;
    font-variant-numeric: tabular-nums;
  }

  .where {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .photo {
    width: 38px;
    height: 38px;
    border: 0;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: rgb(255 255 255 / 9%);
    color: #eef1f7;
    cursor: pointer;
  }

  .more {
    margin-top: 10px;
    padding: 8px 16px;
  }

  .seen-list .muted {
    color: #a9b1c2;
  }

  @media (max-width: 560px) {
    .legend .hint {
      display: none;
    }
  }
</style>
