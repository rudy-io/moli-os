<script>
  import { device, value, reachable, nameOf, act } from '../lib/home.svelte.js';
  import { printFile, pagesOf, isPdf, parsePages } from '../lib/paper.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** The paper printer: its state in words, each cartridge, the last print,
   *  and printing a photo or a PDF from here (pages made in the browser). */
  let { id } = $props();

  const d = $derived(device(id));
  const v = (k) => value(id, k);
  const ok = $derived(reachable(id) && d?.online !== null);
  const status = $derived(ok ? (v('status') ?? t('salon.papier.prete')) : t('salon.eteinte'));
  // The printer's own words (Moli's printer driver says them in French): the ones that mean it is stuck.
  // The printer's words, in the house's language (French or English).
  const STUCK = /papier|bourrage|capot|vide ou absente|arrêtée|paper|jam|cover|empty or missing|stopped/i;
  const tone = $derived(!ok ? '' : v('state') === 'processing' ? 'cool' : STUCK.test(status) ? 'alert' : v('ink_low') ? 'warm' : 'good');

  // Cartridges: « Encre couleur » gets the three inks, « Encre noire » black.
  const inks = $derived(
    (d?.points ?? [])
      .filter((p) => /^ink_\d+$/.test(p.key))
      .map((p) => ({ key: p.key, label: p.label, level: v(p.key) })),
  );
  const inkFill = (label) =>
    /couleur|colou?r/i.test(label)
      ? 'linear-gradient(90deg, #39d2e7, #d945dd, #dfd31d)'
      : /noire|black/i.test(label)
        ? '#1d2230'
        : /cyan/i.test(label)
          ? '#39d2e7'
          : /magenta/i.test(label)
            ? '#d945dd'
            : /jaune|yellow/i.test(label)
              ? '#dfd31d'
              : 'var(--ink-3)';

  const job = $derived(v('job'));
  const busy = $derived(/envoi|en attente|en cours|bloquée|interrompue|sending|waiting|in progress|blocked|interrupted/.test(String(job ?? '')));

  // ---- printing ----
  let input;
  let file = $state(null);
  let count = $state(0);
  let copies = $state(1);
  let color = $state(true);
  let which = $state('');
  let sending = $state(null); // { done, total, step }
  let error = $state('');
  let sent = $state('');

  async function choose(e) {
    const f = e.currentTarget.files?.[0];
    e.currentTarget.value = '';
    if (!f) return;
    error = '';
    sent = '';
    file = f;
    count = 0;
    which = '';
    try {
      if (isPdf(f)) {
        const pages = await pagesOf(f);
        count = pages.count;
        pages.close();
      } else {
        count = 1;
      }
    } catch (err) {
      error = err.message;
      file = null;
    }
  }

  const selected = $derived(count > 1 ? parsePages(which, count) : null);
  const pageCount = $derived(selected ? selected.length : count);

  async function go() {
    if (!file || sending) return;
    if (selected && !selected.length) {
      error = t('salon.papier.pages_exemple');
      return;
    }
    error = '';
    sending = { done: 0, total: pageCount, step: 'prepare' };
    try {
      await printFile(id, file, { copies, color, pages: selected }, (p) => (sending = p));
      sent = t('salon.papier.envoye', { nom: file.name, count: pageCount });
      file = null;
    } catch (err) {
      error = err.partial ? err.message : t('salon.papier.pas_imprime', { message: err.message });
    } finally {
      sending = null;
    }
  }

  let confirm = $state(false);
  let confirmTimer;
  function cancel() {
    if (!confirm) {
      confirm = true;
      clearTimeout(confirmTimer);
      confirmTimer = setTimeout(() => (confirm = false), 5000);
      return;
    }
    confirm = false;
    act(`${id}/cancel`, true, t('salon.impression.act_annuler', { nom: nameOf(id) }));
  }
</script>

<!-- A file chosen or pages on their way: a new version waits (no reload). -->
<section class="card paper" data-unsaved={file || sending ? true : undefined}>
  <div class="head">
    <span class="logo"><Icon name="printer" size={22} /></span>
    <div class="title">
      <h2>{d?.label?.name || t('salon.papier.imprimante')}</h2>
      {#if d?.model || d?.manufacturer}<span class="muted">{[d.manufacturer, d.model].filter(Boolean).join(' ')}</span>{/if}
    </div>
    <span class="chip {tone}">{status}</span>
    {#if ok}
      <button class="round" onclick={() => act(`${id}/identify`, true, t('salon.papier.act_clignoter', { nom: nameOf(id) }))} title={t('salon.papier.clignoter_titre')} aria-label={t('salon.papier.clignoter')}>
        <Icon name="flash" size={18} />
      </button>
    {/if}
  </div>

  {#if inks.length}
    <ul class="inks">
      {#each inks as ink (ink.key)}
        <li class:low={typeof ink.level === 'number' && ink.level <= 15}>
          <span class="label">{ink.label}</span>
          <span class="gauge" role="meter" aria-label={ink.label} aria-valuemin="0" aria-valuemax="100" aria-valuenow={ink.level ?? undefined}>
            <i style:width="{typeof ink.level === 'number' ? Math.max(3, ink.level) : 0}%" style:background={inkFill(ink.label)}></i>
          </span>
          <b class="num">{typeof ink.level === 'number' ? `${ink.level} %` : '?'}</b>
        </li>
      {/each}
    </ul>
    {#if inks.every((i) => i.level === 0) && v('ink_low')}
      <p class="note muted">{t('salon.papier.zero_pour_cent')}</p>
    {/if}
  {/if}

  {#if job}
    <div class="job">
      <Icon name={busy ? 'timer' : 'history'} size={16} />
      <span>{job}</span>
      {#if busy && ok}
        <button class="ghost" class:danger={confirm} onclick={cancel}>{confirm ? t('salon.papier.sur') : t('commun.annuler')}</button>
      {/if}
    </div>
  {/if}

  <input bind:this={input} type="file" accept="image/*,application/pdf,.pdf" hidden onchange={choose} />

  {#if file}
    <div class="sheet">
      <div class="file">
        <Icon name={isPdf(file) ? 'book' : 'camera'} size={18} />
        <b>{file.name}</b>
        <span class="muted">{count ? t('salon.papier.nb_pages', { count }) : '…'}</span>
        <button class="round small" onclick={() => (file = null)} aria-label={t('salon.papier.ne_pas_imprimer')} disabled={!!sending}><Icon name="close" size={16} /></button>
      </div>
      <div class="options">
        <div class="seg" role="radiogroup" aria-label={t('salon.papier.couleur')}>
          <button role="radio" aria-checked={color} class:on={color} onclick={() => (color = true)}>{t('salon.papier.couleur')}</button>
          <button role="radio" aria-checked={!color} class:on={!color} onclick={() => (color = false)}>{t('salon.papier.noir_blanc')}</button>
        </div>
        <div class="stepper" aria-label={t('salon.papier.exemplaires')}>
          <button onclick={() => (copies = Math.max(1, copies - 1))} aria-label={t('salon.papier.un_de_moins')} disabled={copies <= 1}>−</button>
          <span class="num">{t('salon.papier.nb_exemplaires', { count: copies })}</span>
          <button onclick={() => (copies = Math.min(20, copies + 1))} aria-label={t('salon.papier.un_de_plus')} disabled={copies >= 20}>+</button>
        </div>
        {#if count > 1}
          <label class="pages">
            <span>{t('salon.papier.pages')}</span>
            <input type="text" inputmode="numeric" placeholder={t('salon.papier.toutes')} bind:value={which} />
          </label>
        {/if}
      </div>
      <button class="primary" onclick={go} disabled={!!sending || !count || !ok}>
        <Icon name="printer" size={18} />
        {#if sending}
          {sending.step === 'prepare' ? t('salon.papier.preparation') : sending.step === 'attend' ? t('salon.papier.file_pleine') : t('salon.papier.envoi')} {Math.min(sending.done + 1, sending.total)}/{sending.total}…
        {:else}
          {pageCount > 1 ? t('salon.papier.imprimer_pages', { count: pageCount }) : t('salon.papier.imprimer')}
        {/if}
      </button>
    </div>
  {:else}
    <button class="primary" onclick={() => input.click()} disabled={!ok}>
      <Icon name="printer" size={18} />{t('salon.papier.imprimer_photo')}
    </button>
  {/if}

  {#if !ok}<p class="note muted">{t('salon.papier.eteinte_aide')}</p>{/if}
  {#if error}<p class="note error">{error}</p>{/if}
  {#if sent}<p class="note good">{t('salon.papier.suite', { envoye: sent })}</p>{/if}
</section>

<style>
  .paper {
    display: grid;
    gap: 16px;
  }

  .head {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .logo {
    display: grid;
    place-items: center;
    width: 44px;
    height: 44px;
    border-radius: 14px;
    background: var(--surface-2);
    color: var(--ink-2);
  }

  .title {
    display: grid;
    flex: 1;
    min-width: 0;
  }

  .title h2 {
    font-size: 18px;
  }

  .title .muted {
    font-size: 13px;
  }

  .round {
    display: grid;
    place-items: center;
    width: 38px;
    height: 38px;
    border: none;
    border-radius: 50%;
    background: var(--surface-2);
    color: var(--ink-2);
    cursor: pointer;
  }

  .round.small {
    width: 30px;
    height: 30px;
  }

  .inks {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 10px;
  }

  .inks li {
    display: grid;
    grid-template-columns: 120px 1fr 48px;
    align-items: center;
    gap: 12px;
    font-size: 14px;
  }

  .inks .label {
    color: var(--ink-2);
    font-weight: 600;
  }

  .gauge {
    height: 10px;
    border-radius: 999px;
    background: var(--surface-3);
    overflow: hidden;
  }

  .gauge i {
    display: block;
    height: 100%;
    border-radius: inherit;
  }

  .inks b {
    text-align: right;
    font-weight: 700;
  }

  .inks li.low b {
    color: var(--warm-ink);
  }

  .job {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 12px;
    border-radius: var(--r-sm);
    background: var(--surface-2);
    font-size: 14px;
    color: var(--ink-2);
  }

  .job span {
    flex: 1;
    min-width: 0;
  }

  .ghost {
    border: none;
    background: none;
    color: var(--ink-2);
    font: inherit;
    font-weight: 650;
    cursor: pointer;
  }

  .ghost.danger {
    color: var(--alert);
  }

  .primary {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    min-height: 46px;
    padding: 0 18px;
    border: none;
    border-radius: 999px;
    background: var(--ink);
    color: var(--surface);
    font: inherit;
    font-weight: 700;
    cursor: pointer;
  }

  .primary:disabled {
    opacity: 0.45;
    cursor: default;
  }

  .sheet {
    display: grid;
    gap: 12px;
    padding: 14px;
    border-radius: var(--r-md);
    background: var(--surface-2);
  }

  .file {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }

  .file b {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .file .muted {
    flex: 1;
    font-size: 13px;
    white-space: nowrap;
  }

  .options {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    align-items: center;
  }

  .seg {
    display: inline-flex;
    padding: 4px;
    background: var(--surface);
    border-radius: 999px;
    gap: 2px;
  }

  .seg button {
    border: 0;
    background: none;
    border-radius: 999px;
    padding: 7px 12px;
    font: inherit;
    font-size: 13px;
    font-weight: 650;
    color: var(--ink-3);
    cursor: pointer;
  }

  .seg button.on {
    background: var(--ink);
    color: var(--surface);
  }

  .stepper {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 4px;
    border-radius: 999px;
    background: var(--surface);
  }

  .stepper button {
    width: 30px;
    height: 30px;
    border: none;
    border-radius: 50%;
    background: var(--surface-2);
    font: inherit;
    font-size: 17px;
    cursor: pointer;
  }

  .stepper span {
    min-width: 46px;
    text-align: center;
    font-size: 13px;
    font-weight: 700;
  }

  .pages {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
    color: var(--ink-2);
  }

  .pages input {
    width: 150px;
    padding: 7px 10px;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--surface);
    color: var(--ink);
    font: inherit;
  }

  .note {
    margin: 0;
    font-size: 13px;
  }

  .note.error {
    color: var(--alert);
  }

  .note.good {
    color: var(--good);
  }

  @media (max-width: 480px) {
    .inks li {
      grid-template-columns: 96px 1fr 44px;
    }
  }
</style>
