<script>
  import { home, device, value, act } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** The house's scenes (device `scenes:maison`): one tap, the lights of
   *  several rooms take the look; the one in place is marked. Names, icons
   *  and colours come from the house's layout (`scenes` in `home.json`),
   *  which also picks which ones show and in which order. With a rhythm of
   *  the day, a switch says whether lights follow it. */
  const ID = 'scenes:maison';
  const d = $derived(device(ID));
  const point = (key) => d?.points.find((p) => p.key === key && p.access?.write);
  const ids = $derived((point('scene')?.kind?.values ?? []).filter((v) => v !== 'none'));
  const wordy = (id) => id.charAt(0).toUpperCase() + id.slice(1).replaceAll('-', ' ');
  const list = $derived.by(() => {
    const laid = (home.config?.scenes ?? []).filter((s) => ids.includes(s.id));
    return (laid.length ? laid : ids.map((id) => ({ id }))).map((s) => ({
      id: s.id,
      name: s.name ?? wordy(s.id),
      icon: s.icon ?? 'palette',
      colors: s.colors ?? [],
    }));
  });
  const now = $derived(value(ID, 'scene'));
  const hasRhythm = $derived(!!point('rhythm'));
  const rhythm = $derived(value(ID, 'rhythm') === true);

  const swatch = (s) =>
    s.colors.length > 1 ? `conic-gradient(${[...s.colors, s.colors[0]].join(', ')})` : (s.colors[0] ?? 'var(--surface-3)');
</script>

{#if list.length || hasRhythm}
  <div class="scenes">
    <div class="head">
      <span class="title"><Icon name="movie" size={16} />{t('maison.accueil.scenes')}</span>
      {#if hasRhythm}
        <button class="rhythm" class:on={rhythm} onclick={() => act(`${ID}/rhythm`, !rhythm, t('maison.accueil.rythme'))} aria-pressed={rhythm}>
          <Icon name="sunset" size={16} /><span>{t('maison.accueil.rythme')}</span>
        </button>
      {/if}
    </div>
    {#if list.length}
      <div class="strip" role="group" aria-label={t('maison.accueil.scenes')}>
        {#each list as s (s.id)}
          <button class="look" class:on={now === s.id} onclick={() => act(`${ID}/scene`, s.id, s.name)} aria-pressed={now === s.id}>
            <span class="swatch" class:pale={!s.colors.length} style:background={swatch(s)}><Icon name={s.icon} size={16} /></span>
            <span class="name">{s.name}</span>
          </button>
        {/each}
      </div>
    {/if}
  </div>
{/if}

<style>
  .scenes {
    grid-template-columns: minmax(0, 1fr);
    margin-top: 18px;
    padding-top: 16px;
    border-top: 1px solid var(--line);
    display: grid;
    gap: 12px;
  }

  .head {
    min-width: 0;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px 14px;
    flex-wrap: wrap;
  }

  .title {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-weight: 700;
    font-size: 14px;
  }

  .rhythm {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 6px 12px;
    border: none;
    border-radius: 999px;
    background: var(--surface-3);
    color: var(--ink-2);
    font: inherit;
    font-size: 13px;
    font-weight: 600;
    cursor: pointer;
  }

  .rhythm.on {
    background: var(--warm);
    color: #3a2a10;
  }

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
    transition:
      transform 0.15s var(--ease),
      background 0.2s var(--ease);
  }

  .look:hover {
    transform: translateY(-2px);
  }

  .look:focus-visible,
  .rhythm:focus-visible {
    outline: 3px solid var(--cool);
    outline-offset: 2px;
  }

  .look.on {
    background: var(--warm-soft);
    box-shadow: inset 0 0 0 2px var(--warm);
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
</style>
