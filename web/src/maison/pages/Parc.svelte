<script>
  import { hub, value, nameOf, hidden, num, watches } from '../lib/home.svelte.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from '../ui/Icon.svelte';

  /** What the house watches beyond itself: every device with an `alert`
   *  point (semantic `alert`, true = something is wrong). Nothing is listed
   *  by hand: a service that says how it is doing shows up by itself, and
   *  one taken away is gone. */
  const DETAIL = 'detail';

  /** A count of disks has no decimals, a throughput has one; and an ISO date
   *  a service hands over as is reads better shortened than on 32 characters. */
  const ISO = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}/;
  const readable = (v) => {
    if (typeof v === 'number') return num(v, Number.isInteger(v) ? 0 : 1);
    if (v === true) return t('commun.oui');
    if (v === false) return t('commun.non');
    if (typeof v === 'string' && ISO.test(v)) return `${v.slice(8, 10)}/${v.slice(5, 7)} ${v.slice(11, 16)}`;
    // A composite value (an object, a list) is no sentence: shown as JSON, never « [object Object] ».
    if (v !== null && typeof v === 'object') return JSON.stringify(v);
    return v;
  };

  const items = $derived(
    Object.values(hub.devices)
      .filter((d) => !hidden(d.id) && watches(d))
      .map((d) => {
        const alerts = d.points.filter((p) => p.semantic === 'alert').map((p) => p.key);
        return {
          id: d.id,
          name: nameOf(d.id),
          alert: alerts.some((k) => value(d.id, k) === true),
          silent: d.online === false,
          // The service words it when it can; otherwise the card shows its
          // values rather than make a sentence up.
          detail: value(d.id, DETAIL) ?? null,
          facts: d.points
            .filter((p) => !alerts.includes(p.key) && p.key !== DETAIL)
            .map((p) => ({ key: p.key, label: p.label ?? p.key, v: value(d.id, p.key), unit: p.unit }))
            .filter((f) => f.v != null && f.v !== '')
            .slice(0, 6),
        };
      })
      // What is wrong first: this page is opened for that.
      .sort((a, b) => Number(b.alert || b.silent) - Number(a.alert || a.silent) || a.name.localeCompare(b.name, locale())),
  );

  const wrong = $derived(items.filter((x) => x.alert || x.silent).length);
</script>

<div class="parc">
  <header>
    <div>
      <h1 class="page-title">{t('systeme.parc.titre')}</h1>
      <p class="page-sub">{t('systeme.parc.sous_titre')}</p>
    </div>
    {#if items.length}
      {#if wrong}
        <span class="chip warm"><Icon name="warning" size={15} />{t('systeme.parc.a_voir', { count: wrong })}</span>
      {:else}
        <span class="chip good"><Icon name="ok" size={15} />{t('systeme.parc.tout_va_bien')}</span>
      {/if}
    {/if}
  </header>

  {#if items.length}
    <div class="grid">
      {#each items as it (it.id)}
        <section class="card" class:alert={it.alert || it.silent}>
          <h2>
            <Icon name={it.alert || it.silent ? 'warning' : 'ok'} size={17} />
            {it.name}
          </h2>
          <p class="detail">
            {#if it.silent}{t('systeme.parc.muet')}{:else if it.detail}{it.detail}{:else}{t('systeme.parc.sans_phrase')}{/if}
          </p>
          {#if it.facts.length}
            <dl>
              {#each it.facts as f (f.key)}
                <div><dt>{f.label}</dt><dd>{readable(f.v)}{#if f.unit}&nbsp;{f.unit}{/if}</dd></div>
              {/each}
            </dl>
          {/if}
        </section>
      {/each}
    </div>
  {:else}
    <p class="muted">{t('systeme.parc.aucun')}</p>
  {/if}
</div>

<style>
  .parc {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 20px;
  }

  header {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 340px), 1fr));
    gap: 20px;
    align-items: start;
  }

  .card {
    display: grid;
    gap: 10px;
  }

  h2 {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0;
    font-size: 1rem;
  }

  .card.alert h2 {
    color: var(--warm);
  }

  .detail {
    margin: 0;
    color: var(--ink-2);
  }

  dl {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 140px), 1fr));
    gap: 8px 16px;
    margin: 0;
  }

  dt {
    font-size: 0.82rem;
    color: var(--ink-2);
  }

  dd {
    margin: 0;
    font-variant-numeric: tabular-nums;
  }
</style>
