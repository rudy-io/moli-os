<script>
  import { home, device, value, nameOf, num } from '../lib/home.svelte.js';
  import { onMinutes, onLevel } from '../lib/paper.js';
  import { duration } from '../lib/printers.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** The paper printer on the home page while it is on: its state, how long
   *  it has been on (a worry after 12 hours), its toner or ink. The whole
   *  card, with printing, is on the Printing page. */
  let { id } = $props();

  const d = $derived(device(id));
  const status = $derived(value(id, 'status') ?? t('salon.papier.prete'));
  const STUCK = /papier|bourrage|capot|vide ou absente|arrêtée|paper|jam|cover|empty or missing|stopped/i;
  const level = $derived(onLevel(d, home.now));
  const minutes = $derived(onMinutes(d, home.now));
  const tone = $derived(STUCK.test(status) ? 'alert' : level === 'too_long' ? 'alert' : level === 'long' || value(id, 'ink_low') ? 'warm' : 'good');
  const inks = $derived(
    (d?.points ?? [])
      .filter((p) => /^ink_\d+$/.test(p.key))
      .map((p) => ({ key: p.key, label: p.label, level: value(id, p.key) }))
      .filter((i) => typeof i.level === 'number'),
  );
</script>

<section class="card printer-home">
  <div class="card-head">
    <h2><Icon name="printer" size={18} />{nameOf(id)}</h2>
    <span class="chip {tone}">{status}</span>
    <a class="more" href="#/impression" aria-label={t('maison.accueil.imprimante_page')}><Icon name="arrow-top-right" size={18} /></a>
  </div>
  <p class="line" class:alert={level === 'too_long'} class:warm={level === 'long'}>
    {#if minutes != null}{t('maison.accueil.imprimante_allumee', { time: duration(minutes) })}{:else}<span class="muted">—</span>{/if}
    {#each inks as ink (ink.key)}<span class="num ink" class:low={ink.level <= 15}>{ink.label} {num(ink.level, 0)} %</span>{/each}
  </p>
</section>

<style>
  .printer-home {
    display: grid;
    gap: 10px;
  }

  .printer-home .card-head {
    margin-bottom: 0;
    justify-content: flex-start;
    gap: 10px;
  }

  .card-head h2 {
    flex: 1;
  }

  .line {
    display: flex;
    flex-wrap: wrap;
    justify-content: space-between;
    gap: 6px 14px;
    font-size: 13.5px;
    color: var(--ink-2);
  }

  .line.warm {
    color: var(--warm-ink);
  }

  .line.alert {
    color: var(--alert);
    font-weight: 650;
  }

  .ink.low {
    color: var(--alert);
  }

  .muted {
    color: var(--ink-3);
  }
</style>
