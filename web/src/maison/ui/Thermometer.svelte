<script>
  import { onDestroy } from 'svelte';
  import { device, value, reachable, num, act } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import { pointOf, canWrite, numericRange, quantize, climateKeys, isValve } from '../lib/capabilities.js';

  /** A drawn thermometer: the mercury is the room's temperature, the bead
   *  the setpoint, moved with the finger (up warms, down cools). One order
   *  leaves when the finger lifts, through the house's usual path (act). */
  let { id, name } = $props();
  const uid = $props.id();

  const d = $derived(device(id));
  const keys = $derived(climateKeys(d));
  const unit = $derived(pointOf(d, keys.target)?.unit);
  const range = $derived(numericRange(d, keys.target, 0.5, !isValve(d) && (unit == null || unit === '°C') ? { min: 16, max: 30 } : null));
  const temp = $derived(value(id, keys.temperature));
  const target = $derived(value(id, keys.target));
  const writable = $derived(!!range && canWrite(d, keys.target) && reachable(id));

  let wanted = $state(null);
  let dragging = $state(false);
  let svgEl = $state(null);
  const shown = $derived(wanted ?? target);

  const lo = $derived.by(() => {
    let l = range ? range.first - 2 : 10;
    if (typeof temp === 'number' && temp < l) l = Math.floor(temp) - 2;
    return l;
  });
  const hi = $derived.by(() => {
    let h = range ? range.last + 2 : 32;
    if (typeof temp === 'number' && temp > h) h = Math.ceil(temp) + 2;
    return h;
  });

  const TOP = 20;
  const BOTTOM = 230;
  const H = 210;

  function yFor(v) {
    if (typeof v !== 'number' || !Number.isFinite(v)) return BOTTOM;
    const c = Math.max(lo, Math.min(hi, v));
    return BOTTOM - ((c - lo) / (hi - lo)) * H;
  }

  const mercuryY = $derived(yFor(temp));
  const cursorY = $derived(typeof shown === 'number' ? yFor(shown) : null);

  const trend = $derived.by(() => {
    if (typeof shown !== 'number' || typeof temp !== 'number') return 'stable';
    return shown > temp + 0.3 ? 'chauffe' : shown < temp - 0.3 ? 'refroidit' : 'stable';
  });
  const TREND = {
    chauffe: { color: '#df5a40', arrow: '↑', key: 'commun.thermo.chauffe' },
    refroidit: { color: '#4b74f2', arrow: '↓', key: 'commun.thermo.refroidit' },
    stable: { color: 'var(--ink-3)', arrow: '=', key: 'commun.thermo.stable' },
  };
  const look = $derived(TREND[trend]);

  const grads = $derived.by(() => {
    const out = [];
    for (let g = Math.ceil(lo / 2) * 2; g <= Math.floor(hi / 2) * 2; g += 2) out.push({ v: g, y: yFor(g) });
    return out;
  });

  let sendTimer = null;
  let wantedTimer = null;

  function valueAt(e) {
    if (!range || !svgEl) return null;
    const r = svgEl.getBoundingClientRect();
    const y = (e.clientY - r.top) * (300 / r.height);
    const q = quantize(lo + ((BOTTOM - y) / H) * (hi - lo), range);
    return q == null ? null : Math.max(range.first, Math.min(range.last, q));
  }

  function follow(e) {
    const v = valueAt(e);
    if (v != null) wanted = v;
  }
  function down(e) {
    if (!writable) return;
    svgEl.setPointerCapture(e.pointerId);
    dragging = true;
    follow(e);
  }
  function move(e) {
    if (dragging) follow(e);
  }
  function up() {
    if (!dragging) return;
    dragging = false;
    send();
  }

  function key(e) {
    if (!writable || !range) return;
    const base = wanted ?? target;
    if (typeof base !== 'number') return;
    let next;
    if (e.key === 'ArrowUp' || e.key === 'ArrowRight') next = base + range.step;
    else if (e.key === 'ArrowDown' || e.key === 'ArrowLeft') next = base - range.step;
    else return;
    e.preventDefault();
    const q = quantize(next, range);
    wanted = Math.max(range.first, Math.min(range.last, q ?? next));
    clearTimeout(sendTimer);
    sendTimer = setTimeout(() => {
      sendTimer = null;
      send();
    }, 900);
  }

  async function send() {
    if (wanted == null || wanted === target) {
      wanted = null;
      return;
    }
    const sent = wanted;
    const done = await act(`${id}/${keys.target}`, sent, t('commun.clim.label_consigne', { name }));
    // A unit that is off starts on the new setpoint (as on the dial).
    if (done === 'ok' && !isValve(d) && canWrite(d, 'on') && value(id, 'on') === false) await act(`${id}/on`, true, t('commun.clim.label_marche', { name }));
    if (done !== 'ok') {
      wanted = null;
      return;
    }
    clearTimeout(wantedTimer);
    wantedTimer = setTimeout(() => {
      if (wanted === sent) wanted = null;
    }, 15000);
  }

  onDestroy(() => {
    clearTimeout(sendTimer);
    clearTimeout(wantedTimer);
  });
</script>

<div class="thermo">
  <svg
    bind:this={svgEl}
    class="tube"
    class:writable
    viewBox="0 0 120 300"
    tabindex={writable ? 0 : -1}
    role="slider"
    aria-label={t('commun.thermo.glisser', { name })}
    aria-valuemin={range?.first}
    aria-valuemax={range?.last}
    aria-valuenow={typeof shown === 'number' ? shown : undefined}
    aria-valuetext={typeof shown === 'number' ? `${num(shown, 1)}°` : undefined}
    aria-readonly={!writable}
    onpointerdown={down}
    onpointermove={move}
    onpointerup={up}
    onpointercancel={up}
    onkeydown={key}
  >
    <defs>
      <linearGradient id={`g${uid}`} gradientUnits="userSpaceOnUse" x1="0" y1={BOTTOM} x2="0" y2={TOP}>
        <stop offset="0" stop-color="#4b74f2" />
        <stop offset="0.5" stop-color="#f0a53a" />
        <stop offset="1" stop-color="#df5a40" />
      </linearGradient>
      <clipPath id={`c${uid}`}>
        <rect x="45" y="20" width="30" height="210" rx="15" />
        <circle cx="60" cy="255" r="26" />
      </clipPath>
      <filter id={`s${uid}`} x="-50%" y="-50%" width="200%" height="200%">
        <feDropShadow dx="0" dy="1" stdDeviation="2" flood-opacity="0.3" />
      </filter>
    </defs>

    <rect x="45" y="20" width="30" height="210" rx="15" fill="var(--surface-3)" stroke="color-mix(in srgb, var(--ink) 12%, transparent)" stroke-width="2" />
    <circle cx="60" cy="255" r="26" fill="var(--surface-3)" stroke="color-mix(in srgb, var(--ink) 12%, transparent)" stroke-width="2" />

    {#if typeof temp === 'number'}
      <g clip-path={`url(#c${uid})`}>
        <rect class="merc" x="45" width="30" style:y="{mercuryY}px" style:height="{BOTTOM - mercuryY + 30}px" fill={`url(#g${uid})`} />
        <circle cx="60" cy="255" r="26" fill={`url(#g${uid})`} />
      </g>
      <!-- A glint along the glass. -->
      <rect x="51" y="28" width="5" height="190" rx="2.5" fill="rgb(255 255 255 / 35%)" pointer-events="none" />
    {/if}

    {#each grads as g (g.v)}
      <line x1="36" x2="42" y1={g.y} y2={g.y} stroke="var(--ink-3)" stroke-width="1" />
      <text x="32" y={g.y + 3} text-anchor="end" font-size="9" fill="var(--ink-3)">{g.v}</text>
    {/each}

    {#if cursorY != null}
      <g class="bead" style:transform="translateY({cursorY}px)">
        <circle cx="60" cy="0" r="11" fill="#fff" stroke={look.color} stroke-width="4" filter={`url(#s${uid})`} />
        <text x="76" y="5" font-size="15" font-weight="700" fill={look.color}>{num(shown, 1)}°</text>
      </g>
    {/if}
  </svg>

  <div class="trend" style:color={look.color}>
    <span aria-hidden="true">{look.arrow}</span>
    <span>{t(look.key)}</span>
  </div>
  {#if typeof temp === 'number'}
    <div class="measured">{t('commun.thermo.mesure', { temp: num(temp, 1) })}</div>
  {/if}
</div>

<style>
  .thermo {
    display: grid;
    justify-items: center;
    gap: 6px;
  }

  .tube {
    width: 120px;
    height: 300px;
    touch-action: none;
    overflow: visible;
  }

  .tube.writable {
    cursor: ns-resize;
  }

  .merc {
    transition:
      y 0.8s cubic-bezier(0.22, 1, 0.36, 1),
      height 0.8s cubic-bezier(0.22, 1, 0.36, 1);
  }

  .bead {
    transition: transform 0.15s ease-out;
  }

  .trend {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 15px;
    font-weight: 700;
  }

  .measured {
    font-size: 12.5px;
    color: var(--ink-3);
  }

  @media (prefers-reduced-motion: reduce) {
    .merc,
    .bead {
      transition: none;
    }
  }
</style>
