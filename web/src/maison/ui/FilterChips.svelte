<script>
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import { CATEGORIES, categoryOf, saveFilter } from '../lib/filters.js';

  /**
   * Chips to see only some kinds of devices. `devices`: those concerned
   * (only their categories show, with how many); `filter`: the chosen ids,
   * none = everything; `key`: where this viewer's choice is kept.
   */
  let { devices, filter = $bindable([]), key } = $props();

  const counts = $derived.by(() => {
    const n = new Map();
    for (const d of devices) n.set(categoryOf(d), (n.get(categoryOf(d)) ?? 0) + 1);
    return n;
  });
  const present = $derived(CATEGORIES.filter((c) => counts.has(c.id)));

  function set(next) {
    filter = next;
    saveFilter(key, next);
  }

  function flip(id) {
    set(filter.includes(id) ? filter.filter((f) => f !== id) : [...filter, id]);
  }
</script>

{#if present.length > 1}
  <div class="filters" role="group" aria-label={t('commun.filtre.ne_voir_que')}>
    <button class:active={!filter.length} aria-pressed={!filter.length} onclick={() => set([])}>{t('commun.filtre.tout')}</button>
    {#each present as c (c.id)}
      <button class:active={filter.includes(c.id)} aria-pressed={filter.includes(c.id)} onclick={() => flip(c.id)}>
        <Icon name={c.icon} size={15} />{t(c.label)}<small>{counts.get(c.id)}</small>
      </button>
    {/each}
  </div>
{/if}

<style>
  .filters {
    display: flex;
    gap: 6px;
    overflow-x: auto;
    padding-bottom: 2px;
    scrollbar-width: none;
  }

  .filters::-webkit-scrollbar {
    display: none;
  }

  button {
    display: inline-flex;
    flex: none;
    align-items: center;
    gap: 6px;
    padding: 7px 12px;
    border-radius: 999px;
    border: 1px solid var(--line);
    background: var(--surface);
    color: var(--ink-2);
    font: inherit;
    font-size: 13px;
    font-weight: 600;
    white-space: nowrap;
    cursor: pointer;
  }

  button.active {
    border-color: var(--ink);
    background: var(--ink);
    color: var(--surface);
  }

  small {
    color: inherit;
    opacity: 0.6;
    font-weight: 700;
  }
</style>
