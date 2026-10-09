<script>
  import { onMount } from 'svelte';
  import { looks, loadAmbiances, setLook } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** A strip of ambiances for these lights (a room): one tap, the room
   *  takes the look. Lamps that cannot follow are left alone by Moli. */
  let { lights, label } = $props();

  onMount(loadAmbiances);

  // What the chip shows: the ambiance's colours, or its white.
  const swatch = (a) =>
    a.colors.length > 1
      ? `conic-gradient(${[...a.colors, a.colors[0]].join(', ')})`
      : (a.colors[0] ?? (a.white >= 4000 ? '#eef3ff' : '#ffd9a8'));
</script>

{#if looks.list?.length && lights.length}
  <div class="strip" role="group" aria-label={t('commun.ambiances', { label })}>
    {#each looks.list as a (a.id)}
      {@const busy = looks.busy === JSON.stringify({ ambiance: a.id })}
      <button class="look" class:busy onclick={() => setLook(lights, { ambiance: a.id }, t('commun.action', { label, action: a.name }))} disabled={looks.busy != null}>
        <span class="swatch" class:pale={!a.colors.length} style:background={swatch(a)}><Icon name={a.icon} size={16} /></span>
        <span class="name">{a.name}</span>
      </button>
    {/each}
  </div>
{/if}

<style>
  /* It scrolls within its parent, never widens it. */
  .strip {
    min-width: 0;
    max-width: 100%;
    display: flex;
    gap: 8px;
    overflow-x: auto;
    padding: 2px 2px 6px;
    scrollbar-width: thin;
    scroll-snap-type: x proximity;
  }

  .look {
    all: unset;
    box-sizing: border-box;
    flex: none;
    display: grid;
    justify-items: center;
    gap: 6px;
    width: 76px;
    padding: 8px 4px;
    border-radius: var(--r-md);
    background: var(--surface-2);
    cursor: pointer;
    scroll-snap-align: start;
    transition: transform 0.15s var(--ease), background 0.2s var(--ease);
  }

  .look:hover:not(:disabled) {
    transform: translateY(-2px);
  }

  .look:focus-visible {
    outline: 3px solid var(--cool);
    outline-offset: 2px;
  }

  .look:disabled {
    cursor: default;
  }

  .swatch {
    width: 38px;
    height: 38px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    color: #fff;
    box-shadow: inset 0 0 0 1px rgb(0 0 0 / 12%);
  }

  .swatch :global(svg) {
    filter: drop-shadow(0 1px 1px rgb(0 0 0 / 45%));
  }

  /* A white look: a dark icon on its pale swatch. */
  .swatch.pale {
    color: var(--ink-2);
  }

  .swatch.pale :global(svg) {
    filter: none;
  }

  .name {
    font-size: 12px;
    font-weight: 600;
    text-align: center;
    line-height: 1.2;
    color: var(--ink-2);
  }

  .busy .swatch {
    animation: pulse 0.9s ease-in-out infinite;
  }

  @keyframes pulse {
    50% {
      opacity: 0.5;
      transform: scale(0.92);
    }
  }
</style>
