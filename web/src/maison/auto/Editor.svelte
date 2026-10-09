<script>
  import { onMount } from 'svelte';
  import Icon from '../ui/Icon.svelte';
  import Canvas from './Canvas.svelte';
  import Inspector from './Inspector.svelte';
  import Palette from './Palette.svelte';
  import Orb from '../moli/Orb.svelte';
  import { home, note, relative, clock } from '../lib/home.svelte.js';
  import { api, asHuman, layout, needsLayout, newNode, isTrigger, liveRuns, pending, renamedIn, NODE_W } from '../lib/auto.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';

  let { id } = $props();

  let a = $state(null);
  let meta = $state({ approved: false, live: false, author: 'human', note: null, fingerprint: null, canRestore: false });
  let check = $state({ summary: '', problems: [], protected: [] });
  let runs = $state([]);
  let dirty = $state(false);
  let busy = $state('');
  let selected = $state(null);
  let palette = $state(null);
  let statuses = $state({});
  let liveNodes = $state({});
  let shownRun = $state(null);
  let ask = $state('');
  let thinking = $state(false);
  let canvas = $state();
  let loaded = $state(false);
  let error = $state('');
  let dryRun = null;

  const isNew = $derived(id === 'nouveau');
  const selectedNode = $derived(selected?.kind === 'node' ? a?.graph.nodes.find((n) => n.id === selected.id) : null);
  const problemNodes = $derived(new Set(check.problems.filter((p) => p.level === 'error' && p.node).map((p) => p.node)));
  const errors = $derived(check.problems.filter((p) => p.level === 'error'));
  const warnings = $derived(check.problems.filter((p) => p.level === 'warning'));
  const renamed = $derived(renamedIn(a?.graph));

  async function load() {
    error = '';
    if (isNew) {
      const draft = pending.draft;
      pending.draft = null;
      a = {
        id: '',
        name: draft?.name ?? t('automatismes.editeur.nouveau_nom'),
        mode: draft?.mode ?? 'restart',
        enabled: false,
        graph: draft?.graph ?? { nodes: [], edges: [] },
        note: draft?.note ?? null,
      };
      if (needsLayout(a.graph)) layout(a.graph);
      dirty = !!draft;
      meta = { approved: false, live: false, author: draft ? 'assistant' : 'human', note: draft?.note ?? null, fingerprint: null, canRestore: false };
    } else {
      try {
        const v = await api.get(id);
        a = structuredClone(v.automation);
        if (needsLayout(a.graph)) layout(a.graph);
        meta = metaOf(v);
        runs = v.runs ?? [];
        check = { summary: v.summary, problems: v.problems, protected: v.protected };
        dirty = false;
      } catch (err) {
        error = err.message;
      }
    }
    loaded = true;
    requestAnimationFrame(() => canvas?.fit());
  }

  function metaOf(v) {
    return {
      approved: v.approved,
      live: v.live,
      author: v.automation.author,
      note: v.automation.note,
      fingerprint: v.fingerprint,
      canRestore: v.can_restore,
    };
  }

  onMount(() => {
    load();
    const stop = liveRuns(onLive);
    return stop;
  });

  // The server checks the graph and writes the sentence, as you edit.
  let timer;
  $effect(() => {
    if (!a) return;
    const graph = JSON.stringify(a.graph);
    clearTimeout(timer);
    timer = setTimeout(async () => {
      try {
        check = await api.check(JSON.parse(graph));
      } catch {
        /* offline: keep the last one */
      }
    }, 350);
  });

  function onLive(ev) {
    if (!a?.id) return;
    if (ev.type === 'changed' && ev.automation === a.id) {
      // What is shown is what gets approved: always the server's version.
      if (!dirty) load();
      else note(t('automatismes.editeur.modifie_ailleurs'));
      return;
    }
    const run = ev.run;
    const automation = ev.automation ?? run?.automation;
    if (automation !== a.id) return;
    // Dry runs are shown by « Essayer » itself.
    if (run?.dry) {
      dryRun = run.id;
      return;
    }
    if (ev.type === 'step' && ev.run === dryRun) return;
    if (ev.type === 'run_started') {
      shownRun = null;
      statuses = { [run.trigger]: 'done' };
      liveNodes = { [run.trigger]: true };
    } else if (ev.type === 'step') {
      statuses = { ...statuses, [ev.step.node]: ev.step.status };
      liveNodes = ev.step.status === 'waiting' ? { [ev.step.node]: true } : {};
    } else if (ev.type === 'run_ended') {
      liveNodes = {};
      runs = [run, ...runs.filter((r) => r.id !== run.id)].slice(0, 20);
    }
  }

  function changed(kind) {
    dirty = true;
    if (kind === 'edit') shownRun = null;
  }

  async function save() {
    busy = 'save';
    try {
      const body = { ...a, graph: a.graph };
      const v = isNew || !a.id ? await api.create(body) : await api.update(body);
      a.id = v.automation.id;
      a.enabled = v.automation.enabled;
      meta = metaOf(v);
      dirty = false;
      if (isNew) history.replaceState(null, '', `#/automatismes/${v.automation.id}`);
      note(v.live ? t('automatismes.editeur.enregistre') : t('automatismes.editeur.enregistre_a_valider'));
      return v;
    } catch (err) {
      note(err.message, 'error');
      throw err;
    } finally {
      busy = '';
    }
  }

  async function activate() {
    if (errors.length) {
      note(t('automatismes.editeur.corrige'), 'error');
      return;
    }
    const imported = !!fromHa;
    try {
      await asHuman(t('automatismes.editeur.valider_label', { nom: a.name }), async () => {
        const fingerprint = dirty || !a.id ? (await save()).fingerprint : meta.fingerprint;
        const v = await api.approve(a.id, fingerprint);
        meta = metaOf(v);
        a.enabled = true;
        note(imported ? t('automatismes.editeur.parti_import') : t('automatismes.parti'));
      });
    } catch (err) {
      if (err.status === 409) {
        note(t('automatismes.editeur.change_entre_temps'), 'error');
        load();
      }
    }
  }

  async function switchOff() {
    try {
      const v = await api.enabled(a.id, false);
      meta = metaOf(v);
      a.enabled = false;
      note(t('automatismes.editeur.coupe'));
    } catch (err) {
      note(err.message, 'error');
    }
  }

  async function test() {
    busy = 'test';
    try {
      if (dirty || !a.id) await save();
      const run = await api.test(a.id);
      showRun(run);
      runs = [run, ...runs].slice(0, 20);
    } catch (err) {
      note(err.message, 'error');
    } finally {
      busy = '';
    }
  }

  async function runNow() {
    try {
      await asHuman(t('automatismes.lancer_label', { nom: a.name }), () => api.run(a.id, meta.fingerprint));
    } catch (err) {
      if (err.status === 409) load();
    }
  }

  async function restore() {
    try {
      await asHuman(t('automatismes.editeur.revenir'), async () => {
        await api.restore(a.id);
        await load();
        note(t('automatismes.editeur.revenu'));
      });
    } catch {
      /* said already */
    }
  }

  let confirming = $state(false);
  async function remove() {
    if (!a.id) {
      location.hash = '#/automatismes';
      return;
    }
    try {
      await asHuman(t('automatismes.editeur.supprimer_label', { nom: a.name }), async () => {
        await api.remove(a.id);
        note(t('automatismes.editeur.supprime'));
        location.hash = '#/automatismes';
      });
    } catch {
      /* said already */
    }
  }

  function showRun(run) {
    shownRun = run;
    const s = { [run.trigger]: 'done' };
    for (const step of run.steps) s[step.node] = step.status;
    statuses = s;
    liveNodes = {};
  }

  // Adding steps.
  function openPalette(from, port, at) {
    palette = { from, port, at, mode: from || a.graph.nodes.length ? 'next' : 'start' };
  }
  function pick(type) {
    const { from, port, at } = palette;
    const node = newNode(type, a.graph, at?.x ?? 0, at?.y ?? 0);
    if (!from && !at?.x && a.graph.nodes.length) {
      const right = Math.max(...a.graph.nodes.map((n) => n.x));
      node.x = right + NODE_W + 80;
      node.y = a.graph.nodes.at(-1).y;
    }
    a.graph.nodes.push(node);
    if (from && !isTrigger(type)) a.graph.edges.push({ from: from.id, port, to: node.id });
    palette = null;
    selected = { kind: 'node', id: node.id };
    changed('edit');
  }

  function update(patch) {
    Object.assign(selectedNode, patch);
    changed('edit');
  }

  function removeSelected() {
    const nid = selected.id;
    a.graph.nodes = a.graph.nodes.filter((n) => n.id !== nid);
    a.graph.edges = a.graph.edges.filter((e) => e.from !== nid && e.to !== nid);
    selected = null;
    changed('edit');
  }

  // Moli reworks the graph from words.
  async function askMoli(e) {
    e?.preventDefault();
    const request = ask.trim();
    if (!request || thinking) return;
    thinking = true;
    try {
      const draft = await api.draft(request, a.id || a.graph.nodes.length ? { ...a, id: a.id || 'brouillon' } : null);
      const known = new Map(a.graph.nodes.map((n) => [n.id, n]));
      const graph = draft.graph;
      const fresh = graph.nodes.filter((n) => !n.x && !n.y);
      if (fresh.length === graph.nodes.length) layout(graph);
      else
        for (const n of fresh) {
          const parent = graph.edges.find((ed) => ed.to === n.id);
          const p = parent && graph.nodes.find((x) => x.id === parent.from);
          n.x = p ? p.x + NODE_W + 80 : Math.max(...graph.nodes.map((x) => x.x)) + NODE_W + 80;
          n.y = p ? p.y + (parent.port === 'no' || parent.port === 'timeout' ? 110 : 0) : 200;
        }
      a.graph = graph;
      if (!known.size) a.name = draft.name;
      a.mode = draft.mode;
      if (draft.note) note(draft.note);
      ask = '';
      selected = null;
      changed('edit');
      requestAnimationFrame(() => canvas?.fit());
    } catch (err) {
      note(err.status === 503 ? t('automatismes.moli_absent') : t('automatismes.moli_erreur', { message: err.message }), 'error');
    } finally {
      thinking = false;
    }
  }

  const status = $derived(
    !a?.id ? 'new' : meta.live ? 'live' : !meta.approved ? 'draft' : !a.enabled ? 'off' : 'live',
  );
  // Their words: `automatismes.etat.<status>` and `automatismes.editeur.auteur.<author>`.
  const AUTHORS = ['assistant', 'import'];

  // An import never validated yet: Home Assistant's original still runs.
  // The server's note starts with which one and what to do: that part is
  // the warning, the rest is what Moli could not translate.
  // (The end of that sentence is written in the house's language, like the note.)
  const fromHa = $derived.by(() => {
    if (meta.author !== 'import' || meta.approved || meta.canRestore) return null;
    const text = meta.note ?? '';
    const haEnd = t('automatismes.ha_fin');
    const end = text.indexOf(haEnd);
    if (end < 0) return { warning: t('automatismes.ha_avertissement'), rest: text || null };
    return { warning: text.slice(0, end + haEnd.length), rest: text.slice(end + haEnd.length).trim() || null };
  });
  const noteText = $derived(fromHa ? fromHa.rest : meta.note);
  const STEPS = ['done', 'failed', 'simulated', 'yes', 'no', 'timeout', 'waiting'];
  const stepText = (s) => (STEPS.includes(s) ? t('automatismes.editeur.pas.' + s) : s);
</script>

{#if error}
  <div class="card"><p>{error}</p><a href="#/automatismes">{t('automatismes.editeur.retour')}</a></div>
{:else if a}
  <div class="editor" data-unsaved={dirty || undefined}>
    <header class="top">
      <a class="back" href="#/automatismes" aria-label={t('automatismes.editeur.retour_liste')}><Icon name="arrow-left" size={22} /></a>
      <input class="name" bind:value={a.name} oninput={() => (dirty = true)} aria-label={t('automatismes.editeur.nom')} />
      <span class="pill {status}">{t('automatismes.etat.' + status)}</span>
      {#if AUTHORS.includes(meta.author)}<span class="pill author">{t('automatismes.editeur.auteur.' + meta.author)}</span>{/if}
      <div class="actions">
        <button class="ghost" onclick={test} disabled={!!busy || !a.graph.nodes.length} title={t('automatismes.editeur.essai_titre')}><Icon name="flask" size={18} /><span>{t('automatismes.editeur.essayer')}</span></button>
        {#if a.id && meta.live && !dirty}
          <button class="ghost" onclick={runNow} title={t('automatismes.editeur.lancer_titre')}><Icon name="play-circle" size={18} /><span>{t('automatismes.editeur.lancer')}</span></button>
        {/if}
        {#if dirty}
          <button class="ghost" onclick={save} disabled={!!busy}><Icon name="save" size={18} /><span>{t('automatismes.editeur.enregistrer')}</span></button>
        {/if}
        {#if status === 'live' && !dirty}
          <button class="on" onclick={switchOff} title={t('automatismes.editeur.couper')}><span class="sw"><i></i></span>{t('automatismes.etat.live')}</button>
        {:else}
          <button class="primary" onclick={activate} disabled={!!busy || !a.graph.nodes.length}><Icon name="shield" size={18} /><span>{dirty && a.id ? t('automatismes.editeur.enregistrer_activer') : t('automatismes.editeur.valider_activer')}</span></button>
        {/if}
      </div>
    </header>

    <section class="sentence" class:bad={errors.length}>
      <p>{check.summary || '…'}</p>
      {#if check.protected?.length}
        <p class="warn"><Icon name="lock" size={16} />{t('automatismes.editeur.protegee', { pieces: check.protected.join(', ') })}</p>
      {/if}
      {#if renamed.length}
        <p class="small names"><Icon name="info" size={15} />{t('automatismes.editeur.noms_donnes')}
          {#each renamed as r, i (r.id)}{i ? t('automatismes.editeur.noms_sep') : ' '}{t('automatismes.editeur.nom_donne', { nom: r.name, natif: r.native })} <code>{r.id}</code>{/each}.
          {t('automatismes.editeur.noms_verifier')}</p>
      {/if}
      {#if fromHa}
        <p class="warn"><Icon name="alert" size={16} />{fromHa.warning}</p>
      {/if}
      {#if noteText}<p class="muted small"><Icon name="info" size={15} />{noteText}</p>{/if}
      {#if meta.canRestore}
        <p class="small changed"><Icon name="history" size={15} />{t('automatismes.editeur.modifie_depuis')}
          <button class="link" onclick={restore}>{t('automatismes.editeur.revenir')}</button></p>
      {/if}
      {#if errors.length}
        <p class="small err">{errors[0].node ? t('automatismes.editeur.problemes', { count: errors.length }) : t('automatismes.editeur.problemes_detail', { count: errors.length, message: errors[0].message })}</p>
      {/if}
    </section>

    <div class="body">
      <div class="canvas-wrap">
        <Canvas
          bind:this={canvas}
          bind:graph={a.graph}
          bind:selected
          {statuses}
          live={liveNodes}
          problems={problemNodes}
          onchange={changed}
          onadd={openPalette} />
      </div>

      <div class="side">
        {#if selectedNode}
          <Inspector
            node={selectedNode}
            problems={check.problems.filter((p) => p.node === selectedNode.id)}
            onupdate={update}
            onremove={removeSelected}
            onclose={() => (selected = null)} />
        {:else}
          <form class="card moli" onsubmit={askMoli}>
            <div class="moli-head"><Orb size={30} state={thinking ? 'thinking' : 'idle'} /><b>{a.graph.nodes.length ? t('automatismes.editeur.moli_modifier') : t('automatismes.editeur.moli_decrire')}</b></div>
            <textarea
              rows="3"
              bind:value={ask}
              placeholder={a.graph.nodes.length ? t('automatismes.editeur.exemple_modifier') : t('automatismes.editeur.exemple_creer')}
              onkeydown={(e) => e.key === 'Enter' && !e.shiftKey && askMoli(e)}></textarea>
            <button class="primary" disabled={!ask.trim() || thinking}>{thinking ? t('automatismes.moli_dessine') : t('automatismes.editeur.demander')}</button>
          </form>

          <section class="card">
            <div class="card-head"><h2><Icon name="plus" size={18} />{t('automatismes.editeur.etapes')}</h2></div>
            <button class="add" onclick={() => openPalette(null, null, null)}><Icon name="plus" size={18} />{t('automatismes.editeur.ajouter_etape')}</button>
            <p class="muted small">{t('automatismes.editeur.aide_graphe')}</p>
            <label class="mode">
              <span>{t('automatismes.editeur.mode')}</span>
              <select bind:value={a.mode} onchange={() => (dirty = true)}>
                <option value="restart">{t('automatismes.editeur.mode_restart')}</option>
                <option value="single">{t('automatismes.editeur.mode_single')}</option>
                <option value="queued">{t('automatismes.editeur.mode_queued')}</option>
              </select>
            </label>
          </section>

          <section class="card runs">
            <div class="card-head"><h2><Icon name="history" size={18} />{t('automatismes.editeur.executions')}</h2></div>
            {#if shownRun}
              <div class="run-detail">
                <p><b>{shownRun.dry ? t('automatismes.editeur.essai_a_blanc') : shownRun.why}</b> · {clock(shownRun.started)}</p>
                <ul>
                  {#each shownRun.steps as s, i (i)}
                    {@const n = a.graph.nodes.find((x) => x.id === s.node)}
                    <li class={s.status}><span>{stepText(s.status)}</span>{n ? n.id : s.node}{#if s.detail} · <em>{s.detail}</em>{/if}</li>
                  {/each}
                </ul>
                <button class="link" onclick={() => ((shownRun = null), (statuses = {}))}>{t('automatismes.editeur.effacer')}</button>
              </div>
            {/if}
            {#if runs.length}
              <ul class="run-list">
                {#each runs as r (r.id + (r.dry ? 'd' : ''))}
                  <li>
                    <button class:active={shownRun?.id === r.id} onclick={() => showRun(r)}>
                      <i class="dot {r.status}"></i>
                      <span>{r.dry ? t('automatismes.editeur.essai') : r.why}</span>
                      <small class="muted">{relative(r.started, home.now)}</small>
                    </button>
                  </li>
                {/each}
              </ul>
            {:else}
              <p class="muted small">{t('automatismes.editeur.aucune_execution')}</p>
            {/if}
          </section>

          {#if a.id}
            {#if confirming}
              <div class="confirm">
                <span>{t('automatismes.editeur.supprimer_question', { nom: a.name })}</span>
                <button class="danger" onclick={remove}>{t('automatismes.editeur.supprimer')}</button>
                <button class="link" onclick={() => (confirming = false)}>{t('commun.annuler')}</button>
              </div>
            {:else}
              <button class="link danger" onclick={() => (confirming = true)}><Icon name="trash" size={15} />{t('automatismes.editeur.supprimer_automatisme')}</button>
            {/if}
          {/if}
        {/if}
      </div>
    </div>
  </div>
{:else}
  <p class="muted">{t('automatismes.editeur.chargement')}</p>
{/if}

{#if palette}
  <Palette mode={palette.mode} onpick={pick} onclose={() => (palette = null)} />
{/if}

<style>
  .editor {
    display: grid;
    grid-template-rows: auto auto 1fr;
    gap: 14px;
    height: calc(100dvh - 56px);
  }

  .top {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }

  .back {
    width: 42px;
    height: 42px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--surface);
    color: var(--ink);
    box-shadow: var(--shadow);
  }

  .name {
    flex: 1;
    min-width: 180px;
    border: 0;
    background: none;
    font: inherit;
    font-size: 26px;
    font-weight: 700;
    letter-spacing: -0.02em;
    color: var(--ink);
    padding: 4px 6px;
    border-radius: 10px;
  }

  .name:focus {
    background: var(--surface);
    outline: none;
  }

  .pill {
    padding: 6px 12px;
    border-radius: 999px;
    font-size: 13px;
    font-weight: 700;
    background: var(--surface-2);
    color: var(--ink-2);
  }

  .pill.live {
    background: var(--good-soft);
    color: var(--good);
  }

  .pill.draft {
    background: var(--warm-soft);
    color: var(--warm-ink);
  }

  .pill.author {
    font-weight: 600;
    color: var(--ink-3);
  }

  .actions {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }

  .actions button {
    display: flex;
    align-items: center;
    gap: 8px;
    border: 0;
    border-radius: 999px;
    padding: 10px 16px;
    font-weight: 700;
  }

  .ghost {
    background: var(--surface);
    color: var(--ink);
    box-shadow: var(--shadow);
  }

  .primary {
    background: var(--ink);
    color: var(--bg);
  }

  .on {
    background: var(--good-soft);
    color: var(--good);
  }

  .sw {
    position: relative;
    width: 34px;
    height: 20px;
    border-radius: 999px;
    background: var(--good);
  }

  .sw i {
    position: absolute;
    top: 3px;
    right: 3px;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: #fff;
  }

  button:disabled {
    opacity: 0.45;
  }

  .sentence {
    display: grid;
    gap: 6px;
    padding: 16px 20px;
    border-radius: var(--r-lg);
    background: var(--surface);
    box-shadow: var(--shadow);
  }

  .sentence > p:first-child {
    font-size: 17px;
    font-weight: 600;
    line-height: 1.45;
  }

  .sentence.bad > p:first-child {
    color: var(--ink-2);
  }

  .small {
    font-size: 13.5px;
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .err {
    color: var(--alert);
    font-weight: 650;
  }

  .changed {
    color: var(--warm-ink);
    flex-wrap: wrap;
  }

  /* An agent may have named these devices: their own name and id, before approving. */
  .names {
    color: var(--warm-ink);
    flex-wrap: wrap;
  }

  .names code {
    font-size: 11.5px;
    color: var(--ink-3);
  }

  .warn {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13.5px;
    color: var(--cool);
    font-weight: 600;
  }

  .body {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 360px;
    gap: 16px;
    min-height: 0;
  }

  .canvas-wrap {
    min-height: 0;
    height: 100%;
  }

  .side {
    display: grid;
    align-content: start;
    gap: 14px;
    overflow-y: auto;
    min-height: 0;
    padding-bottom: 80px;
  }

  .moli {
    display: grid;
    gap: 10px;
  }

  .moli-head {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  textarea,
  select {
    border: 0;
    border-radius: 14px;
    padding: 10px 12px;
    background: var(--surface-2);
    font: inherit;
    color: var(--ink);
    resize: vertical;
  }

  .moli .primary {
    justify-self: end;
    border: 0;
    border-radius: 999px;
    padding: 10px 18px;
    font-weight: 700;
  }

  .add {
    width: 100%;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    border: 0;
    border-radius: 14px;
    padding: 12px;
    background: var(--surface-2);
    font-weight: 700;
    color: var(--ink);
    margin-bottom: 10px;
  }

  .mode {
    display: grid;
    gap: 6px;
    margin-top: 12px;
    font-size: 13px;
    font-weight: 650;
    color: var(--ink-2);
  }

  .run-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
  }

  .run-list button {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 9px 10px;
    border: 0;
    border-radius: 12px;
    background: none;
    text-align: left;
    color: var(--ink);
  }

  .run-list button:hover,
  .run-list button.active {
    background: var(--surface-2);
  }

  .run-list span {
    flex: 1;
    font-size: 14px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .dot {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: var(--good);
    flex: none;
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

  .run-detail {
    padding: 12px;
    border-radius: 14px;
    background: var(--surface-2);
    margin-bottom: 10px;
    font-size: 14px;
  }

  .run-detail ul {
    list-style: none;
    margin: 8px 0;
    padding: 0;
    display: grid;
    gap: 4px;
  }

  .run-detail li span {
    display: inline-block;
    min-width: 62px;
    font-weight: 700;
    color: var(--good);
  }

  .run-detail li.failed span,
  .run-detail li.no span,
  .run-detail li.timeout span {
    color: var(--alert);
  }

  .run-detail li.simulated span {
    color: var(--cool);
  }

  .run-detail em {
    font-style: normal;
    color: var(--ink-3);
  }

  .link {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    border: 0;
    background: none;
    color: var(--cool);
    font-weight: 650;
    padding: 4px 0;
  }

  .link.danger {
    color: var(--alert);
  }

  .confirm {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
    font-weight: 600;
  }

  .confirm .danger {
    border: 0;
    border-radius: 999px;
    padding: 8px 14px;
    background: var(--alert);
    color: #fff;
    font-weight: 700;
  }

  @media (max-width: 1100px) {
    .editor {
      height: auto;
    }

    .body {
      grid-template-columns: 1fr;
    }

    .canvas-wrap {
      height: 62dvh;
    }

    .side {
      overflow: visible;
    }

    .actions span {
      display: none;
    }
  }
</style>
