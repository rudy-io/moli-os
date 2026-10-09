<script>
  import Icon from '../ui/Icon.svelte';
  import { CATALOG, FAMILIES } from '../lib/auto.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';

  /** What can come next: triggers to start, the rest to follow. */
  let { mode = 'next', onpick, onclose } = $props();

  let search = $state('');

  const groups = $derived(
    FAMILIES.filter((f) => (mode === 'start' ? f.id === 'trigger' : true))
      .map((f) => ({
        ...f,
        items: Object.entries(CATALOG)
          .filter(([, m]) => m.family === f.id)
          .filter(([, m]) => !search || `${m.label} ${m.hint}`.toLowerCase().includes(search.toLowerCase())),
      }))
      .filter((g) => g.items.length),
  );

  function onkey(e) {
    if (e.key === 'Escape') onclose?.();
  }
</script>

<svelte:window onkeydown={onkey} />

<button class="scrim" onclick={onclose} aria-label={t('commun.fermer')}></button>
<div class="palette" role="dialog" aria-modal="true" aria-label={t('automatismes.palette.ajouter')}>
  <header>
    <h2>{mode === 'start' ? t('automatismes.palette.demarrer') : t('automatismes.palette.ensuite')}</h2>
    <button class="x" onclick={onclose} aria-label={t('commun.fermer')}><Icon name="close" size={20} /></button>
  </header>
  <input class="search" placeholder={t('automatismes.palette.chercher')} bind:value={search} aria-label={t('automatismes.palette.chercher_aria')} />
  <div class="groups">
    {#each groups as g (g.id)}
      <section class={g.id}>
        <h3>{g.label} <small>{g.hint}</small></h3>
        {#each g.items as [type, m] (type)}
          <button class="item" onclick={() => onpick?.(type)}>
            <span class="tile"><Icon name={m.icon} size={20} /></span>
            <span class="words"><b>{m.label}</b><small>{m.hint}</small></span>
          </button>
        {/each}
      </section>
    {/each}
  </div>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 44;
    border: 0;
    background: rgb(10 14 22 / 30%);
  }

  .palette {
    position: fixed;
    z-index: 45;
    left: 50%;
    top: 50%;
    transform: translate(-50%, -50%);
    width: min(560px, calc(100vw - 32px));
    max-height: min(680px, calc(100dvh - 48px));
    display: grid;
    grid-template-rows: auto auto 1fr;
    gap: 12px;
    padding: 20px;
    border-radius: var(--r-xl);
    background: var(--surface);
    box-shadow: var(--shadow-lift);
  }

  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  h2 {
    font-size: 20px;
    font-weight: 700;
  }

  .x {
    width: 38px;
    height: 38px;
    border-radius: 50%;
    border: 0;
    background: var(--surface-2);
    display: grid;
    place-items: center;
  }

  .search {
    border: 0;
    border-radius: 14px;
    padding: 12px 16px;
    background: var(--surface-2);
    font: inherit;
    color: var(--ink);
  }

  .groups {
    overflow-y: auto;
    display: grid;
    gap: 14px;
    padding-right: 4px;
  }

  h3 {
    font-size: 12px;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--ink-3);
    margin-bottom: 6px;
  }

  h3 small {
    text-transform: none;
    letter-spacing: 0;
    font-weight: 500;
    margin-left: 6px;
  }

  .item {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px;
    border: 0;
    border-radius: 14px;
    background: none;
    text-align: left;
    color: var(--ink);
  }

  .item:hover {
    background: var(--surface-2);
  }

  .tile {
    width: 40px;
    height: 40px;
    border-radius: 12px;
    display: grid;
    place-items: center;
    flex: none;
  }

  .trigger .tile {
    background: color-mix(in srgb, #e9a23b 16%, var(--surface));
    color: #e9a23b;
  }
  .logic .tile {
    background: color-mix(in srgb, #4b74f2 14%, var(--surface));
    color: #4b74f2;
  }
  .action .tile {
    background: color-mix(in srgb, #2c9a88 14%, var(--surface));
    color: #2c9a88;
  }
  .wait .tile {
    background: color-mix(in srgb, #8a92a4 16%, var(--surface));
    color: #8a92a4;
  }
  .notify .tile {
    background: color-mix(in srgb, #b05fd8 14%, var(--surface));
    color: #b05fd8;
  }
  .moli .tile {
    background: conic-gradient(from 200deg, #f5c84b, #f0a53a, #ef7f5a, #8f8cf5, #5fb4f0, #f5c84b);
    color: #fff;
  }

  .words {
    display: grid;
  }

  .words b {
    font-weight: 650;
  }

  .words small {
    color: var(--ink-3);
    font-size: 13px;
  }
</style>
