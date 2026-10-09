<script>
  import { onMount } from 'svelte';
  import Icon from '../ui/Icon.svelte';
  import Orb from '../moli/Orb.svelte';
  import Editor from '../auto/Editor.svelte';
  import { home, note, relative } from '../lib/home.svelte.js';
  import { api, asHuman, pending, liveRuns, CATALOG, CANCELLED } from '../lib/auto.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';

  /** « The n8n of the house »: what runs by itself, and a way to ask for
   *  more in plain words. */
  let { route = '' } = $props();

  const sub = $derived(route.split('/')[1] ?? '');

  let list = $state(null);
  let abilities = $state({});
  let recent = $state([]);
  let ask = $state('');
  let drafting = $state(false);
  let error = $state('');

  async function refresh() {
    try {
      const r = await api.list();
      list = r.automations;
      abilities = r.abilities ?? {};
      recent = await api.runs(12);
      error = '';
    } catch (err) {
      error = err.message;
      list = [];
    }
  }

  onMount(() => {
    refresh();
    return liveRuns((ev) => {
      if (ev.type === 'run_ended' || ev.type === 'changed') refresh();
    });
  });

  $effect(() => {
    if (!sub) refresh();
  });

  // Icon and id of each idea; its words are `automatismes.idee.<id>.titre` and `.demande`.
  const IDEAS = [
    ['door-open', 'entree'],
    ['moon', 'minuit'],
    ['bolt', 'conso'],
    ['bell', 'sonnette'],
    ['sparkles', 'recap'],
    ['cctv', 'cour'],
  ];

  async function draft(request) {
    const text = (request ?? ask).trim();
    if (!text || drafting) return;
    drafting = true;
    try {
      const d = await api.draft(text, null);
      pending.draft = { name: d.name, mode: d.mode, graph: d.graph, note: d.note };
      ask = '';
      location.hash = '#/automatismes/nouveau';
    } catch (err) {
      note(err.status === 503 ? t('automatismes.moli_absent_zero') : t('automatismes.moli_erreur', { message: err.message }), 'error');
    } finally {
      drafting = false;
    }
  }

  async function toggle(item) {
    const a = item.automation;
    try {
      if (item.live) {
        await api.enabled(a.id, false);
        note(t('automatismes.liste.coupe_note', { nom: a.name }));
      } else {
        await asHuman(t('automatismes.activer_label', { nom: a.name }), () => api.approve(a.id, item.fingerprint));
        // Home Assistant's original still runs until someone switches it off.
        note(a.author === 'import' ? t('automatismes.liste.tourne_import', { nom: a.name }) : t('automatismes.liste.tourne_note', { nom: a.name }));
      }
    } catch (err) {
      if (err.message !== CANCELLED) note(err.message, 'error');
    }
    refresh();
  }

  function stateOf(item) {
    if (item.live) return ['live', t('automatismes.etat.live')];
    if (item.problems?.some((p) => p.level === 'error')) return ['bad', t('automatismes.etat.bad')];
    if (!item.approved) return ['draft', t('automatismes.etat.draft')];
    return ['off', t('automatismes.etat.off')];
  }

  const mine = $derived((list ?? []).filter((i) => i.automation.author !== 'import' || i.live));
  const imported = $derived((list ?? []).filter((i) => i.automation.author === 'import' && !i.live));

  const triggerIcon = (a) => CATALOG[a.graph.nodes.find((n) => CATALOG[n.type]?.family === 'trigger')?.type]?.icon ?? 'robot';
  const AUTHOR = { assistant: 'Moli', import: 'HA' };
</script>

{#snippet tile(item)}
          {@const a = item.automation}
  {@const state = stateOf(item)}
  {@const st = state[0]}
  {@const label = state[1]}
  <article class="card auto {st}">
    <a class="open" href="#/automatismes/{a.id}" aria-label={t('automatismes.liste.ouvrir', { nom: a.name })}></a>
    <div class="head">
      <span class="ic"><Icon name={triggerIcon(a)} size={20} /></span>
      <h3>{a.name}</h3>
      <button class="switch" class:on={item.live} onclick={() => toggle(item)} aria-label={item.live ? t('automatismes.liste.couper') : t('automatismes.liste.activer')} aria-pressed={item.live}><i></i></button>
    </div>
    <p class="summary">{item.summary}</p>
    <footer>
      <span class="pill {st}">{label}</span>
      {#if AUTHOR[a.author]}<span class="pill">{AUTHOR[a.author]}</span>{/if}
      {#if item.protected?.length}<span class="pill lock"><Icon name="lock" size={12} />{item.protected.join(', ')}</span>{/if}
      <span class="last muted">
        {#if item.last_run}
          <i class="dot {item.last_run.status}"></i>{t(item.last_run.dry ? 'automatismes.liste.essai_quand' : 'automatismes.liste.a_tourne', { quand: relative(item.last_run.started, home.now) })}
        {:else}
          {t('automatismes.liste.jamais')}
        {/if}
      </span>
    </footer>
  </article>
{/snippet}

{#if sub}
  <Editor id={sub} />
{:else}
  <div class="autos">
    <header>
      <h1 class="page-title">{t('automatismes.titre')}</h1>
      <p class="page-sub">{t('automatismes.liste.sous_titre')}</p>
    </header>

    <form class="ask" onsubmit={(e) => (e.preventDefault(), draft())}>
      <Orb size={46} state={drafting ? 'thinking' : 'idle'} />
      <input bind:value={ask} placeholder={t('automatismes.liste.placeholder')} aria-label={t('automatismes.liste.decrire')} disabled={drafting} />
      <button disabled={!ask.trim() || drafting}>{drafting ? t('automatismes.moli_dessine') : t('automatismes.liste.creer')}</button>
    </form>

    <section class="ideas" aria-label={t('automatismes.liste.idees')}>
      {#each IDEAS as [icon, id] (id)}
        <button onclick={() => draft(t('automatismes.idee.' + id + '.demande'))} disabled={drafting}>
          <span class="i"><Icon name={icon} size={20} /></span>
          <b>{t('automatismes.idee.' + id + '.titre')}</b>
          <small>{t('automatismes.idee.' + id + '.demande')}</small>
        </button>
      {/each}
      <a class="blank" href="#/automatismes/nouveau"><Icon name="plus" size={22} /><b>{t('automatismes.liste.partir_zero')}</b><small>{t('automatismes.liste.partir_zero_aide')}</small></a>
    </section>

    {#if error}<p class="muted">{error}</p>{/if}

    {#if list?.length}
      <h2 class="section">{t('automatismes.liste.section')} <span class="muted">{t('automatismes.liste.actifs', { count: list.filter((i) => i.live).length, total: mine.length })}</span></h2>
      {#if mine.length}
        <div class="grid">
          {#each mine as item (item.automation.id)}{@render tile(item)}{/each}
        </div>
      {:else}
        <p class="empty muted">{t('automatismes.liste.aucun_a_toi')}</p>
      {/if}
      {#if imported.length}
        <details class="imported">
          <summary>
            <Icon name="history" size={18} />
            <b>{t('automatismes.liste.importes_titre')}</b>
            <span class="muted">{t('automatismes.liste.importes', { count: imported.length })}</span>
          </summary>
          <p class="warn"><Icon name="alert" size={16} />{t('automatismes.liste.importes_avertissement')}</p>
          <div class="grid">
            {#each imported as item (item.automation.id)}{@render tile(item)}{/each}
          </div>
        </details>
      {/if}
    {:else if list}
      <p class="empty muted">{t('automatismes.liste.aucun')}</p>
    {/if}

    {#if recent.length}
      <h2 class="section">{t('automatismes.liste.recent')}</h2>
      <ul class="feed">
        {#each recent as r (r.id + (r.dry ? 'd' : ''))}
          <li>
            <i class="dot {r.status}"></i>
            <a href="#/automatismes/{r.automation}">{r.name}</a>
            <span class="muted">{r.dry ? t('automatismes.liste.essai_a_blanc') : r.why}</span>
            <small class="muted">{relative(r.started, home.now)}</small>
          </li>
        {/each}
      </ul>
    {/if}

    {#if !abilities.telegram}
      <p class="muted hint"><Icon name="info" size={15} />{t('automatismes.liste.telegram_absent')}</p>
    {/if}
  </div>
{/if}

<style>
  .autos {
    display: grid;
    gap: 22px;
  }

  .ask {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 10px 10px 10px 14px;
    border-radius: 999px;
    background: var(--surface);
    box-shadow: var(--shadow-lift);
  }

  .ask input {
    flex: 1;
    min-width: 0;
    border: 0;
    background: none;
    font: inherit;
    font-size: 19px;
    color: var(--ink);
    outline: none;
  }

  .ask button {
    border: 0;
    border-radius: 999px;
    padding: 14px 22px;
    background: var(--ink);
    color: var(--bg);
    font-weight: 700;
  }

  .ask button:disabled {
    opacity: 0.35;
  }

  .ideas {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(210px, 1fr));
    gap: 12px;
  }

  .ideas button,
  .blank {
    display: grid;
    gap: 4px;
    align-content: start;
    padding: 16px;
    border: 0;
    border-radius: var(--r-md);
    background: var(--surface);
    box-shadow: var(--shadow);
    text-align: left;
    color: var(--ink);
    text-decoration: none;
    transition: transform 0.18s var(--ease);
  }

  .ideas button:hover,
  .blank:hover {
    transform: translateY(-2px);
  }

  .ideas .i {
    width: 38px;
    height: 38px;
    border-radius: 12px;
    display: grid;
    place-items: center;
    background: var(--warm-soft);
    color: var(--warm-ink);
    margin-bottom: 6px;
  }

  .ideas small,
  .blank small {
    font-size: 13px;
    color: var(--ink-3);
    line-height: 1.35;
  }

  .blank {
    background: none;
    box-shadow: inset 0 0 0 2px var(--line);
    color: var(--ink-2);
  }

  .section {
    font-size: 15px;
    font-weight: 700;
    color: var(--ink-2);
    display: flex;
    gap: 10px;
    align-items: baseline;
  }

  .section span {
    font-weight: 500;
    font-size: 13px;
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 340px), 1fr));
    gap: 16px;
  }

  .auto {
    display: grid;
    gap: 10px;
    transition: transform 0.18s var(--ease);
  }

  .auto:hover {
    transform: translateY(-2px);
  }

  .open {
    position: absolute;
    inset: 0;
    border-radius: inherit;
  }

  .head {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .ic {
    width: 40px;
    height: 40px;
    border-radius: 13px;
    display: grid;
    place-items: center;
    background: var(--surface-2);
    color: var(--ink-3);
  }

  .live .ic {
    background: color-mix(in srgb, #e9a23b 16%, var(--surface));
    color: #e9a23b;
  }

  h3 {
    flex: 1;
    font-size: 17px;
    font-weight: 700;
  }

  .switch {
    position: relative;
    z-index: 1;
    width: 46px;
    height: 28px;
    border-radius: 999px;
    border: 0;
    background: var(--surface-3);
  }

  .switch i {
    position: absolute;
    top: 4px;
    left: 4px;
    width: 20px;
    height: 20px;
    border-radius: 50%;
    background: var(--surface);
    box-shadow: 0 1px 3px rgb(0 0 0 / 20%);
    transition: transform 0.25s var(--ease);
  }

  .switch.on {
    background: var(--good);
  }

  .switch.on i {
    transform: translateX(18px);
  }

  .summary {
    color: var(--ink-2);
    font-size: 14.5px;
    line-height: 1.45;
  }

  footer {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }

  .pill {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 4px 10px;
    border-radius: 999px;
    font-size: 12px;
    font-weight: 700;
    background: var(--surface-2);
    color: var(--ink-3);
  }

  .pill.live {
    background: var(--good-soft);
    color: var(--good);
  }

  .pill.draft {
    background: var(--warm-soft);
    color: var(--warm-ink);
  }

  .pill.bad {
    background: var(--alert-soft);
    color: var(--alert);
  }

  .pill.lock {
    background: var(--cool-soft);
    color: var(--cool);
  }

  .last {
    margin-left: auto;
    font-size: 12.5px;
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  .dot {
    display: inline-block;
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--good);
  }

  .dot.failed {
    background: var(--alert);
  }

  .dot.cancelled {
    background: var(--ink-3);
  }

  .dot.running {
    background: var(--warm);
  }

  .imported {
    border-radius: var(--r-lg);
    background: color-mix(in srgb, var(--surface) 55%, transparent);
    padding: 14px 16px;
  }

  .imported summary {
    display: flex;
    align-items: center;
    gap: 10px;
    cursor: pointer;
    flex-wrap: wrap;
    list-style: none;
  }

  .imported summary span {
    font-size: 13px;
  }

  .imported[open] summary {
    margin-bottom: 14px;
  }

  /* As in the editor. */
  .warn {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13.5px;
    color: var(--cool);
    font-weight: 600;
    margin-bottom: 14px;
  }

  .feed {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 8px;
  }

  .feed li {
    display: flex;
    align-items: center;
    gap: 10px;
    font-size: 14px;
  }

  .feed a {
    font-weight: 650;
    color: var(--ink);
    text-decoration: none;
  }

  .feed small {
    margin-left: auto;
  }

  .empty {
    padding: 20px 0;
  }

  .hint {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
  }

  @media (max-width: 560px) {
    .ask {
      border-radius: var(--r-lg);
      flex-wrap: wrap;
    }

    .ask input {
      font-size: 16px;
    }
  }
</style>
