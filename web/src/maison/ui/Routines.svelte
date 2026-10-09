<script>
  import { onMount } from 'svelte';
  import { note } from '../lib/home.svelte.js';
  import { api, asHuman, isTrigger, CANCELLED } from '../lib/auto.svelte.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** The routines: approved automations that only run when someone launches
   *  them (Home Assistant's scripts: « Dodo », « Départ »…). One tap each;
   *  an approved routine asks no code from the household's dashboard. */
  let list = $state([]);
  let busy = $state(null);

  const launchedOnly = (a) => {
    const triggers = a.graph.nodes.filter((n) => isTrigger(n.type));
    return triggers.length > 0 && triggers.every((n) => n.type === 'manual');
  };

  async function load() {
    try {
      const { automations } = await api.list();
      list = automations
        .filter((v) => v.approved && v.automation.enabled && launchedOnly(v.automation))
        .map((v) => ({ id: v.automation.id, name: v.automation.name, fingerprint: v.fingerprint }))
        .sort((a, b) => a.name.localeCompare(b.name, locale()));
    } catch {
      /* the card stays as it was */
    }
  }

  onMount(() => {
    load();
    const timer = setInterval(load, 120_000);
    return () => clearInterval(timer);
  });

  async function launch(r) {
    busy = r.id;
    try {
      try {
        await api.run(r.id, r.fingerprint);
      } catch (err) {
        // A dashboard not trusted as the household's: the code, then again.
        if (err.status !== 403) throw err;
        await asHuman(t('automatismes.lancer_label', { nom: r.name }), () => api.run(r.id, r.fingerprint));
      }
      note(t('automatismes.routines.lancee', { nom: r.name }), 'info');
    } catch (err) {
      if (err.status === 409) load();
      if (err.message !== CANCELLED) note(t('automatismes.routines.erreur', { nom: r.name, message: err.message }), 'error');
    } finally {
      busy = null;
    }
  }
</script>

{#if list.length}
  <section class="card routines">
    <div class="card-head">
      <h2><Icon name="hand" size={18} />{t('automatismes.routines.titre')}</h2>
      <a class="more" href="#/automatismes" aria-label={t('automatismes.routines.tous')}><Icon name="arrow-top-right" size={18} /></a>
    </div>
    <div class="buttons">
      {#each list as r (r.id)}
        <button onclick={() => launch(r)} disabled={busy === r.id} aria-busy={busy === r.id}>
          <Icon name="play-circle" size={18} />
          <span>{r.name}</span>
        </button>
      {/each}
    </div>
  </section>
{/if}

<style>
  .routines {
    margin-top: 20px;
  }

  .buttons {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
    gap: 10px;
  }

  button {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    border: 0;
    border-radius: var(--r-sm);
    padding: 12px 14px;
    background: var(--surface-2);
    color: inherit;
    font: inherit;
    font-weight: 650;
    font-size: 14px;
    text-align: left;
    cursor: pointer;
    transition: transform 0.15s var(--ease);
  }

  button:hover {
    transform: translateY(-1px);
  }

  button:disabled {
    opacity: 0.6;
    cursor: progress;
  }

  span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
