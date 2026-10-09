<script>
  import { hub, command } from '../lib/hub.svelte.js';
  import { formatValue, unitOf, isAlarm } from '../lib/format.js';
  import { t, locale } from '../lib/i18n.svelte.js';

  let { device, point, big = false } = $props();

  const id = $derived(`${device.id}/${point.key}`);
  const sample = $derived(device.state[point.key]);
  const value = $derived(sample?.value);
  const pending = $derived(id in hub.pending);
  const flash = $derived(hub.changed[id]);
  const writable = $derived(point.access.write);
  const kind = $derived(point.kind.type);

  function submitNumber(e) {
    const n = Number(e.currentTarget.value);
    if (Number.isFinite(n) && n !== value) command(id, n);
  }
</script>

<div class="point" class:big class:pending>
  <span class="label">{point.label}</span>

  {#if writable && kind === 'binary'}
    <button
      class="switch"
      role="switch"
      aria-checked={value === true}
      aria-label={point.label}
      disabled={pending}
      onclick={() => command(id, !(value === true))}
    >
      <span class="knob"></span>
    </button>
  {:else if writable && kind === 'enum'}
    <select
      value={value ?? ''}
      disabled={pending}
      aria-label={point.label}
      onchange={(e) => command(id, e.currentTarget.value)}
    >
      {#if value == null}<option value="" disabled>—</option>{/if}
      {#each point.kind.values as v (v)}
        <option value={v}>{v}</option>
      {/each}
    </select>
  {:else if writable && kind === 'numeric' && point.semantic === 'brightness'}
    <span class="numeric">
      <input
        type="range"
        class="slider"
        min={point.kind.min ?? 0}
        max={point.kind.max ?? 100}
        step="any"
        value={value ?? 0}
        disabled={pending}
        aria-label={point.label}
        onchange={submitNumber}
      />
      <span class="unit num">{value == null ? '—' : Math.round(value)}%</span>
    </span>
  {:else if writable && kind === 'numeric'}
    <span class="numeric">
      <input
        type="number"
        class="num"
        value={value ?? ''}
        min={point.kind.min}
        max={point.kind.max}
        step={point.kind.step ?? 'any'}
        disabled={pending}
        aria-label={point.label}
        onchange={submitNumber}
      />
      <span class="unit">{unitOf(point)}</span>
    </span>
  {:else}
    {#key flash}
      <button
        class="value num"
        class:alarm={isAlarm(point, value)}
        class:flash
        onclick={() => (hub.chart = id)}
        title={sample ? t('systeme.point.releve', { date: new Date(sample.ts).toLocaleString(locale()) }) : t('systeme.point.pas_de_releve')}
      >
        {formatValue(point, value)}{#if unitOf(point) && value != null}<span class="unit">{unitOf(point)}</span>{/if}
      </button>
    {/key}
  {/if}
</div>

<style>
  .point {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    min-height: 32px;
  }
  .label {
    color: var(--ink-2);
    font-size: 13.5px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .value {
    font: inherit;
    font-weight: 700;
    color: inherit;
    background: none;
    border: 0;
    border-radius: 3px;
    padding: 1px 4px;
    margin-right: -4px;
    white-space: nowrap;
    cursor: pointer;
  }
  .value:hover {
    background: color-mix(in srgb, var(--sun) 25%, transparent);
  }
  .big .value {
    font-size: 22px;
    letter-spacing: -0.01em;
  }
  .value.flash {
    animation: flash 1.2s ease-out;
  }
  .unit {
    margin-left: 3px;
    font-size: 0.72em;
    font-weight: 600;
    color: var(--ink-3);
  }
  .alarm {
    color: #fff;
    background: var(--alarm);
  }
  .pending {
    opacity: 0.6;
  }
  .switch {
    position: relative;
    width: 42px;
    height: 24px;
    flex: none;
    border: var(--stroke) solid var(--ink);
    border-radius: 999px;
    background: var(--card);
    cursor: pointer;
    padding: 0;
    transition: background 0.15s;
  }
  .switch[aria-checked='true'] {
    background: var(--sun);
  }
  .knob {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    background: var(--ink);
    transition: transform 0.15s;
  }
  .switch[aria-checked='true'] .knob {
    transform: translateX(18px);
  }
  .switch:focus-visible,
  select:focus-visible,
  input:focus-visible {
    outline: 2px solid var(--copper);
    outline-offset: 2px;
  }
  select,
  input {
    border: var(--stroke) solid var(--line);
    border-radius: var(--radius);
    background: var(--card);
    padding: 3px 6px;
    max-width: 150px;
  }
  input {
    width: 84px;
    text-align: right;
  }
  .numeric {
    display: flex;
    align-items: baseline;
  }
  .slider {
    width: 110px;
    padding: 0;
    border: 0;
    accent-color: var(--sun);
  }
  .slider + .unit {
    min-width: 36px;
    text-align: right;
  }
</style>
