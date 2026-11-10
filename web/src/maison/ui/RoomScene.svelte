<script>
  import { onMount } from 'svelte';
  import { hub, value, toggle, isOn, powerKey, nameOf, deviceKind, act, num } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import { canWrite, climateKeys, numericRange } from '../lib/capabilities.js';
  import Icon from './Icon.svelte';

  /** One room as a picture: its photo (or a warm evening backdrop), its
   *  controls floating on it around the house's hub, a dimmer on the right
   *  edge, what it measures at the bottom. A tap on a lamp turns it on or
   *  off; a clim or a shutter opens its full control (onopen). The photo is
   *  sent by the parent (onphoto), made small here first. */
  let { room, photo = null, onphoto = async () => {}, onopen = () => {} } = $props();

  let sceneEl = $state(null);
  let fileInput = $state(null);
  let narrow = $state(false);
  let wanted = $state(null);
  let wantedTimer = null;
  let errorTimer = null;
  let sending = $state(false);
  let failed = $state(false);
  let busy = $state({});

  onMount(() => {
    const mq = window.matchMedia('(max-width: 760px)');
    narrow = mq.matches;
    const change = (e) => (narrow = e.matches);
    mq.addEventListener('change', change);
    return () => {
      mq.removeEventListener('change', change);
      clearTimeout(wantedTimer);
      clearTimeout(keyTimer);
      clearTimeout(errorTimer);
    };
  });

  const rx = $derived(narrow ? 31 : 34);
  /** On a phone the ring leans left: the dimmer keeps the right edge. */
  const CX = $derived(narrow ? 43 : 50);
  const ry = $derived(narrow ? 30 : 28);
  /** The hub sits a little low: the top keeps room for the photo's call. */
  const CY = 54;

  const controls = $derived.by(() => {
    const out = [];
    const r = room ?? {};
    for (const d of [...(r.lights ?? []), ...(r.plugs ?? []), ...(r.climates ?? []), ...(r.covers ?? []), ...(r.media ?? [])]) {
      const live = d?.id ? hub.devices[d.id] : null;
      if (live) out.push({ id: live.id, d: live, kind: deviceKind(live), name: nameOf(live.id) });
    }
    return out.slice(0, 10);
  });

  const placed = $derived(
    controls.map((c, i) => {
      const a = -Math.PI / 2 + (2 * Math.PI * i) / Math.max(1, controls.length);
      return { ...c, x: CX + rx * Math.cos(a), y: CY + ry * Math.sin(a) };
    }),
  );

  const lights = $derived(controls.filter((c) => c.kind === 'light'));
  const dimmable = $derived(lights.filter((c) => canWrite(c.d, 'brightness')));
  const writable = $derived(lights.filter((c) => canWrite(c.d, powerKey(c.d))));
  const anyOn = $derived(writable.some((c) => isOn(c.id)));
  const onShare = $derived(lights.length ? lights.filter((c) => isOn(c.id)).length / lights.length : 0);

  /** Each lamp says its own range (0–255, 1–100…); 0–100 when it says none. */
  const reads = (x) => typeof x === 'number' && Number.isFinite(x);
  const span = (d) => numericRange(d, 'brightness', 1) ?? { min: 0, max: 100 };
  const toShare = (d, raw) => {
    const { min, max } = span(d);
    return Math.max(0, Math.min(100, ((raw - min) / (max - min)) * 100));
  };
  const fromShare = (d, share) => {
    const { min, max } = span(d);
    return Math.round(min + (share / 100) * (max - min));
  };
  const average = $derived.by(() => {
    const vals = dimmable
      .map((c) => {
        const raw = value(c.id, 'brightness');
        return reads(raw) ? toShare(c.d, raw) : null;
      })
      .filter((v) => v != null);
    return vals.length ? vals.reduce((s, v) => s + v, 0) / vals.length : 0;
  });
  const shown = $derived(wanted ?? average);

  const ICON = { light: 'light', plug: 'socket', cover: 'shutter', climate: 'air-conditioner', tv: 'tv', speaker: 'speaker' };
  const opens = (c) => c.kind === 'climate' || c.kind === 'cover';

  function climateTemp(d) {
    const v = value(d.id, climateKeys(d).temperature);
    return reads(v) ? `${num(v, 1)}°` : null;
  }

  async function tap(c) {
    if (c.d.online === false) return;
    if (opens(c)) return onopen(c.id);
    if (!canWrite(c.d, powerKey(c.d))) return;
    busy = { ...busy, [c.id]: true };
    try {
      await toggle(c.id, t(isOn(c.id) ? 'maison.scene.eteindre' : 'maison.scene.allumer', { name: c.name }));
    } finally {
      const { [c.id]: _, ...rest } = busy;
      busy = rest;
    }
  }

  function follow(e) {
    const r = e.currentTarget.getBoundingClientRect();
    wanted = Math.round(Math.max(1, Math.min(100, ((r.bottom - e.clientY) / r.height) * 100)));
    clearTimeout(wantedTimer);
    wantedTimer = setTimeout(() => (wanted = null), 10_000);
  }
  function down(e) {
    e.currentTarget.setPointerCapture?.(e.pointerId);
    follow(e);
  }
  function move(e) {
    if (e.currentTarget.hasPointerCapture?.(e.pointerId)) follow(e);
  }
  async function send() {
    if (wanted == null) return;
    const v = wanted;
    for (const c of dimmable) await act(`${c.id}/brightness`, fromShare(c.d, v), t('maison.scene.luminosite'));
  }
  function up(e) {
    e.currentTarget.releasePointerCapture?.(e.pointerId);
    send();
  }
  let keyTimer;
  function key(e) {
    if (!dimmable.length) return;
    const step = e.key === 'ArrowUp' || e.key === 'ArrowRight' ? 10 : e.key === 'ArrowDown' || e.key === 'ArrowLeft' ? -10 : 0;
    if (!step) return;
    e.preventDefault();
    wanted = Math.max(1, Math.min(100, Math.round(shown) + step));
    clearTimeout(wantedTimer);
    wantedTimer = setTimeout(() => (wanted = null), 10_000);
    clearTimeout(keyTimer);
    keyTimer = setTimeout(send, 900);
  }

  async function allPower() {
    const on = !anyOn;
    const label = t(on ? 'maison.scene.tout_allumer' : 'maison.scene.tout_eteindre');
    for (const c of writable) await act(`${c.id}/${powerKey(c.d)}`, on, label);
  }

  function reading(keys) {
    const r = room ?? {};
    for (const d of [...(r.sensors ?? []), ...(r.climates ?? [])]) {
      for (const k of keys) {
        const v = d?.id ? value(d.id, k) : null;
        if (reads(v)) return num(v, k === 'humidity' ? 0 : 1);
      }
    }
    return null;
  }
  const temp = $derived(reading(['temperature', 'temp_current']));
  const hum = $derived(reading(['humidity']));

  async function shrink(file) {
    let img;
    try {
      img = await createImageBitmap(file);
    } catch {
      img = await new Promise((ok, ko) => {
        const el = new Image();
        const url = URL.createObjectURL(file);
        el.onload = () => (URL.revokeObjectURL(url), ok(el));
        el.onerror = (err) => (URL.revokeObjectURL(url), ko(err));
        el.src = url;
      });
    }
    const w0 = img.width || img.naturalWidth;
    const h0 = img.height || img.naturalHeight;
    const k = Math.min(1, 1600 / Math.max(w0, h0));
    const canvas = document.createElement('canvas');
    canvas.width = Math.round(w0 * k);
    canvas.height = Math.round(h0 * k);
    canvas.getContext('2d').drawImage(img, 0, 0, canvas.width, canvas.height);
    const blob = await new Promise((ok) => canvas.toBlob(ok, 'image/jpeg', 0.82));
    if (!blob) throw new Error('encode');
    return new File([blob], 'piece.jpg', { type: 'image/jpeg' });
  }

  async function chosen(e) {
    const input = e.currentTarget;
    const file = input.files?.[0];
    input.value = '';
    if (!file) return;
    sending = true;
    failed = false;
    clearTimeout(errorTimer);
    try {
      await onphoto(await shrink(file));
    } catch {
      failed = true;
      errorTimer = setTimeout(() => (failed = false), 4000);
    } finally {
      sending = false;
    }
  }
</script>

<div class="scene" bind:this={sceneEl}>
  {#if photo}
    <img class="photo" src={photo} alt={room?.name ?? ''} />
    <div class="shade"></div>
  {:else}
    <div class="fallback"><span class="ghost"><Icon name="home" size={220} /></span></div>
  {/if}

  {#if onShare > 0}
    <div class="glow" style:opacity={onShare * 0.9}></div>
  {/if}

  <svg class="lines" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
    {#each placed as c (c.id)}
      <line x1={CX} y1={CY} x2={c.x} y2={c.y} class:lit={isOn(c.id)} vector-effect="non-scaling-stroke" />
    {/each}
  </svg>

  <div class="hub" aria-hidden="true" style:left="{CX}%" style:top="{CY}%">
    <span class="ring"></span>
    <span class="core"><Icon name="home" size={28} /></span>
  </div>

  {#each placed as c, i (c.id)}
    <div class="control" style:left="{c.x}%" style:top="{c.y}%" style:animation-delay="{i * 60}ms">
      <button
        class="orb"
        class:on={!opens(c) && isOn(c.id)}
        class:busy={busy[c.id]}
        disabled={c.d.online === false}
        aria-pressed={opens(c) ? undefined : isOn(c.id)}
        aria-label={c.name}
        title={c.name}
        onclick={() => tap(c)}
      >
        <Icon name={ICON[c.kind] ?? 'power'} size={22} />
        {#if c.kind === 'climate' && climateTemp(c.d)}<span class="badge num">{climateTemp(c.d)}</span>{/if}
      </button>
      <span class="label">{c.name}</span>
    </div>
  {/each}

  {#if dimmable.length || writable.length}
    <div class="dimmer">
      {#if dimmable.length}
        <div class="slider">
          <Icon name="sun" size={18} />
          <div
            class="track"
            role="slider"
            tabindex="0"
            aria-label={t('maison.scene.luminosite')}
            aria-valuemin="0"
            aria-valuemax="100"
            aria-valuenow={Math.round(shown)}
            onpointerdown={down}
            onpointermove={move}
            onpointerup={up}
            onpointercancel={up}
            onkeydown={key}
          >
            <div class="fill" style:transform="scaleY({shown / 100})"></div>
          </div>
          <span class="pct num">{Math.round(shown)} %</span>
        </div>
      {/if}
      {#if writable.length}
        <button class="power" class:on={anyOn} aria-label={t(anyOn ? 'maison.scene.tout_eteindre' : 'maison.scene.tout_allumer')} title={t(anyOn ? 'maison.scene.tout_eteindre' : 'maison.scene.tout_allumer')} onclick={allPower}>
          <Icon name="power" size={20} />
        </button>
      {/if}
    </div>
  {/if}

  <div class="pills left">
    {#if temp != null}<span class="pill"><Icon name="thermometer" size={16} /><b class="num">{temp}°</b></span>{/if}
    {#if hum != null}<span class="pill"><Icon name="humidity" size={16} /><b class="num">{hum} %</b></span>{/if}
  </div>

  <div class="pills right">
    {#if photo}
      <button class="pill tap" onclick={() => fileInput?.click()}><Icon name="camera" size={16} />{t('maison.scene.changer_photo')}</button>
    {/if}
    <button class="pill tap" onclick={() => sceneEl?.requestFullscreen?.()} aria-label={t('maison.scene.plein_ecran')} title={t('maison.scene.plein_ecran')}>
      <Icon name="arrow-top-right" size={16} />
    </button>
  </div>

  {#if !photo}
    <button class="cta" onclick={() => fileInput?.click()}><Icon name="camera" size={20} />{t('maison.scene.ajouter_photo')}</button>
  {/if}

  {#if sending}<div class="toast" role="status">{t('maison.scene.envoi')}</div>{/if}
  {#if failed}<div class="toast bad" role="alert">{t('maison.scene.erreur_photo')}</div>{/if}

  <input type="file" accept="image/*" bind:this={fileInput} onchange={chosen} hidden />
</div>

<style>
  .scene {
    position: relative;
    aspect-ratio: 16 / 10;
    min-height: 360px;
    max-height: 70vh;
    width: 100%;
    border-radius: 28px;
    overflow: hidden;
    isolation: isolate;
    box-shadow: 0 30px 60px -30px rgb(0 0 0 / 50%);
    color: #fff;
    background: #1b1f2a;
  }

  .scene:fullscreen {
    border-radius: 0;
    max-height: none;
    aspect-ratio: auto;
    width: 100vw;
    height: 100vh;
  }

  .photo {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: cover;
  }

  .shade {
    position: absolute;
    inset: 0;
    background: linear-gradient(to bottom, rgb(0 0 0 / 15%), transparent 35%, rgb(0 0 0 / 55%));
    pointer-events: none;
  }

  .fallback {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    background:
      radial-gradient(60% 50% at 30% 30%, rgb(255 170 80 / 22%), transparent 70%),
      linear-gradient(160deg, #2b2118, #4a3524 55%, #1b1f2a);
  }

  .ghost {
    display: flex;
    opacity: 0.07;
  }

  .glow {
    position: absolute;
    inset: 0;
    pointer-events: none;
    background: radial-gradient(ellipse at 50% 40%, rgb(255 170 70 / 45%), transparent 70%);
    mix-blend-mode: soft-light;
    transition: opacity 0.4s ease;
  }

  .lines {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    pointer-events: none;
  }

  .lines line {
    stroke: rgb(255 255 255 / 55%);
    stroke-width: 1.5;
    stroke-dasharray: 3 3;
  }

  .lines line.lit {
    stroke: #ffc46b;
    stroke-dasharray: none;
    filter: drop-shadow(0 0 3px rgb(255 196 107 / 75%));
  }

  .hub {
    position: absolute;
    left: 50%;
    top: 50%;
    transform: translate(-50%, -50%);
    display: grid;
    place-items: center;
    pointer-events: none;
  }

  .core,
  .ring {
    grid-area: 1 / 1;
    width: 64px;
    height: 64px;
    border-radius: 50%;
  }

  .core {
    display: grid;
    place-items: center;
    background: rgb(20 24 32 / 55%);
    backdrop-filter: blur(12px) saturate(140%);
    -webkit-backdrop-filter: blur(12px) saturate(140%);
    border: 1px solid rgb(255 255 255 / 22%);
  }

  .ring {
    border: 2px solid rgb(255 200 120 / 50%);
    animation: pulse 2.6s ease-out infinite;
  }

  @keyframes pulse {
    0% { transform: scale(1); opacity: 0.7; }
    70%, 100% { transform: scale(1.7); opacity: 0; }
  }

  .control {
    position: absolute;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
    transform: translate(-50%, -32px);
    animation: pop 0.42s cubic-bezier(0.22, 1, 0.36, 1) both;
  }

  @keyframes pop {
    from { opacity: 0; scale: 0.6; }
  }

  .orb {
    position: relative;
    width: 58px;
    height: 58px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    padding: 0;
    background: rgb(20 24 32 / 45%);
    backdrop-filter: blur(12px) saturate(140%);
    -webkit-backdrop-filter: blur(12px) saturate(140%);
    border: 1px solid rgb(255 255 255 / 22%);
    color: #fff;
    transition: transform 0.16s ease, background 0.2s ease, box-shadow 0.25s ease;
  }

  .orb:hover:not(:disabled) { transform: scale(1.08); }
  .orb:active:not(:disabled) { transform: scale(0.94); }

  .orb.on {
    background: linear-gradient(135deg, #ffbf5c, #f08a3a);
    color: #3a2105;
    border-color: rgb(255 255 255 / 30%);
    box-shadow: 0 0 0 6px rgb(255 180 80 / 25%), 0 10px 30px rgb(240 138 58 / 55%);
  }

  .orb:disabled { opacity: 0.45; }
  .orb.busy { opacity: 0.7; }

  .orb.busy::after {
    content: '';
    position: absolute;
    inset: -4px;
    border-radius: 50%;
    border: 2px solid rgb(255 255 255 / 70%);
    border-top-color: transparent;
    animation: spin 0.8s linear infinite;
  }

  @keyframes spin { to { transform: rotate(360deg); } }

  .badge {
    position: absolute;
    top: -6px;
    right: -10px;
    padding: 2px 6px;
    border-radius: 999px;
    background: rgb(10 12 18 / 85%);
    border: 1px solid rgb(255 255 255 / 20%);
    color: #ffdca8;
    font-size: 11px;
    font-weight: 700;
    line-height: 1.2;
  }

  .label {
    max-width: 14ch;
    padding: 3px 9px;
    border-radius: 999px;
    background: rgb(0 0 0 / 45%);
    backdrop-filter: blur(6px);
    -webkit-backdrop-filter: blur(6px);
    font-size: 12px;
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .dimmer {
    position: absolute;
    right: 18px;
    top: 50%;
    transform: translateY(-50%);
    display: grid;
    justify-items: center;
    gap: 12px;
  }

  .slider {
    display: grid;
    justify-items: center;
    gap: 8px;
    padding: 12px 8px;
    border-radius: 999px;
    background: rgb(20 24 32 / 42%);
    backdrop-filter: blur(12px) saturate(140%);
    -webkit-backdrop-filter: blur(12px) saturate(140%);
    border: 1px solid rgb(255 255 255 / 22%);
    color: #ffd9a0;
  }

  .track {
    position: relative;
    width: 44px;
    height: 200px;
    border-radius: 999px;
    background: rgb(0 0 0 / 45%);
    overflow: hidden;
    cursor: ns-resize;
    touch-action: none;
  }

  .fill {
    position: absolute;
    left: 0;
    right: 0;
    top: 0;
    bottom: 0;
    background: linear-gradient(to top, #f08a3a, #ffcf7a);
    transform-origin: bottom;
    transition: transform 0.12s ease;
  }

  .pct {
    font-size: 11px;
    font-weight: 700;
    color: #ffe7c8;
  }

  .power {
    width: 44px;
    height: 44px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    padding: 0;
    background: rgb(20 24 32 / 45%);
    backdrop-filter: blur(12px) saturate(140%);
    -webkit-backdrop-filter: blur(12px) saturate(140%);
    border: 1px solid rgb(255 255 255 / 22%);
    color: #fff;
  }

  .power.on {
    background: linear-gradient(135deg, #ffbf5c, #f08a3a);
    color: #3a2105;
  }

  .pills {
    position: absolute;
    bottom: 16px;
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    max-width: 48%;
  }

  .pills.left { left: 16px; }
  .pills.right { right: 16px; justify-content: flex-end; }

  .pill {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 8px 13px;
    border-radius: 999px;
    background: rgb(0 0 0 / 45%);
    backdrop-filter: blur(10px) saturate(140%);
    -webkit-backdrop-filter: blur(10px) saturate(140%);
    border: 1px solid rgb(255 255 255 / 20%);
    color: #fff;
    font-size: 13px;
    font-weight: 600;
    white-space: nowrap;
  }

  .pill.tap { transition: transform 0.14s ease, background 0.2s ease; }
  .pill.tap:hover { background: rgb(20 24 32 / 60%); }
  .pill.tap:active { transform: scale(0.96); }

  .cta {
    position: absolute;
    left: 50%;
    top: 18px;
    transform: translateX(-50%);
    display: inline-flex;
    align-items: center;
    gap: 10px;
    padding: 14px 22px;
    border-radius: 999px;
    background: rgb(20 24 32 / 55%);
    backdrop-filter: blur(14px) saturate(140%);
    -webkit-backdrop-filter: blur(14px) saturate(140%);
    border: 1px solid rgb(255 255 255 / 28%);
    box-shadow: 0 10px 30px rgb(0 0 0 / 35%);
    color: #fff;
    font-size: 15px;
    font-weight: 700;
  }

  .toast {
    position: absolute;
    left: 50%;
    top: 76px;
    transform: translateX(-50%);
    padding: 10px 20px;
    border-radius: 999px;
    background: rgb(20 24 32 / 80%);
    border: 1px solid rgb(255 255 255 / 22%);
    font-size: 13px;
    font-weight: 600;
    pointer-events: none;
  }

  .toast.bad { background: rgb(120 30 30 / 85%); }

  @media (max-width: 760px) {
    .scene { aspect-ratio: 3 / 4; min-height: 420px; border-radius: 22px; }
    .orb { width: 44px; height: 44px; }
    .control { transform: translate(-50%, -24px); gap: 4px; }
    .label { max-width: 10ch; font-size: 11px; padding: 2px 7px; }
    .core, .ring { width: 50px; height: 50px; }
    .slider { padding: 8px 6px; gap: 6px; }
    .track { width: 28px; height: 110px; }
    .power { width: 36px; height: 36px; }
    .dimmer { right: 8px; gap: 8px; }
    .pills { bottom: 12px; max-width: 70%; }
    .pills.left { left: 12px; }
    .pills.right { right: 12px; }
    .pill { padding: 6px 10px; font-size: 12px; }
    .cta { top: 12px; padding: 10px 16px; font-size: 13px; white-space: nowrap; }
  }

  @media (prefers-reduced-motion: reduce) {
    .ring, .control, .orb.busy::after { animation: none; }
    .ring { opacity: 0; }
    .orb, .fill, .glow, .pill.tap { transition: none; }
  }
</style>
