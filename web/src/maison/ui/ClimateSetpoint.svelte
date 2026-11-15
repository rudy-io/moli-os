<script>
  import { onDestroy } from 'svelte';
  import { device, value, reachable, act } from '../lib/home.svelte.js';
  import { pointOf, canWrite, numericRange, quantize } from '../lib/capabilities.js';
  import { completionGuard } from '../lib/completion-guard.js';
  import { t } from '../../lib/i18n.svelte.js';

  let { id, pointKey, label, name } = $props();
  const d = $derived(device(id));
  const unit = $derived(pointOf(d, pointKey)?.unit);
  const range = $derived(numericRange(d, pointKey));
  const target = $derived(value(id, pointKey));
  const writable = $derived(!!range && canWrite(d, pointKey) && reachable(id));
  let wanted = $state(null);
  const shown = $derived(wanted ?? target);
  const completion = completionGuard();
  let timer;
  let resetTimer;
  function cancel() {
    completion.change();
    clearTimeout(timer);
    clearTimeout(resetTimer);
    wanted = null;
  }
  onDestroy(cancel);
  $effect(() => { void id; void pointKey; void unit; cancel(); });
  $effect(() => { if (target === wanted) wanted = null; });

  function edit(next) {
    if (!writable || !Number.isFinite(next)) return;
    completion.change();
    wanted = quantize(next, range);
    clearTimeout(timer);
    const forId = id, forKey = pointKey, forUnit = unit;
    timer = setTimeout(() => {
      if (id === forId && pointKey === forKey && unit === forUnit) flush();
      else cancel();
    }, 900);
  }
  function nudge(delta) {
    if (Number.isFinite(shown)) edit(shown + delta);
  }
  async function flush() {
    clearTimeout(timer);
    const v = wanted;
    if (!writable || v == null || v === target || quantize(v, range) !== v) { wanted = null; return; }
    const owns = completion.capture();
    const done = await act(`${id}/${pointKey}`, v, t('commun.clim.label_consigne', { name: `${name} · ${label}` }));
    if (!owns()) return;
    if (done !== 'ok') wanted = null;
    else resetTimer = setTimeout(() => owns() && wanted === v && (wanted = null), 15_000);
  }
</script>

<div class="setpoint" role="group" aria-label={label}>
  <label>
    {label}
    <input type="number" aria-label={label} value={Number.isFinite(shown) ? shown : ''}
      min={range?.first} max={range?.last} step={range?.step}
      disabled={!writable} oninput={(e) => edit(e.currentTarget.valueAsNumber)} />
    {unit ?? ''}
  </label>
  <button aria-label={t('commun.clim.moins_chaud')} disabled={!writable || !Number.isFinite(shown) || shown <= range.first} onclick={() => nudge(-range.step)}>−</button>
  <button aria-label={t('commun.clim.plus_chaud')} disabled={!writable || !Number.isFinite(shown) || shown >= range.last} onclick={() => nudge(range.step)}>+</button>
</div>

<style>
  .setpoint { display: flex; align-items: center; gap: 8px; }
  input { width: 5em; }
  button { font: inherit; border-radius: 50%; width: 32px; height: 32px; }
  button:disabled { opacity: 0.35; }
</style>
