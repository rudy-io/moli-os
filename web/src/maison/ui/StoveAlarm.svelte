<script>
  import { untrack } from 'svelte';
  import { device, value, act, clock } from '../lib/home.svelte.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  // A pellet stove's alarm (Micronova), said for the family: in alarm or
  // stored, its code and what it means, what to look at, when it came and
  // what the stove was doing then. Re-armed here only when the driver says
  // the stove takes it (`alarm_reset`), else on the stove itself.
  let { id, name } = $props();

  const mode = $derived(value(id, 'mode'));
  const code = $derived.by(() => {
    const c = value(id, 'alarm');
    return typeof c === 'string' && c !== 'OK' ? c : null;
  });
  const alarmed = $derived(mode === 'alarm' || mode === 'mem_alarm');
  const shown = $derived(alarmed || !!code);
  // A code the catalogue explains (the Micronova board's A01…A09), else the code alone.
  const known = (key) => t(key) !== key;
  const meaning = $derived(code && known(`commun.poele.code.${code}`) ? t(`commun.poele.code.${code}`) : null);
  const advice = $derived(code && known(`commun.poele.conseil.${code}`) ? t(`commun.poele.conseil.${code}`) : null);

  const point = (key) => device(id)?.points.find((p) => p.key === key);
  const canReset = $derived(!!point('alarm_reset')?.access?.write);

  // When it came, from the state's history (changes only): the start of the
  // alarm now on, and the lighting it interrupted.
  const LIGHTING = ['ignition', 'awaiting_flame', 'starting'];
  let began = $state(null);
  let lighting = $state(null);
  async function look() {
    try {
      const res = await fetch(`/api/history?point=${encodeURIComponent(`${id}/mode`)}&hours=168&points=2000`);
      if (!res.ok) return;
      const h = await res.json();
      const list = [...(h.before ? [h.before] : []), ...(h.raw ?? [])];
      let at = list.length;
      while (at > 0 && ['alarm', 'mem_alarm'].includes(list[at - 1][1])) at--;
      began = list[at]?.[0] ?? null;
      let from = at;
      while (from > 0 && LIGHTING.includes(list[from - 1][1])) from--;
      lighting = from < at ? list[from][0] : null;
    } catch {
      /* said without its time */
    }
  }
  $effect(() => {
    if (alarmed) untrack(look);
  });
  /** « dimanche 25 octobre » (« aujourd'hui ») and its time. */
  const when = (ts) => {
    const d = new Date(ts);
    const today = new Date().toDateString() === d.toDateString();
    const day = today ? t('commun.poele.alarme_aujourdhui') : d.toLocaleDateString(locale(), { weekday: 'long', day: 'numeric', month: 'long' });
    return { day, time: clock(ts) };
  };

  let asked = $state(false);
  async function reset() {
    asked = true;
    await act(`${id}/alarm_reset`, true, t('commun.poele.label_rearmer', { name }));
    setTimeout(() => (asked = false), 60_000);
  }
  $effect(() => {
    if (!alarmed) asked = false;
  });
</script>

{#if shown}
  <section class="alarm" id="alarme-{id}" aria-labelledby="alarme-{id}-titre">
    <h4 id="alarme-{id}-titre">
      <Icon name="alert" size={18} />
      {mode === 'mem_alarm' ? t('commun.poele.alarme_titre_memorisee') : t('commun.poele.alarme_titre')}
      {#if code}<b class="code">{code}</b>{/if}
    </h4>
    {#if meaning}<p class="meaning">{meaning}</p>{:else if !code}<p class="meaning">{t('commun.poele.alarme_sans_code')}</p>{/if}
    {#if began}
      <p class="when">{lighting ? t('commun.poele.alarme_pendant_allumage', { ...when(began), start: clock(lighting) }) : t('commun.poele.alarme_le', when(began))}</p>
    {/if}
    <p>{mode === 'mem_alarm' ? t('commun.poele.alarme_explique_memorisee') : alarmed ? t('commun.poele.alarme_explique') : t('commun.poele.alarme_explique_code')}</p>
    {#if advice}
      <p class="advice"><b>{t('commun.poele.alarme_a_regarder')}</b> {advice}</p>
    {/if}
    {#if alarmed}
      {#if canReset}
        <p class="muted">{t('commun.poele.rearmer_aide')}</p>
        <button class="reset" onclick={reset} disabled={asked} aria-busy={asked}>
          {#if asked}<span class="busy-ring" aria-hidden="true"></span>{t('commun.poele.rearmement')}{:else}<Icon name="power" size={16} />{t('commun.poele.rearmer')}{/if}
        </button>
      {:else}
        <p class="muted">{t('commun.poele.rearmer_sur_poele')}</p>
      {/if}
    {/if}
    {#if code}<p class="muted small">{t('commun.poele.alarme_notice')}</p>{/if}
  </section>
{/if}

<style>
  .alarm {
    display: grid;
    gap: 6px;
    padding: 12px 14px;
    border-radius: 14px;
    background: var(--alert-soft);
    color: var(--ink);
    font-size: 13px;
    scroll-margin-top: 16px;
  }

  h4 {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 14.5px;
    font-weight: 700;
    color: var(--alert);
  }

  .code {
    padding: 1px 8px;
    border-radius: 999px;
    background: var(--surface);
    color: var(--alert);
    font-size: 12.5px;
    letter-spacing: 0.02em;
  }

  .meaning {
    font-size: 14px;
    font-weight: 650;
  }

  .when {
    font-weight: 600;
    color: var(--ink-2);
  }

  .advice b {
    font-weight: 700;
  }

  .muted {
    font-size: 12px;
    color: var(--ink-3);
  }

  .small {
    font-size: 11px;
  }

  .reset {
    display: inline-flex;
    align-items: center;
    justify-self: start;
    gap: 6px;
    padding: 8px 16px;
    border: none;
    border-radius: 999px;
    background: var(--alert);
    color: #fff;
    font: inherit;
    font-size: 14px;
    font-weight: 650;
    cursor: pointer;
  }

  .reset:disabled {
    opacity: 0.7;
    cursor: default;
  }
</style>
