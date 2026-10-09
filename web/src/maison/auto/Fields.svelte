<script>
  // Small editors shared by the inspector: a value for a point, days of the
  // week, a duration, a time.
  import { pointSpec, valueText, dayInitials } from '../lib/auto.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';

  /** kind: value | days | duration | time */
  let { kind, point = '', value = null, onchange } = $props();

  const spec = $derived(kind === 'value' ? pointSpec(point) : null);
  const type = $derived(spec?.kind?.type);

  // Durations: the biggest unit that reads well.
  const unit = $derived(kind === 'duration' ? (value >= 3600 && value % 3600 === 0 ? 3600 : value >= 60 && value % 60 === 0 ? 60 : 1) : 1);
</script>

{#if kind === 'value'}
  {#if type === 'binary'}
    <div class="seg">
      {#each [true, false] as v (v)}
        <button class:on={value === v} onclick={() => onchange?.(v)}>{valueText(point, v)}</button>
      {/each}
    </div>
  {:else if type === 'enum'}
    {@const values = spec.kind.values ?? []}
    {#if values.length <= 4}
      <div class="seg">
        {#each values as v (v)}
          <button class:on={value === v} onclick={() => onchange?.(v)}>{v}</button>
        {/each}
      </div>
    {:else}
      <select value={value ?? ''} onchange={(e) => onchange?.(e.currentTarget.value)} aria-label={t('automatismes.champ.valeur')}>
        {#each values as v (v)}<option value={v}>{v}</option>{/each}
      </select>
    {/if}
  {:else if type === 'numeric'}
    <label class="num">
      <input
        type="number"
        value={value ?? ''}
        min={spec.kind.min}
        max={spec.kind.max}
        step={spec.kind.step ?? 'any'}
        onchange={(e) => onchange?.(e.currentTarget.value === '' ? null : Number(e.currentTarget.value))} />
      {#if spec.unit}<span>{spec.unit}</span>{/if}
    </label>
  {:else}
    <input class="text" value={value ?? ''} onchange={(e) => onchange?.(e.currentTarget.value)} aria-label={t('automatismes.champ.valeur')} />
  {/if}
{:else if kind === 'days'}
  <div class="days" role="group" aria-label={t('automatismes.champ.jours')}>
    {#each dayInitials() as d, i (i)}
      {@const n = i + 1}
      <button
        class:on={(value ?? []).includes(n)}
        onclick={() => {
          const set = new Set(value ?? []);
          if (set.has(n)) set.delete(n);
          else set.add(n);
          onchange?.([...set].sort());
        }}>{d}</button>
    {/each}
    <small class="muted">{(value ?? []).length ? '' : t('automatismes.champ.tous_les_jours')}</small>
  </div>
{:else if kind === 'duration'}
  <div class="num">
    <input type="number" min="1" value={Math.round((value ?? 0) / unit)} onchange={(e) => onchange?.(Math.max(1, Number(e.currentTarget.value) || 1) * unit)} aria-label={t('automatismes.champ.duree')} />
    <select value={unit} onchange={(e) => onchange?.(Math.max(1, Math.round((value ?? 0) / unit)) * Number(e.currentTarget.value))} aria-label={t('automatismes.champ.unite')}>
      <option value={1}>{t('automatismes.champ.secondes')}</option>
      <option value={60}>{t('automatismes.champ.minutes')}</option>
      <option value={3600}>{t('automatismes.champ.heures')}</option>
    </select>
  </div>
{:else if kind === 'time'}
  <input class="time" type="time" value={value ?? ''} onchange={(e) => onchange?.(e.currentTarget.value || null)} aria-label={t('automatismes.champ.heure')} />
{/if}

<style>
  .seg {
    display: flex;
    gap: 4px;
    padding: 4px;
    border-radius: 14px;
    background: var(--surface-2);
    flex-wrap: wrap;
  }

  .seg button {
    flex: 1;
    border: 0;
    border-radius: 10px;
    padding: 9px 12px;
    background: none;
    color: var(--ink-2);
    font-weight: 650;
  }

  .seg button.on {
    background: var(--surface);
    color: var(--ink);
    box-shadow: var(--shadow);
  }

  select,
  input {
    border: 0;
    border-radius: 12px;
    padding: 10px 12px;
    background: var(--surface-2);
    font: inherit;
    color: var(--ink);
  }

  .num {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .num input {
    width: 110px;
  }

  .text {
    width: 100%;
  }

  .days {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }

  .days button {
    width: 38px;
    height: 38px;
    border-radius: 50%;
    border: 0;
    background: var(--surface-2);
    color: var(--ink-2);
    font-weight: 700;
  }

  .days button.on {
    background: var(--ink);
    color: var(--bg);
  }

  .time {
    width: 140px;
  }
</style>
