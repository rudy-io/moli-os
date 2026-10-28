<script>
  import Icon from '../Icon.svelte';
  import { cellier, saveSettings, readerWhy } from '../../lib/cellier.svelte.js';
  import { note } from '../../lib/home.svelte.js';
  import { t } from '../../../lib/i18n.svelte.js';

  /** The split threshold, each chain (on or off, the store used, its
   *  minimum order and fee, typed in euros, kept in cents), and who reads
   *  the receipts. */

  const reader = $derived(cellier.reader);

  // The slider while it moves; the saved value otherwise.
  let moving = $state(null);
  const pct = $derived(moving ?? Math.round(cellier.threshold * 100));

  async function saveThreshold(value) {
    try {
      await saveSettings({ threshold: value / 100 });
      note(t('cellier.reglages.seuil_enregistre', { pct: value }));
    } catch (err) {
      note(err.message, 'error');
    } finally {
      moving = null;
    }
  }

  // Edits in progress, per chain: a change arriving live (an agent, the
  // other adult) does not wipe what is being typed.
  let drafts = $state({});
  const toForm = (s) => ({ enabled: s.enabled, store: s.store, min: s.min_order_cents / 100, fee: s.fee_cents / 100 });
  const form = (p) => drafts[p.info.id] ?? toForm(p.settings);
  function edit(p, field, value) {
    drafts[p.info.id] = { ...form(p), [field]: value };
  }

  /** Euros typed (« 35 », « 2,5 ») as cents; anything else is 0. */
  function cents(v) {
    const n = Number(String(v).replace(',', '.'));
    return Number.isFinite(n) && n > 0 ? Math.round(n * 100) : 0;
  }

  async function save(e, p) {
    e.preventDefault();
    const f = form(p);
    try {
      await saveSettings({
        providers: [{ id: p.info.id, enabled: f.enabled, store: f.store.trim(), min_order_cents: cents(f.min), fee_cents: cents(f.fee) }],
      });
      delete drafts[p.info.id];
      note(t('cellier.reglages.enregistre', { nom: p.info.name }));
    } catch (err) {
      note(err.message, 'error');
    }
  }
</script>

<section class="card">
  <label class="threshold">
    <span class="head">
      <b>{t('cellier.reglages.seuil')}</b>
      <output class="num">{t('commun.pourcent', { n: pct })}</output>
    </span>
    <input
      type="range"
      min="0"
      max="90"
      step="5"
      value={pct}
      oninput={(e) => (moving = Number(e.currentTarget.value))}
      onchange={(e) => saveThreshold(Number(e.currentTarget.value))}
    />
    <small class="muted">{t('cellier.reglages.seuil_aide', { pct })}</small>
  </label>
</section>

<h2 class="title">{t('cellier.reglages.tickets')}</h2>
<section class="card reader">
  <span class="ic" class:off={!reader?.allowed}><Icon name="receipt" size={22} /></span>
  <div>
    {#if !reader}
      <p class="muted">…</p>
    {:else if reader.allowed}
      <p><b>{t('cellier.reglages.lecteur', { lecteur: `${reader.provider}/${reader.model}` })}</b></p>
      <p class="muted small">{t('cellier.reglages.photos_partent')}</p>
    {:else}
      <p><b>{t('cellier.reglages.lecteur_refuse')}</b></p>
      <p class="muted small">{readerWhy(reader)}</p>
    {/if}
  </div>
</section>

<h2 class="title">{t('cellier.reglages.enseignes')}</h2>
<div class="chains">
  {#each cellier.providers as p (p.info.id)}
    {@const f = form(p)}
    {@const id = `cellier-${p.info.id}`}
    <form class="card chain" class:off={!f.enabled} onsubmit={(e) => save(e, p)}>
      <header>
        <h3>{p.info.name}</h3>
        <span class="chip">{p.info.kind === 'drive' ? t('cellier.reglages.drive') : t('cellier.reglages.en_ligne')}</span>
        <label class="toggle">
          <input type="checkbox" checked={f.enabled} onchange={(e) => edit(p, 'enabled', e.currentTarget.checked)} />
          {t('cellier.reglages.activee')}
        </label>
      </header>
      <div class="fields">
        <label for="{id}-store">{t('cellier.reglages.magasin')}</label>
        <input id="{id}-store" value={f.store} oninput={(e) => edit(p, 'store', e.currentTarget.value)} autocomplete="off" />
        <label for="{id}-min">{t('cellier.reglages.minimum')}</label>
        <input id="{id}-min" type="number" min="0" step="0.01" inputmode="decimal" value={f.min} oninput={(e) => edit(p, 'min', e.currentTarget.value)} />
        <label for="{id}-fee">{t('cellier.reglages.frais')}</label>
        <input id="{id}-fee" type="number" min="0" step="0.01" inputmode="decimal" value={f.fee} oninput={(e) => edit(p, 'fee', e.currentTarget.value)} />
      </div>
      <button class="save" disabled={!drafts[p.info.id]}>{t('cellier.reglages.enregistrer')}</button>
    </form>
  {/each}
</div>

<style>
  .threshold {
    display: grid;
    gap: 10px;
  }

  .head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 12px;
  }

  .head b {
    font-weight: 650;
  }

  .head output {
    font-size: 22px;
    font-weight: 600;
  }

  input[type='range'] {
    width: 100%;
    height: 32px;
    accent-color: var(--warm);
  }

  .title {
    margin: 22px 0 10px;
    font-size: 15px;
    font-weight: 650;
    color: var(--ink-2);
  }

  .reader {
    display: flex;
    align-items: flex-start;
    gap: 14px;
  }

  .reader > div {
    display: grid;
    gap: 4px;
    min-width: 0;
  }

  .reader b {
    font-weight: 650;
    overflow-wrap: anywhere;
  }

  .small {
    font-size: 13px;
  }

  .ic {
    width: 40px;
    height: 40px;
    flex: none;
    border-radius: 12px;
    display: grid;
    place-items: center;
    background: var(--good-soft);
    color: var(--good);
  }

  .ic.off {
    background: var(--surface-2);
    color: var(--ink-3);
  }

  .chains {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 300px), 1fr));
    gap: 14px;
  }

  .chain {
    display: grid;
    gap: 14px;
  }

  .chain.off h3 {
    color: var(--ink-3);
  }

  .chain header {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }

  .chain h3 {
    font-size: 17px;
    font-weight: 650;
  }

  .toggle {
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    gap: 8px;
    min-height: 44px;
    font-size: 14px;
    font-weight: 600;
    cursor: pointer;
  }

  .toggle input {
    width: 22px;
    height: 22px;
    accent-color: var(--good);
  }

  .fields {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    align-items: center;
    gap: 8px 12px;
  }

  .fields label {
    font-size: 13px;
    font-weight: 600;
    color: var(--ink-2);
  }

  .fields input {
    min-height: 44px;
    min-width: 0;
    padding: 0 12px;
    border: 1px solid var(--line);
    border-radius: var(--r-sm);
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
    font-size: 16px;
  }

  .save {
    justify-self: end;
    min-height: 44px;
    padding: 0 20px;
    border: 0;
    border-radius: 999px;
    background: var(--ink);
    color: var(--surface);
    font-weight: 650;
  }

  .save:disabled {
    opacity: 0.35;
  }

  @media (max-width: 420px) {
    .fields {
      grid-template-columns: minmax(0, 1fr);
      gap: 4px;
    }

    .fields input {
      margin-bottom: 6px;
    }
  }
</style>
