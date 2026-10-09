<script>
  import { formatBytes, ago } from '../lib/format.js';
  import { t } from '../lib/i18n.svelte.js';

  let { onclose } = $props();
  let bench = $state(null);
  let error = $state('');

  $effect(() => {
    fetch('/api/bench')
      .then((r) => (r.ok ? r.json() : r.json().then((e) => Promise.reject(new Error(e.error)))))
      .then((b) => (bench = b))
      .catch((e) => (error = e.message));
  });

  const pct = (n, d) => (d ? Math.round((100 * n) / d) : 0);
  const ratio = $derived(
    bench?.ha.memory_bytes && bench?.moli.memory_bytes
      ? Math.round(bench.ha.memory_bytes / bench.moli.memory_bytes)
      : null,
  );

  function onkey(e) {
    if (e.key === 'Escape') onclose();
  }
</script>

<svelte:window onkeydown={onkey} />

<div class="backdrop" onclick={onclose} role="presentation"></div>
<div class="panel" role="dialog" aria-modal="true" aria-label={t('systeme.bench.aria')}>
  <header>
    <h3>{t('systeme.bench.titre')}</h3>
    <button class="close" onclick={onclose} aria-label={t('commun.fermer')}>×</button>
  </header>

  {#if error}
    <p class="muted">{error}</p>
  {:else if !bench}
    <p class="muted">{t('systeme.chargement')}</p>
  {:else}
    <div class="figures">
      <div>
        <span class="big num">{bench.coverage.covered}<small>/{bench.coverage.total}</small></span>
        <span class="muted">{t('systeme.bench.couverts', { pct: pct(bench.coverage.covered, bench.coverage.total) })}</span>
      </div>
      <div>
        <span class="big num">{formatBytes(bench.moli.memory_bytes)}</span>
        <span class="muted">{t('systeme.bench.contre', { bytes: formatBytes(bench.ha.memory_bytes) })}{#if ratio} · ÷{ratio}{/if}</span>
      </div>
      <div>
        <span class="big num">{bench.moli.points}</span>
        <span class="muted">{t('systeme.bench.points', { entities: bench.ha.entities })}</span>
      </div>
    </div>

    <table>
      <thead>
        <tr><th>{t('systeme.bench.integration')}</th><th class="num">{t('systeme.bench.couverts_col')}</th><th>{t('systeme.bench.pas_encore')}</th></tr>
      </thead>
      <tbody>
        {#each bench.integrations as row (row.integration)}
          <tr class:done={row.covered === row.ha_devices}>
            <td>{row.integration}</td>
            <td class="num">
              <span class="bar"><span style="width:{pct(row.covered, row.ha_devices)}%"></span></span>
              {row.covered}/{row.ha_devices}
            </td>
            <td class="missing">{row.missing.join(', ')}</td>
          </tr>
        {/each}
      </tbody>
    </table>
    <p class="muted">{t('systeme.bench.mesure', { when: ago(bench.generated, Date.now()) })}</p>
  {/if}
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: color-mix(in srgb, var(--ink) 35%, transparent);
    z-index: 20;
  }
  .panel {
    position: fixed;
    z-index: 21;
    left: 50%;
    top: 50%;
    transform: translate(-50%, -50%);
    width: min(820px, calc(100vw - 32px));
    max-height: calc(100vh - 32px);
    overflow: auto;
    background: var(--card);
    border: var(--stroke) solid var(--ink);
    border-radius: var(--radius);
    padding: 16px 18px;
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  h3 {
    margin: 0;
    font-size: 18px;
  }
  .close {
    border: 0;
    background: none;
    font-size: 22px;
    cursor: pointer;
  }
  .figures {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
    gap: 16px;
    margin: 14px 0 18px;
  }
  .figures div {
    display: grid;
  }
  .big {
    font-size: 30px;
    font-weight: 800;
    letter-spacing: -0.02em;
  }
  .big small {
    font-size: 18px;
    color: var(--ink-3);
  }
  .muted {
    color: var(--ink-3);
    font-size: 13px;
  }
  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 14px;
  }
  th {
    text-align: left;
    font-size: 11px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--ink-3);
    padding: 6px 4px;
    border-bottom: var(--stroke) solid var(--ink);
  }
  td {
    padding: 6px 4px;
    border-bottom: 1px solid var(--line);
    vertical-align: top;
  }
  td.num {
    white-space: nowrap;
  }
  .bar {
    display: inline-block;
    width: 60px;
    height: 6px;
    background: var(--line);
    border-radius: 3px;
    margin-right: 6px;
    vertical-align: middle;
    overflow: hidden;
  }
  .bar span {
    display: block;
    height: 100%;
    background: var(--copper);
  }
  tr.done .bar span {
    background: var(--ok);
  }
  .missing {
    color: var(--ink-3);
    font-size: 13px;
  }
</style>
