<script>
  import '@fontsource-variable/figtree';
  import './maison.css';
  import { onMount, untrack } from 'svelte';
  import { flip } from 'svelte/animate';
  import { scale } from 'svelte/transition';
  import { home, hub, start, value, readPref, savePref, watches, hidden, reloadLayout, deviceKind } from './lib/home.svelte.js';
  import { t } from '../lib/i18n.svelte.js';
  import Icon from './ui/Icon.svelte';
  import PinSheet from './ui/PinSheet.svelte';
  import ThemeSheet from './ui/ThemeSheet.svelte';
  import Correct from './moli/Correct.svelte';
  import Setup from './ui/Setup.svelte';
  import Login from './ui/Login.svelte';
  import Accueil from './pages/Accueil.svelte';
  import Salon from './pages/Salon.svelte';
  import Pieces from './pages/Pieces.svelte';
  import Plan from './pages/Plan.svelte';
  import Cameras from './pages/Cameras.svelte';
  import Alarme from './pages/Alarme.svelte';
  import { isAlarm } from './lib/alarm.js';
  import Energie from './pages/Energie.svelte';
  import Impression from './pages/Impression.svelte';
  import Pac from './pages/Pac.svelte';
  import Dehors from './pages/Dehors.svelte';
  import Chauffage from './pages/Chauffage.svelte';
  import Moli from './pages/Moli.svelte';
  import MoliSheet from './moli/MoliSheet.svelte';
  import Automatismes from './pages/Automatismes.svelte';
  import Systeme from './pages/Systeme.svelte';
  import Parler from './pages/Parler.svelte';
  import Orb from './moli/Orb.svelte';
  import { moli } from './lib/moli.svelte.js';
  import { airzonePresent } from './lib/airzone.js';
  import { printersPresent, camerasUseful, readHidden, toggleHidden, menuTabs } from './lib/tabs.js';
  import { mapUseful } from './lib/map-tab.js';
  import { isValve } from './lib/capabilities.js';
  import { scheduleSpec } from './lib/schedule.js';
  import { themeList, pickTheme, applyTheme } from './lib/themes.js';
  import { readSaved, menuSettings, orderTabs, moveTab } from './lib/menu.js';

  let { route } = $props();
  $effect(() => {
    if (hub.layoutRevision > 0) untrack(() => {void reloadLayout();});
  });

  // Shown only if the link stays down a few seconds (a blink is not news).
  let offline = $state(false);
  $effect(() => {
    if (hub.connected) {
      offline = false;
      return;
    }
    const timer = setTimeout(() => (offline = true), 3000);
    return () => clearTimeout(timer);
  });

  // `label` is a catalogue key, said where it is shown; Moli is a name.
  const NAV = [
    { path: '', label: 'commun.nav.accueil', icon: 'home' },
    { path: 'moli', name: 'Moli', orb: true },
    { path: 'salon', label: 'commun.nav.salon', icon: 'remote' },
    { path: 'pieces', label: 'commun.nav.pieces', icon: 'light-group' },
    { path: 'somfy', label: 'systeme.somfy.page_title', icon: 'shutter', shown: () => hub.drivers.some(d => d.integration === 'overkiz') },
    { path: 'airzone', label: 'airzone_ui.title', icon: 'air-conditioner', shown: () => airzonePresent(hub.drivers) },
    { path: 'plan', label: 'commun.nav.plan', icon: 'home-roof' },
    // Where people are needs their phones: no phone and nobody located, no empty map.
    { path: 'carte', label: 'commun.nav.carte', icon: 'map', shown: () => mapUseful(hub.drivers, hub.devices) },
    // A camera, or something for the Sécurité card that lives on the same page.
    { path: 'cameras', label: 'commun.nav.cameras', icon: 'cctv', shown: () => camerasUseful(hub.devices, hub.drivers, home.config) },
    { path: 'alarme', label: 'commun.nav.alarme', icon: 'shield', shown: () => Object.values(hub.devices).some(isAlarm) },
    { path: 'parler', label: 'commun.nav.parler', icon: 'talk' },
    { path: 'energie', label: 'commun.nav.energie', icon: 'bolt' },
    { path: 'pac', label: 'commun.nav.pac', icon: 'fire', shown: () => !!home.config?.heatpump?.device },
    { path: 'chauffage', label: 'commun.nav.chauffage', icon: 'fire', shown: () => Object.values(hub.devices).some((d) => deviceKind(d) === 'heating' || (isValve(d) && scheduleSpec(d))) },
    { path: 'impression', label: 'commun.nav.impression', icon: 'printer3d', shown: () => printersPresent(hub.devices) },
    { path: 'dehors', label: 'commun.nav.dehors', icon: 'flower-outline' },
    { path: 'automatismes', label: 'commun.nav.automatismes', icon: 'robot' },
    { path: 'infra', label: 'commun.nav.infra', icon: 'server' },
    // Only when something is watched: a house with nothing to watch sees no empty page.
    { path: 'parc', label: 'commun.nav.parc', icon: 'package', shown: () => Object.values(hub.devices).some((d) => !hidden(d.id) && watches(d)) },
  ];
  // The tabs the house has something for; the menu keeps those this screen did not turn off.
  const available = $derived(NAV.filter((item) => !item.shown || item.shown()));
  let hiddenTabs = $state(readHidden(readPref('maison-onglets', null)));
  // Where the menu sits, its shape and its order: this screen's, else the house's (home.json `menu`).
  let menuSaved = $state(readSaved(readPref('maison-menu', null)));
  const menu = $derived(menuSettings(menuSaved, home.config?.menu));
  const ordered = $derived(orderTabs(available, menu.order));
  const nav = $derived(menuTabs(ordered, hiddenTabs));
  let themeOpen = $state(false);

  function setMenu(change) {
    menuSaved = { ...menuSaved, ...change };
    savePref('maison-menu', JSON.stringify(menuSaved));
  }

  function moveMenuTab(path, delta) {
    setMenu({ order: moveTab(ordered, path, delta) });
  }

  /** Back to the house's order (or the natural one): this screen forgets its own. */
  function resetOrder() {
    const { order: _, ...rest } = menuSaved;
    menuSaved = rest;
    savePref('maison-menu', JSON.stringify(menuSaved));
  }

  function toggleTab(path, on) {
    hiddenTabs = toggleHidden(hiddenTabs, path, on);
    savePref('maison-onglets', JSON.stringify(hiddenTabs));
  }

  // One pill behind the current tab, gliding from tab to tab. Offsets, not
  // boxes: they give where a tab lands, not where it is mid-animation.
  let tabList = $state();
  let pill = $state({ x: 0, y: 0, w: 0, h: 0, shown: false, ready: false });

  function placePill() {
    const glyph = tabList?.querySelector('a.active .glyph');
    if (!glyph) {
      pill = { ...pill, shown: false };
      return;
    }
    // The glyph is placed within its tab (a positioned item), the item within the list.
    const item = glyph.closest('li');
    pill = {
      x: item.offsetLeft + glyph.offsetLeft,
      y: item.offsetTop + glyph.offsetTop,
      w: glyph.offsetWidth,
      h: glyph.offsetHeight,
      shown: true,
      ready: pill.ready,
    };
    // A bar or a rail longer than the screen scrolls: the current tab comes into view.
    if (tabList.scrollWidth > tabList.clientWidth || tabList.scrollHeight > tabList.clientHeight) item.scrollIntoView({ behavior: 'smooth', block: 'nearest', inline: 'nearest' });
    // The first placement is put, not slid from the corner.
    if (!pill.ready) requestAnimationFrame(() => (pill = { ...pill, ready: true }));
  }

  $effect(() => {
    void page;
    void nav;
    void menu.position;
    void menu.shape;
    // After the menu has moved or changed shape: measured once it is laid out.
    const frame = requestAnimationFrame(() => placePill());
    untrack(placePill);
    return () => cancelAnimationFrame(frame);
  });
  $effect(() => {
    if (!tabList) return;
    const observer = new ResizeObserver(() => placePill());
    observer.observe(tabList);
    return () => observer.disconnect();
  });

  let moliOpen = $state(false);
  let prefersDark = $state(false);

  onMount(() => {
    start();
    const media = matchMedia('(prefers-color-scheme: dark)');
    prefersDark = media.matches;
    const update = (e) => (prefersDark = e.matches);
    media.addEventListener('change', update);
    return () => media.removeEventListener('change', update);
  });

  // Auto: night after sunset (the house's own daylight), else the system's.
  const daylight = $derived(value(home.config?.outdoor?.weather, 'daylight'));
  const theme = $derived(
    home.theme === 'light'
      ? 'day'
      : home.theme === 'dark'
        ? 'night'
        : daylight === false
          ? 'night'
          : daylight === true
            ? 'day'
            : prefersDark
              ? 'night'
              : 'day',
  );

  // The theme this screen wears (Porcelaine, one of Moli's, or the house's from
  // home.json): its own choice, else the house's. `theme` above stays the side, day or night.
  let paletteChoice = $state(readPref('maison-palette', ''));
  const themes = $derived(themeList(home.config));
  const worn = $derived(pickTheme(themes.list, paletteChoice, home.config?.theme));
  const look = $derived(applyTheme(worn, theme));

  function setPalette(id) {
    paletteChoice = id;
    savePref('maison-palette', id);
  }

  $effect(() => {
    const root = document.documentElement.style;
    root.background = look.bg;
    document.body.style.background = 'transparent';
    return () => {
      root.background = '';
      document.body.style.background = '';
    };
  });

  function setTheme(choice) {
    home.theme = choice;
    savePref('maison-theme', choice);
  }

  const page = $derived(route.split('/')[0] ?? '');
</script>

<div class="maison" data-theme={theme} data-menu={menu.position} data-shape={menu.shape} style={look.style || undefined}>
  {#if look.wallpaper}<div class="wallpaper" style:background={look.wallpaper} aria-hidden="true"></div>{/if}
  <nav class="rail" aria-label={t('commun.nav.aria')}>
    <a class="brand" href="#/" aria-label={t('commun.nav.logo')}>
      <svg viewBox="0 0 32 32" aria-hidden="true">
        <circle cx="16" cy="16" r="15" fill="var(--sun)" />
        <circle cx="16" cy="16" r="8.5" fill="none" stroke="#161512" stroke-width="4.3" />
      </svg>
    </a>
    <ul bind:this={tabList}>
      <li
        class="pill"
        class:shown={pill.shown}
        class:ready={pill.ready}
        role="presentation"
        style:width="{pill.w}px"
        style:height="{pill.h}px"
        style:transform="translate({pill.x}px, {pill.y}px)"
      ></li>
      {#each nav as item (item.path)}
        <li animate:flip={{ duration: 320, easing: (x) => 1 - Math.pow(1 - x, 4) }} in:scale={{ start: 0.6, duration: 280 }}>
          <a
            href="#/{item.path}"
            class:active={page === item.path}
            aria-current={page === item.path ? 'page' : undefined}
            title={menu.shape === 'compact' ? (item.name ?? t(item.label)) : undefined}
          >
            <span class="glyph">
              {#if item.orb}<Orb size={22} state={moli.busy ? 'thinking' : 'idle'} />{:else}<Icon name={item.icon} size={24} />{/if}
            </span>
            <span class="label">{item.name ?? t(item.label)}</span>
          </a>
        </li>
      {/each}
    </ul>
    <div class="rail-foot">
      <button class="theme" onclick={() => (themeOpen = true)} title={t('commun.theme_sheet.bouton_titre')} aria-haspopup="dialog">
        <Icon name="palette" size={20} />
        <span class="label">{t('commun.theme_sheet.bouton')}</span>
      </button>
      <a href="#/systeme" class:active={page === 'systeme'} title={t('commun.nav.systeme_titre')}>
        <Icon name="branch" size={20} />
        <span class="label">{t('commun.nav.systeme')}</span>
      </a>
      {#if hub.session.atelier !== false}
        <a class="atelier" href="#/atelier" class:active={page === 'atelier'} title={t('commun.nav.atelier_titre')}>
          <Icon name="atelier" size={20} />
          <span class="label">{t('commun.nav.atelier')}</span>
        </a>
      {/if}
    </div>
  </nav>

  <main class="content">
    {#key page}<div class="page-in">
    {#if page === 'salon'}
      <Salon />
    {:else if page === 'pieces'}
      <Pieces {route} />
    {:else if page === 'airzone'}
      {#await import('./pages/Airzone.svelte') then { default: Airzone }}<Airzone {route} />{/await}
    {:else if page === 'plan'}
      <Plan />
    {:else if page === 'carte'}
      <!-- The map (Leaflet): loaded only when visited. -->
      {#await import('./pages/Carte.svelte') then { default: Carte }}<Carte />{/await}
    {:else if page === 'cameras'}
      <Cameras />
    {:else if page === 'parler'}
      <Parler />
    {:else if page === 'energie' && route.startsWith('energie/solaire')}
      <!-- The solar demo (a simulation): loaded only when visited. -->
      {#await import('./pages/Solaire.svelte') then { default: Solaire }}<Solaire />{/await}
    {:else if page === 'energie'}
      <Energie />
    {:else if page === 'pac'}
      <Pac />
    {:else if page === 'impression'}
      <Impression />
    {:else if page === 'dehors'}
      <Dehors />
    {:else if page === 'chauffage'}
      <Chauffage />
    {:else if page === 'moli'}
      <Moli />
    {:else if page === 'alarme'}
      <Alarme />
    {:else if page === 'automatismes'}
      <Automatismes {route} />
    {:else if page === 'somfy'}
      {#await import('./pages/Somfy.svelte') then { default: Somfy }}<Somfy />{/await}
    {:else if page === 'systeme'}
      <Systeme />
    {:else if page === 'ajouter'}
      <!-- Adding a device by its address: loaded only when visited. -->
      {#await import('./pages/Ajouter.svelte') then { default: Ajouter }}<Ajouter />{/await}
    {:else if page === 'infra'}
      <!-- The house's machines: loaded only when visited. -->
      {#await import('./pages/Infra.svelte') then { default: Infra }}<Infra />{/await}
    {:else if page === 'atelier'}
      <!-- Every device, raw (owners): loaded only when visited. -->
      {#await import('../Atelier.svelte') then { default: Atelier }}<Atelier />{/await}
    {:else if page === 'parc'}
      <!-- What the house watches beyond itself: loaded only when visited. -->
      {#await import('./pages/Parc.svelte') then { default: Parc }}<Parc />{/await}
    {:else}
      <Accueil />
    {/if}
    </div>{/key}
    <!-- On a phone the rail's foot is hidden: the same three, at the bottom. -->
    <footer class="phone-foot">
      <a href="#/systeme"><Icon name="branch" size={16} />{t('commun.nav.comment_ca_marche')}</a>
      {#if hub.session.atelier !== false}<a href="#/atelier"><Icon name="atelier" size={16} />{t('commun.nav.atelier')}</a>{/if}
      <button onclick={() => (themeOpen = true)} aria-haspopup="dialog"><Icon name="palette" size={16} />{t('commun.theme_sheet.pied')}</button>
    </footer>
  </main>

  {#if page !== 'moli'}
    <button class="orb" onclick={() => (moliOpen = true)} aria-label={t('commun.demander_moli')}>
      <Orb size={38} state={moli.busy ? 'thinking' : 'idle'} />
      <span class="orb-text">{t('commun.demander_moli')}</span>
    </button>
  {/if}

  {#if moliOpen}
    <MoliSheet onclose={() => (moliOpen = false)} />
  {/if}

  {#if moli.fix}
    <Correct />
  {/if}

  {#if themeOpen}
    <ThemeSheet
      tabs={ordered}
      {menu}
      onmenu={setMenu}
      onmove={moveMenuTab}
      onresetorder={menuSaved.order ? resetOrder : null}
      hidden={hiddenTabs}
      theme={home.theme}
      ontheme={setTheme}
      themes={themes.list}
      side={theme}
      worn={worn.id}
      houseTheme={home.config?.theme}
      problems={[...(themes.problems[worn.id] ?? []), ...look.problems]}
      onpalette={setPalette}
      ontoggle={toggleTab}
      onclose={() => (themeOpen = false)}
    />
  {/if}

  {#if home.held}
    <PinSheet />
  {/if}

  {#if hub.session?.login_required}
    <!-- From the Internet, not signed in: nothing of the house, only this. -->
    <Login />
  {/if}

  {#if hub.session?.setup && !home.setupLater}
    <Setup onlater={() => (home.setupLater = true)} />
  {/if}

  {#if offline}
    <div class="offline" role="status"><Icon name="offline" size={16} />{t('commun.hors_ligne')}</div>
  {/if}

  <div class="notices" role="status" aria-live="polite">
    {#each hub.notices as n (n.id)}
      <div class="notice">
        <span class="bell"><Icon name="bell" size={18} /></span>
        <div>
          <b>{n.title ?? n.from ?? 'Moli'}</b>
          <p>{n.message}</p>
          {#if n.title && n.from}<small>{n.from}</small>{/if}
        </div>
        <button onclick={() => (hub.notices = hub.notices.filter((x) => x.id !== n.id))} aria-label={t('commun.fermer')}><Icon name="close" size={16} /></button>
      </div>
    {/each}
  </div>

  <div class="notes" role="status" aria-live="polite">
    {#each home.notes as n (n.id)}
      <p class="note {n.tone}">{n.text}</p>
    {/each}
  </div>
</div>

<style>
  .maison {
    /* the menu's width, and its gap to the edge when it floats */
    --rail-w: 104px;
    --rail-gap: 0px;
    display: grid;
    grid-template-columns: calc(var(--rail-w) + var(--rail-gap)) minmax(0, 1fr);
  }

  .rail {
    position: sticky;
    top: 0;
    height: 100dvh;
    display: flex;
    flex-direction: column;
    align-items: center;
    padding: 22px 0 calc(18px + env(safe-area-inset-bottom)) env(safe-area-inset-left);
    gap: 18px;
  }

  .brand svg {
    width: 40px;
    height: 40px;
    display: block;
  }

  .rail ul {
    position: relative;
    list-style: none;
    margin: 12px 0 0;
    padding: 6px 0 12px;
    display: grid;
    align-content: start;
    gap: 6px;
    /* More tabs than the screen is tall: the list scrolls on its own, and the
       logo and the foot (Thème, Système, Atelier) stay in reach. */
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
    overscroll-behavior: contain;
    scrollbar-width: none;
    mask-image: linear-gradient(transparent, #000 8px, #000 calc(100% - 18px), transparent);
  }

  .rail ul::-webkit-scrollbar {
    display: none;
  }

  /* The current tab's pill: one element that glides, under the icons. */
  .rail .pill {
    position: absolute;
    top: 0;
    left: 0;
    border-radius: 999px;
    background: var(--surface);
    box-shadow: var(--shadow);
    opacity: 0;
    pointer-events: none;
    transition: opacity 0.2s var(--ease);
  }

  .rail .pill.shown {
    opacity: 1;
  }

  .rail .pill.ready {
    transition:
      transform 0.42s var(--ease),
      width 0.42s var(--ease),
      height 0.42s var(--ease),
      opacity 0.2s var(--ease);
  }

  .rail li:not(.pill) {
    position: relative;
    z-index: 1;
  }

  .rail a,
  .rail button {
    display: grid;
    justify-items: center;
    gap: 4px;
    width: 80px;
    padding: 10px 0 8px;
    border-radius: 20px;
    border: 0;
    background: none;
    color: var(--ink-3);
    text-decoration: none;
    transition: color 0.2s var(--ease), background 0.2s var(--ease);
  }

  .rail .glyph {
    width: 52px;
    height: 34px;
    border-radius: 999px;
    display: grid;
    place-items: center;
    transition:
      background 0.25s var(--ease),
      transform 0.25s var(--ease);
  }

  .rail a.active {
    color: var(--ink);
  }

  .rail a:hover {
    color: var(--ink-2);
  }

  .rail a:not(.active):hover .glyph {
    background: color-mix(in srgb, var(--surface) 45%, transparent);
  }

  .rail a:active .glyph {
    transform: scale(0.92);
  }

  /* A new page comes in softly instead of snapping. */
  .page-in {
    /* backwards, not both: once in, no transform is left to trap a page's fixed sheets. */
    animation: page-in 0.32s var(--ease) backwards;
  }

  @keyframes page-in {
    from {
      opacity: 0;
      transform: translateY(6px);
    }
  }

  .label {
    font-size: 12px;
    font-weight: 600;
  }

  .rail-foot {
    margin-top: auto;
    display: grid;
    gap: 4px;
  }

  /* Compact: icons alone; the name stays for screen readers and in the tooltip. */
  .maison[data-shape='compact'] .rail .label {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }

  /* Floating: the menu is a card off the edge; the pill shows on its surface. */
  .maison[data-shape='floating'] .rail .pill {
    background: var(--surface-2);
    box-shadow: inset 0 0 0 1px var(--line);
  }

  /* A short screen: tighter tabs, more of them in view. */
  @media (min-width: 761px) and (max-height: 860px) {
    .rail {
      gap: 10px;
      padding-top: 14px;
    }

    .rail ul {
      gap: 2px;
    }

    .rail a,
    .rail button {
      padding: 6px 0 5px;
    }
  }

  @media (min-width: 761px) {
    /* On the right. */
    .maison[data-menu='right'] {
      grid-template-columns: minmax(0, 1fr) calc(var(--rail-w) + var(--rail-gap));
    }

    .maison[data-menu='right'] .rail {
      grid-column: 2;
      grid-row: 1;
      padding-left: 0;
      padding-right: env(safe-area-inset-right);
    }

    .maison[data-menu='right'] .content {
      grid-column: 1;
      grid-row: 1;
      padding-left: 36px;
      padding-right: 8px;
    }

    .maison[data-menu='right'] .orb {
      right: calc(var(--rail-w) + var(--rail-gap) + 20px);
    }

    /* A bar along the top or the bottom. */
    .maison[data-menu='top'],
    .maison[data-menu='bottom'] {
      grid-template-columns: minmax(0, 1fr);
    }

    .maison[data-menu='top'] .rail,
    .maison[data-menu='bottom'] .rail {
      flex-direction: row;
      height: auto;
      width: 100%;
      padding: 8px max(24px, env(safe-area-inset-right)) 8px max(24px, env(safe-area-inset-left));
      gap: 14px;
      z-index: 25;
    }

    .maison[data-menu='top'] .rail {
      position: sticky;
      top: 0;
      background: color-mix(in srgb, var(--bg) 86%, transparent);
      backdrop-filter: blur(14px);
    }

    .maison[data-menu='bottom'] .rail {
      position: fixed;
      inset: auto 0 0 0;
      padding-bottom: calc(8px + env(safe-area-inset-bottom));
      background: var(--surface);
      box-shadow: var(--shadow-lift);
    }

    .maison[data-menu='top'] .rail ul,
    .maison[data-menu='bottom'] .rail ul {
      grid-auto-flow: column;
      grid-auto-columns: max-content;
      justify-content: safe center;
      margin: 0;
      padding: 0 10px;
      gap: 2px;
      overflow-x: auto;
      overflow-y: hidden;
      mask-image: linear-gradient(90deg, transparent, #000 10px, #000 calc(100% - 10px), transparent);
    }

    .maison[data-menu='top'] .rail a,
    .maison[data-menu='top'] .rail button,
    .maison[data-menu='bottom'] .rail a,
    .maison[data-menu='bottom'] .rail button {
      width: 76px;
      padding: 6px 0 5px;
    }

    .maison[data-menu='top'] .rail-foot,
    .maison[data-menu='bottom'] .rail-foot {
      margin: 0;
      display: flex;
      gap: 2px;
    }

    .maison[data-menu='top'] .content,
    .maison[data-menu='bottom'] .content {
      padding: 24px 36px 120px;
      margin: 0 auto;
    }

    .maison[data-menu='bottom'] .content {
      padding-bottom: 160px;
    }

    .maison[data-menu='bottom'] .orb,
    .maison[data-menu='bottom'] .notes {
      bottom: 112px;
    }

    /* Compact. */
    .maison[data-shape='compact'] {
      --rail-w: 80px;
    }

    .maison[data-shape='compact'] .rail a,
    .maison[data-shape='compact'] .rail button {
      width: 60px;
      padding: 7px 0;
    }

    .maison[data-shape='compact'] .rail .glyph {
      width: 48px;
      height: 40px;
    }

    /* Floating. */
    .maison[data-shape='floating'] {
      --rail-gap: 14px;
    }

    .maison[data-shape='floating'] .rail {
      top: 14px;
      height: calc(100dvh - 28px);
      margin: 14px 0 14px 14px;
      padding: 16px 0 12px;
      border-radius: var(--r-xl);
      background: var(--surface);
      box-shadow: var(--shadow-lift);
    }

    .maison[data-shape='floating'][data-menu='right'] .rail {
      margin: 14px 14px 14px 0;
    }

    .maison[data-shape='floating'][data-menu='top'] .rail {
      top: 12px;
      width: auto;
      height: auto;
      margin: 12px 16px 0;
      padding: 6px 18px;
      backdrop-filter: none;
    }

    .maison[data-shape='floating'][data-menu='bottom'] .rail {
      inset: auto 16px calc(16px + env(safe-area-inset-bottom)) 16px;
      width: auto;
      height: auto;
      margin: 0;
      padding: 6px 18px;
    }

    .maison[data-shape='floating'][data-menu='bottom'] .orb,
    .maison[data-shape='floating'][data-menu='bottom'] .notes {
      bottom: 124px;
    }
  }

  .rail-foot a,
  .rail-foot button {
    padding: 8px 0;
  }

  .content {
    padding: 28px 36px 120px 8px;
    max-width: 1440px;
    width: 100%;
    min-width: 0;
  }

  /* Moli, always one tap away. */
  .orb {
    position: fixed;
    right: 28px;
    bottom: 28px;
    z-index: 30;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 22px 10px 10px;
    border: 0;
    border-radius: 999px;
    background: var(--surface);
    box-shadow: var(--shadow-lift);
    font-weight: 650;
    transition: transform 0.2s var(--ease);
  }

  .orb:hover {
    transform: translateY(-2px);
  }

  .notes {
    position: fixed;
    left: 50%;
    bottom: 28px;
    transform: translateX(-50%);
    display: grid;
    gap: 8px;
    z-index: 40;
    pointer-events: none;
  }

  /* The link to Moli dropped (a restart, the Wi-Fi): say so instead of a
     dashboard that silently stops moving. */
  .offline {
    position: fixed;
    top: 12px;
    left: 50%;
    transform: translateX(-50%);
    z-index: 60;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 14px;
    border-radius: 999px;
    background: var(--warm);
    color: var(--warm-deep);
    font-size: 13.5px;
    font-weight: 600;
    box-shadow: var(--shadow);
  }

  .phone-foot {
    display: none;
  }

  .notices {
    position: fixed;
    top: 18px;
    right: 18px;
    z-index: 42;
    display: grid;
    gap: 10px;
    width: min(380px, calc(100vw - 32px));
  }

  .notice {
    display: flex;
    gap: 12px;
    align-items: flex-start;
    padding: 14px 12px 14px 14px;
    border-radius: var(--r-md);
    background: var(--surface);
    box-shadow: var(--shadow-lift), inset 0 0 0 1px var(--line);
    animation: drop 0.4s var(--ease) both;
  }

  .notice .bell {
    width: 36px;
    height: 36px;
    flex: none;
    border-radius: 12px;
    display: grid;
    place-items: center;
    background: color-mix(in srgb, #b05fd8 14%, var(--surface));
    color: #b05fd8;
  }

  .notice div {
    flex: 1;
    display: grid;
    gap: 2px;
  }

  .notice p {
    font-size: 14.5px;
    line-height: 1.4;
  }

  .notice small {
    color: var(--ink-3);
    font-size: 12px;
  }

  .notice button {
    border: 0;
    background: none;
    color: var(--ink-3);
    padding: 2px;
  }

  @keyframes drop {
    from {
      opacity: 0;
      transform: translateY(-10px);
    }
  }

  .note {
    background: var(--ink);
    color: var(--bg);
    padding: 12px 18px;
    border-radius: 16px;
    font-weight: 550;
    box-shadow: var(--shadow-lift);
  }

  .note.error {
    background: var(--alert);
    color: var(--on-alert);
  }

  /* Phones: the rail becomes a bottom bar. */
  @media (max-width: 760px) {
    /* minmax(0, …): a `1fr` column grows to the widest page content and
       pushes every page sideways on a phone. */
    .maison {
      grid-template-columns: minmax(0, 1fr);
    }

    .rail {
      position: fixed;
      inset: auto 0 0 0;
      height: auto;
      flex-direction: row;
      justify-content: center;
      /* Above the home indicator and clear of the screen's edges: a swipe
         along the very bottom switches apps, one from the side goes back. */
      padding: 8px max(12px, env(safe-area-inset-right)) calc(14px + env(safe-area-inset-bottom))
        max(12px, env(safe-area-inset-left));
      background: var(--surface);
      box-shadow: var(--shadow-lift);
      z-index: 25;
    }

    .rail .brand,
    .rail-foot {
      display: none;
    }

    .rail ul {
      grid-auto-flow: column;
      grid-auto-columns: 62px;
      width: 100%;
      margin: 0;
      gap: 0;
      overflow-x: auto;
      overflow-y: hidden;
      overscroll-behavior-x: contain;
      scrollbar-width: none;
      justify-content: safe center;
      padding: 0;
      mask-image: linear-gradient(90deg, transparent, #000 8px, #000 calc(100% - 8px), transparent);
    }

    .maison[data-shape='compact'] .rail ul {
      grid-auto-columns: 54px;
    }

    /* The bar at the top of a phone. */
    .maison[data-menu='top'] .rail {
      inset: 0 0 auto 0;
      padding-top: calc(8px + env(safe-area-inset-top));
      padding-bottom: 8px;
    }

    .maison[data-menu='top'] .content {
      padding-top: calc(84px + env(safe-area-inset-top));
      padding-bottom: calc(110px + env(safe-area-inset-bottom));
    }

    .maison[data-menu='top'] .orb {
      bottom: calc(20px + env(safe-area-inset-bottom));
    }

    .maison[data-menu='top'] .notes {
      bottom: calc(90px + env(safe-area-inset-bottom));
    }

    /* A floating bar, off the screen's edges. */
    .maison[data-shape='floating'] .rail {
      inset: auto 10px calc(10px + env(safe-area-inset-bottom)) 10px;
      padding: 6px 6px;
      border-radius: var(--r-xl);
    }

    .maison[data-shape='floating'][data-menu='top'] .rail {
      inset: calc(10px + env(safe-area-inset-top)) 10px auto 10px;
    }

    .rail a {
      width: auto;
      padding: 4px 0;
    }

    .label {
      font-size: 10.5px;
      max-width: 60px;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }

    .rail .glyph {
      width: 44px;
      height: 30px;
    }

    .content {
      padding: 20px 16px calc(150px + env(safe-area-inset-bottom));
    }

    .phone-foot {
      display: flex;
      flex-wrap: wrap;
      justify-content: center;
      gap: 8px 18px;
      margin-top: 28px;
      font-size: 13px;
    }

    .phone-foot a,
    .phone-foot button {
      display: inline-flex;
      align-items: center;
      gap: 6px;
      border: none;
      background: none;
      color: var(--ink-3);
      font: inherit;
      text-decoration: none;
      cursor: pointer;
    }

    .orb {
      right: 16px;
      bottom: calc(96px + env(safe-area-inset-bottom));
      padding: 8px;
    }

    .orb-text {
      display: none;
    }

    .notes {
      bottom: calc(160px + env(safe-area-inset-bottom));
      width: calc(100% - 32px);
    }
  }
</style>
