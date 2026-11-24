<script>
  import { t, i18n } from '../../lib/i18n.svelte.js';
  import { applyTheme, themeName } from '../lib/themes.js';
  import {
    mdiThemeLightDark,
    mdiWhiteBalanceSunny,
    mdiMoonWaningCrescent,
    mdiDockLeft,
    mdiDockRight,
    mdiDockTop,
    mdiDockBottom,
    mdiFormatListText,
    mdiDotsGrid,
    mdiCardOutline,
    mdiChevronUp,
    mdiChevronDown,
  } from '@mdi/js';
  import Icon from './Icon.svelte';
  import SegmentedControl from './SegmentedControl.svelte';
  import Orb from '../moli/Orb.svelte';
  import { KEPT } from '../lib/tabs.js';

  /** This screen's theme: day or night, its colours, and which tabs the menu keeps.
   *  `tabs` are those the house has something for; a tab it has nothing for
   *  (no camera yet) is not listed and comes back on its own. `themes` are
   *  Moli's and the house's (home.json); `worn` the one this screen wears,
   *  `side` 'day' or 'night' (what the previews show), `problems` what Moli
   *  ignored or adjusted in it. */
  let {
    tabs,
    hidden,
    theme,
    ontheme,
    ontoggle,
    onclose,
    themes = [],
    worn = 'porcelaine',
    side = 'day',
    houseTheme = null,
    problems = [],
    onpalette = () => {},
    menu = { position: 'left', shape: 'classic' },
    onmenu = () => {},
    onmove = () => {},
    onresetorder = null,
  } = $props();

  // A narrow screen: the menu's choices show their icons alone (the name stays for screen readers).
  let width = $state(1024);
  const narrow = $derived(width < 520);

  const label = (item) => (item.builtin ? t(item.builtin) : themeName(item, i18n.language));
  const previews = $derived(themes.map((item) => ({ item, look: applyTheme(item, side) })));

  /** One line for whoever wrote the theme: what Moli did not take as written. */
  function say(p) {
    if (p.code === 'contrast') return t('commun.theme_sheet.probleme.contrast', { key: p.key, against: p.against });
    return t(`commun.theme_sheet.probleme.${p.code}`, { key: p.key, value: p.value ?? '' });
  }

  function onkey(e) {
    if (e.key === 'Escape') onclose();
  }
</script>

<svelte:window onkeydown={onkey} bind:innerWidth={width} />

<div class="scrim" onclick={onclose} aria-hidden="true"></div>
<div class="sheet" role="dialog" aria-modal="true" aria-labelledby="theme-title">
  <header>
    <h2 id="theme-title">{t('commun.theme_sheet.titre')}</h2>
    <button class="close" onclick={onclose} aria-label={t('commun.fermer')}><Icon name="close" size={20} /></button>
  </header>

  <section>
    <h3>{t('commun.theme_sheet.apparence')}</h3>
    <SegmentedControl
      label={t('commun.theme_sheet.apparence')}
      value={theme}
      onchange={ontheme}
      segments={[
        { value: 'auto', label: t('commun.theme.auto'), icon: mdiThemeLightDark },
        { value: 'light', label: t('commun.theme.jour'), icon: mdiWhiteBalanceSunny },
        { value: 'dark', label: t('commun.theme.nuit'), icon: mdiMoonWaningCrescent },
      ]}
    />
    <p class="hint">{theme === 'light' ? t('commun.theme.titre_jour') : theme === 'dark' ? t('commun.theme.titre_nuit') : t('commun.theme.titre_auto')}</p>
  </section>

  {#if themes.length > 1}
    <section>
      <h3>{t('commun.theme_sheet.couleurs')}</h3>
      <div class="palettes" role="radiogroup" aria-label={t('commun.theme_sheet.couleurs')}>
        {#each previews as { item, look } (item.id)}
          {@const k = look.tokens}
          <button
            class="palette"
            role="radio"
            aria-checked={item.id === worn}
            onclick={() => onpalette(item.id)}
          >
            <span class="mini" style:background={look.wallpaper ?? k.bg}>
              <span class="card" style:background={k.surface} style:border-radius="{Math.round(9 * look.corners)}px">
                <span class="line" style:background={k.ink}></span>
                <span class="line short" style:background={k['ink-3']}></span>
                <span class="dots">
                  <i style:background={k.warm}></i><i style:background={k.cool}></i><i style:background={k.good}></i><i style:background={k.alert}></i>
                </span>
              </span>
            </span>
            <span class="pname" style:font-family={look.font}>{label(item)}</span>
            {#if item.id === houseTheme}<span class="house">{t('commun.theme_sheet.de_la_maison')}</span>{/if}
          </button>
        {/each}
      </div>
      {#if problems.length}
        <details class="problems">
          <summary>{t('commun.theme_sheet.ajuste', { count: problems.length })}</summary>
          <ul>
            {#each problems as p, i (i)}<li>{say(p)}</li>{/each}
          </ul>
        </details>
      {/if}
    </section>
  {/if}

  <section class="menu">
    <h3>{t('commun.theme_sheet.menu')}</h3>
    <span class="sub">{t('commun.theme_sheet.position')}</span>
    <SegmentedControl
      label={t('commun.theme_sheet.position')}
      compact={narrow}
      value={menu.position}
      onchange={(position) => onmenu({ position })}
      segments={[
        { value: 'left', label: t('commun.theme_sheet.positions.left'), icon: mdiDockLeft },
        { value: 'right', label: t('commun.theme_sheet.positions.right'), icon: mdiDockRight },
        { value: 'top', label: t('commun.theme_sheet.positions.top'), icon: mdiDockTop },
        { value: 'bottom', label: t('commun.theme_sheet.positions.bottom'), icon: mdiDockBottom },
      ]}
    />
    <span class="sub">{t('commun.theme_sheet.forme')}</span>
    <SegmentedControl
      label={t('commun.theme_sheet.forme')}
      compact={narrow}
      value={menu.shape}
      onchange={(shape) => onmenu({ shape })}
      segments={[
        { value: 'classic', label: t('commun.theme_sheet.formes.classic'), icon: mdiFormatListText },
        { value: 'compact', label: t('commun.theme_sheet.formes.compact'), icon: mdiDotsGrid },
        { value: 'floating', label: t('commun.theme_sheet.formes.floating'), icon: mdiCardOutline },
      ]}
    />
    <p class="hint">{t('commun.theme_sheet.menu_aide')}</p>
  </section>

  <section>
    <h3>{t('commun.theme_sheet.onglets')}</h3>
    <p class="hint">{t('commun.theme_sheet.onglets_aide')}</p>
    <ul>
      {#each tabs as tab, i (tab.path)}
        {@const on = !hidden.includes(tab.path)}
        {@const name = tab.name ?? t(tab.label)}
        <li class:off={!on}>
          <span class="glyph">
            {#if tab.orb}<Orb size={20} state="idle" />{:else}<Icon name={tab.icon} size={22} />{/if}
          </span>
          <span class="name">{name}</span>
          {#if tab.path === KEPT}
            <span class="kept">{t('commun.theme_sheet.toujours')}</span>
          {:else}
            <span class="moves">
              <button
                class="move"
                disabled={i <= 1}
                aria-label={t('commun.theme_sheet.monter', { name })}
                title={t('commun.theme_sheet.monter', { name })}
                onclick={() => onmove(tab.path, -1)}><Icon path={mdiChevronUp} size={20} /></button>
              <button
                class="move"
                disabled={i >= tabs.length - 1}
                aria-label={t('commun.theme_sheet.descendre', { name })}
                title={t('commun.theme_sheet.descendre', { name })}
                onclick={() => onmove(tab.path, 1)}><Icon path={mdiChevronDown} size={20} /></button>
            </span>
            <button
              class="switch"
              role="switch"
              aria-checked={on}
              aria-label={t('commun.theme_sheet.montrer', { name })}
              onclick={() => ontoggle(tab.path, !on)}
            >
              <span class="knob"></span>
            </button>
          {/if}
        </li>
      {/each}
    </ul>
    {#if onresetorder}
      <button class="reset" onclick={onresetorder}>{t('commun.theme_sheet.ordre_origine')}</button>
    {/if}
  </section>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: rgb(10 14 22 / 45%);
    backdrop-filter: blur(4px);
    z-index: 50;
    animation: fade 0.25s var(--ease) both;
  }

  .sheet {
    position: fixed;
    left: 50%;
    top: 50%;
    transform: translate(-50%, -50%);
    z-index: 51;
    width: min(440px, calc(100vw - 32px));
    max-height: calc(100dvh - 48px);
    overflow-y: auto;
    overscroll-behavior: contain;
    background: var(--surface);
    border-radius: var(--r-xl);
    box-shadow: var(--shadow-lift);
    padding: 22px 22px 18px;
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 22px;
    animation: rise 0.35s var(--ease) both;
  }

  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  h2 {
    font-size: 21px;
    font-weight: 700;
  }

  h3 {
    font-size: 13px;
    font-weight: 650;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--ink-3);
    margin-bottom: 10px;
  }

  .close {
    border: 0;
    background: var(--surface-2);
    color: var(--ink-2);
    width: 40px;
    height: 40px;
    border-radius: 50%;
    display: grid;
    place-items: center;
  }

  .hint {
    color: var(--ink-3);
    font-size: 13.5px;
    margin: 8px 0 0;
  }

  h3 + .hint {
    margin: -4px 0 10px;
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
  }

  li {
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: 52px;
    padding: 4px 6px 4px 4px;
    border-radius: var(--r-sm);
    transition: opacity 0.25s var(--ease);
  }

  li.off {
    opacity: 0.55;
  }

  .glyph {
    width: 40px;
    height: 40px;
    flex: none;
    border-radius: 12px;
    display: grid;
    place-items: center;
    background: var(--surface-2);
    color: var(--ink-2);
  }

  .name {
    flex: 1;
    font-weight: 600;
  }

  .kept {
    color: var(--ink-3);
    font-size: 13px;
  }

  .switch {
    width: 52px;
    height: 32px;
    flex: none;
    border: 0;
    border-radius: 999px;
    background: var(--surface-3);
    padding: 3px;
    display: flex;
    cursor: pointer;
    transition: background 0.25s var(--ease);
  }

  .switch .knob {
    width: 26px;
    height: 26px;
    border-radius: 50%;
    background: var(--surface);
    box-shadow: 0 1px 3px rgb(0 0 0 / 25%);
    transition: transform 0.3s var(--ease);
  }

  .switch[aria-checked='true'] {
    background: var(--warm);
  }

  .switch[aria-checked='true'] .knob {
    transform: translateX(20px);
  }

  .menu {
    display: grid;
    justify-items: start;
  }

  .sub {
    font-size: 13px;
    font-weight: 600;
    color: var(--ink-2);
    margin: 4px 0 6px;
  }

  .sub ~ .sub {
    margin-top: 14px;
  }

  .moves {
    display: flex;
    gap: 2px;
    margin-right: 6px;
  }

  .move {
    width: 34px;
    height: 34px;
    border: 0;
    border-radius: 50%;
    background: transparent;
    color: var(--ink-3);
    display: grid;
    place-items: center;
    cursor: pointer;
  }

  .move:hover:not(:disabled) {
    background: var(--surface-2);
    color: var(--ink);
  }

  .move:disabled {
    opacity: 0.3;
    cursor: default;
  }

  .reset {
    margin-top: 10px;
    border: 0;
    background: none;
    color: var(--ink-3);
    font-size: 13.5px;
    text-decoration: underline;
    cursor: pointer;
    padding: 4px 0;
  }

  .palettes {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(118px, 1fr));
    gap: 10px;
  }

  .palette {
    border: 0;
    background: transparent;
    color: var(--ink);
    padding: 4px;
    border-radius: var(--r-md);
    display: grid;
    gap: 6px;
    justify-items: start;
    text-align: left;
    cursor: pointer;
  }

  .mini {
    width: 100%;
    aspect-ratio: 16 / 10;
    border-radius: var(--r-sm);
    padding: 10px;
    display: flex;
    align-items: flex-end;
    box-shadow: inset 0 0 0 1px var(--line);
    transition: box-shadow 0.25s var(--ease);
  }

  .palette[aria-checked='true'] .mini {
    box-shadow: 0 0 0 3px var(--warm);
  }

  .card {
    width: 100%;
    padding: 8px;
    display: grid;
    gap: 5px;
    box-shadow: 0 1px 3px rgb(0 0 0 / 12%);
  }

  .line {
    height: 5px;
    width: 70%;
    border-radius: 3px;
  }

  .line.short {
    width: 45%;
  }

  .dots {
    display: flex;
    gap: 4px;
    margin-top: 2px;
  }

  .dots i {
    width: 10px;
    height: 10px;
    border-radius: 50%;
  }

  .pname {
    font-weight: 600;
    font-size: 14.5px;
    padding: 0 2px;
  }

  .house {
    color: var(--ink-3);
    font-size: 12px;
    margin-top: -4px;
    padding: 0 2px;
  }

  .problems {
    margin-top: 10px;
    color: var(--ink-3);
    font-size: 13px;
  }

  .problems summary {
    cursor: pointer;
  }

  .problems ul {
    display: block;
    padding-left: 18px;
    margin-top: 6px;
    list-style: disc;
  }

  .problems li {
    display: list-item;
    min-height: 0;
    padding: 2px 0;
  }

  @keyframes fade {
    from {
      opacity: 0;
    }
  }

  @keyframes rise {
    from {
      opacity: 0;
      transform: translate(-50%, calc(-50% + 16px)) scale(0.98);
    }
  }

  /* Phones: a sheet from the bottom, under the thumb. */
  @media (max-width: 760px) {
    .sheet {
      top: auto;
      bottom: 0;
      transform: translateX(-50%);
      width: 100%;
      max-height: 88dvh;
      border-radius: var(--r-xl) var(--r-xl) 0 0;
      padding-bottom: calc(18px + env(safe-area-inset-bottom));
      animation-name: up;
    }

    @keyframes up {
      from {
        transform: translate(-50%, 40px);
        opacity: 0;
      }
    }
  }
</style>
