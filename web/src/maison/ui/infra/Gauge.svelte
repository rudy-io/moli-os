<script>
  /** A ring gauge: a value against its maximum, coloured by its thresholds
   *  ([warm, alert]: above the first it warms, above the second it alerts). */
  let { label, value = null, max = 100, unit = '%', warn = [80, 95], sub = '' } = $props();

  const R = 34;
  const C = 2 * Math.PI * R;
  const ratio = $derived(typeof value === 'number' ? Math.max(0, Math.min(1, value / max)) : 0);
  const tone = $derived(typeof value !== 'number' ? 'none' : value >= warn[1] ? 'alert' : value >= warn[0] ? 'warm' : 'good');
</script>

<div class="gauge {tone}">
  <svg viewBox="0 0 80 80" role="meter" aria-label={label} aria-valuemin="0" aria-valuemax={max} aria-valuenow={value ?? undefined}>
    <circle cx="40" cy="40" r={R} class="track" />
    <circle cx="40" cy="40" r={R} class="fill" stroke-dasharray="{C * ratio} {C}" transform="rotate(-90 40 40)" />
    <text x="40" y="44" text-anchor="middle" class="value">{typeof value === 'number' ? `${Math.round(value)}${unit}` : '—'}</text>
  </svg>
  <span class="label">{label}</span>
  {#if sub}<small>{sub}</small>{/if}
</div>

<style>
  .gauge {
    display: grid;
    justify-items: center;
    gap: 2px;
    text-align: center;
  }

  svg {
    width: 92px;
    height: 92px;
  }

  .track {
    fill: none;
    stroke: var(--surface-3);
    stroke-width: 8;
  }

  .fill {
    fill: none;
    stroke-width: 8;
    stroke-linecap: round;
    transition: stroke-dasharray 0.6s var(--ease);
  }

  .good .fill {
    stroke: var(--good);
  }

  .warm .fill {
    stroke: var(--warm);
  }

  .alert .fill {
    stroke: var(--alert);
  }

  .value {
    font-size: 17px;
    font-weight: 750;
    fill: var(--ink);
  }

  .label {
    font-size: 13px;
    font-weight: 650;
    color: var(--ink-2);
  }

  small {
    font-size: 12px;
    color: var(--ink-3);
  }
</style>
