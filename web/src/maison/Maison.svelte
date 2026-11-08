<script>
  import '@fontsource-variable/figtree';
  import './maison.css';
  import { onMount, untrack } from 'svelte';
  import { home, hub, start, value, savePref, watches, hidden, reloadLayout } from './lib/home.svelte.js';
  import { t } from '../lib/i18n.svelte.js';
  import Icon from './ui/Icon.svelte';
  import PinSheet from './ui/PinSheet.svelte';
  import Correct from './moli/Correct.svelte';
  import Setup from './ui/Setup.svelte';
  import Login from './ui/Login.svelte';
  import Accueil from './pages/Accueil.svelte';
  import Salon from './pages/Salon.svelte';
  import Pieces from './pages/Pieces.svelte';
  import Plan from './pages/Plan.svelte';
  import Cameras from './pages/Cameras.svelte';
  import Energie from './pages/Energie.svelte';
  import Impression from './pages/Impression.svelte';
  import Dehors from './pages/Dehors.svelte';
  import Moli from './pages/Moli.svelte';
  import MoliSheet from './moli/MoliSheet.svelte';
  import Automatismes from './pages/Automatismes.svelte';
  import Systeme from './pages/Systeme.svelte';
  import Parler from './pages/Parler.svelte';
  import Orb from './moli/Orb.svelte';
  import { moli } from './lib/moli.svelte.js';
  import { airzonePresent } from './lib/airzone.js';
  import { icon } from './lib/icons.js';

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
    { path: 'airzone', label: 'airzone_ui.title', icon: 'air-conditioner', shown: () => airzonePresent(hub.drivers) },
    { path: 'plan', label: 'commun.nav.plan', icon: 'home-roof' },
    { path: 'carte', label: 'commun.nav.carte', icon: 'map' },
    { path: 'cameras', label: 'commun.nav.cameras', icon: 'cctv' },
    { path: 'parler', label: 'commun.nav.parler', icon: 'talk' },
    { path: 'energie', label: 'commun.nav.energie', icon: 'bolt' },
    { path: 'impression', label: 'commun.nav.impression', icon: 'printer3d' },
    { path: 'dehors', label: 'commun.nav.dehors', icon: 'flower-outline' },
    { path: 'automatismes', label: 'commun.nav.automatismes', icon: 'robot' },
    { path: 'infra', label: 'commun.nav.infra', icon: 'server' },
    // Only when something is watched: a house with nothing to watch sees no empty page.
    { path: 'parc', label: 'commun.nav.parc', icon: 'package', shown: () => Object.values(hub.devices).some((d) => !hidden(d.id) && watches(d)) },
  ];
  const nav = $derived(NAV.filter((item) => !item.shown || item.shown()));

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

  $effect(() => {
    const root = document.documentElement.style;
    root.background = theme === 'night' ? '#0d1016' : '#edf0f5';
    document.body.style.background = 'transparent';
    return () => {
      root.background = '';
      document.body.style.background = '';
    };
  });

  // The theme button says the preference; a forced one says its own side.
  const themeName = $derived(home.theme === 'auto' ? t('commun.theme.auto') : theme === 'night' ? t('commun.theme.nuit') : t('commun.theme.jour'));
  const themeTitle = $derived(
    home.theme === 'auto' ? t('commun.theme.titre_auto') : home.theme === 'dark' ? t('commun.theme.titre_nuit') : t('commun.theme.titre_jour'),
  );

  function cycleTheme() {
    home.theme = home.theme === 'auto' ? (theme === 'night' ? 'light' : 'dark') : 'auto';
    savePref('maison-theme', home.theme);
  }

  const page = $derived(route.split('/')[0] ?? '');

  // Look only: each page has its own colour (rail pill, card icons, the
  // backdrop's main cloud) and its icon in a badge before its title.
  const LOOK = {
    '': ['#ffb547', '#f07a3a', 'home'],
    moli: ['#b78cff', '#6a5cff', 'sparkles'],
    salon: ['#c77dff', '#ff6fa5', 'remote'],
    pieces: ['#ffc95a', '#ff8f3a', 'light-group'],
    airzone: ['#5fd4ff', '#3b82f6', 'air-conditioner'],
    plan: ['#6cc7ff', '#4f7bff', 'home-roof'],
    carte: ['#5ee0a0', '#14a37f', 'map'],
    cameras: ['#8b9dff', '#5a4fe0', 'cctv'],
    parler: ['#ff8fb8', '#e2558f', 'talk'],
    energie: ['#ffd84d', '#2fbf71', 'bolt'],
    impression: ['#ff9a6b', '#e2483d', 'printer3d'],
    dehors: ['#9be15d', '#1fa67a', 'flower-outline'],
    automatismes: ['#a78bfa', '#7c3aed', 'robot'],
    infra: ['#5eead4', '#0d9488', 'server'],
    parc: ['#93c5fd', '#475fd6', 'package'],
    systeme: ['#7dd3fc', '#2563eb', 'branch'],
  };
  const look = $derived(LOOK[page] ?? LOOK['']);
  const pageIcon = $derived(
    `url("data:image/svg+xml,${encodeURIComponent(`<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24'><path fill='white' d='${icon(look[2])}'/></svg>`)}")`,
  );

  // Look only: the card under the mouse leans a little towards it (3D).
  // Never while a button is held (a slider being dragged must stay still),
  // never on touch, never when the system asks for less motion.
  let leaning = null;
  const calm = typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;
  function lean(e) {
    if (calm || e.pointerType !== 'mouse' || e.buttons) return unlean();
    const card = e.target instanceof Element ? e.target.closest('.content .card') : null;
    if (card !== leaning) unlean();
    if (!card) return;
    // A map, a plan or a drawing canvas stays flat: you aim at a spot there.
    if (card.querySelector('.leaflet-container, canvas, .house3d, .drawing, .graph')) return;
    const r = card.getBoundingClientRect();
    const x = (e.clientX - r.left) / r.width;
    const y = (e.clientY - r.top) / r.height;
    // A big card leans less: the same angle would throw its far edge out.
    const amp = Math.min(4, 2400 / Math.max(r.width, r.height));
    card.style.setProperty('--ry', `${((x - 0.5) * amp).toFixed(2)}deg`);
    card.style.setProperty('--rx', `${((0.5 - y) * amp).toFixed(2)}deg`);
    card.style.setProperty('--mx', `${(x * 100).toFixed(1)}%`);
    card.style.setProperty('--my', `${(y * 100).toFixed(1)}%`);
    card.classList.add('tilting');
    leaning = card;
  }
  function unlean() {
    if (!leaning) return;
    leaning.classList.remove('tilting');
    leaning.style.removeProperty('--rx');
    leaning.style.removeProperty('--ry');
    leaning.style.removeProperty('--mx');
    leaning.style.removeProperty('--my');
    leaning = null;
  }
</script>

<div class="maison" data-theme={theme} style:--accent={look[0]} style:--accent-2={look[1]} style:--page-icon={pageIcon}>
  <nav class="rail" aria-label={t('commun.nav.aria')}>
    <a class="brand" href="#/" aria-label={t('commun.nav.logo')}>
      <svg viewBox="0 0 32 32" aria-hidden="true">
        <circle cx="16" cy="16" r="15" fill="var(--sun)" />
        <circle cx="16" cy="16" r="8.5" fill="none" stroke="#161512" stroke-width="4.3" />
      </svg>
    </a>
    <ul>
      {#each nav as item (item.path)}
        <li>
          <a href="#/{item.path}" class:active={page === item.path} aria-current={page === item.path ? 'page' : undefined}>
            <span class="glyph">
              {#if item.orb}<Orb size={22} state={moli.busy ? 'thinking' : 'idle'} />{:else}<Icon name={item.icon} size={24} />{/if}
            </span>
            <span class="label">{item.name ?? t(item.label)}</span>
          </a>
        </li>
      {/each}
    </ul>
    <div class="rail-foot">
      <button class="theme" onclick={cycleTheme} title={themeTitle}>
        <Icon name={theme === 'night' ? 'moon' : 'sun'} size={20} />
        <span class="label">{themeName}</span>
      </button>
      <a href="#/systeme" class:active={page === 'systeme'} title={t('commun.nav.systeme_titre')}>
        <Icon name="branch" size={20} />
        <span class="label">{t('commun.nav.systeme')}</span>
      </a>
      <a class="atelier" href="#/atelier" title={t('commun.nav.atelier_titre')}>
        <Icon name="atelier" size={20} />
        <span class="label">{t('commun.nav.atelier')}</span>
      </a>
    </div>
  </nav>

  <main class="content" onpointermove={lean} onpointerleave={unlean} onpointerdown={unlean}>
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
    {:else if page === 'impression'}
      <Impression />
    {:else if page === 'dehors'}
      <Dehors />
    {:else if page === 'moli'}
      <Moli />
    {:else if page === 'automatismes'}
      <Automatismes {route} />
    {:else if page === 'systeme'}
      <Systeme />
    {:else if page === 'infra'}
      <!-- The house's machines: loaded only when visited. -->
      {#await import('./pages/Infra.svelte') then { default: Infra }}<Infra />{/await}
    {:else if page === 'parc'}
      <!-- What the house watches beyond itself: loaded only when visited. -->
      {#await import('./pages/Parc.svelte') then { default: Parc }}<Parc />{/await}
    {:else}
      <Accueil />
    {/if}
    <!-- On a phone the rail's foot is hidden: the same three, at the bottom. -->
    <footer class="phone-foot">
      <a href="#/systeme"><Icon name="branch" size={16} />{t('commun.nav.comment_ca_marche')}</a>
      <a href="#/atelier"><Icon name="atelier" size={16} />{t('commun.nav.atelier')}</a>
      <button onclick={cycleTheme}><Icon name={theme === 'night' ? 'moon' : 'sun'} size={16} />{home.theme === 'auto' ? t('commun.theme.auto_pied') : themeName}</button>
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
    display: grid;
    grid-template-columns: 104px minmax(0, 1fr);
  }

  /* The rail: a floating pane of frosted glass, its icons grouped close. */
  .rail {
    position: sticky;
    top: 0;
    height: 100dvh;
    display: flex;
    flex-direction: column;
    align-items: center;
    padding: 14px 0;
    gap: 10px;
  }

  .rail::before {
    content: '';
    position: absolute;
    inset: 12px 10px 12px 12px;
    z-index: -1;
    border-radius: 30px;
    background: var(--glass);
    -webkit-backdrop-filter: var(--glass-blur);
    backdrop-filter: var(--glass-blur);
    box-shadow: var(--shadow), inset 0 0 0 1px var(--glass-line);
  }

  .brand {
    margin-top: 10px;
    border-radius: 999px;
    transition: transform 0.6s var(--ease);
  }

  .brand:hover {
    transform: rotate(-200deg) scale(1.08);
  }

  .brand svg {
    width: 38px;
    height: 38px;
    display: block;
    filter: drop-shadow(0 6px 10px rgb(232 185 49 / 45%));
  }

  .rail ul {
    list-style: none;
    margin: 4px 0 0;
    padding: 0 0 4px;
    display: grid;
    gap: 2px;
    overflow-y: auto;
    scrollbar-width: none;
    min-height: 0;
  }

  .rail a,
  .rail button {
    display: grid;
    justify-items: center;
    gap: 3px;
    width: 78px;
    padding: 6px 0 5px;
    border-radius: 18px;
    border: 0;
    background: none;
    color: var(--ink-3);
    text-decoration: none;
    transition: color 0.2s var(--ease), background 0.2s var(--ease);
  }

  .rail .glyph {
    width: 48px;
    height: 32px;
    border-radius: 999px;
    display: grid;
    place-items: center;
    transition:
      background 0.3s var(--ease),
      transform 0.35s var(--ease),
      box-shadow 0.3s var(--ease);
  }

  .rail a.active {
    color: var(--ink);
  }

  /* The page you are on: a warm lit pill, slightly raised. */
  .rail a.active .glyph {
    background: linear-gradient(135deg, var(--accent), var(--accent-2));
    color: #fff;
    box-shadow: 0 8px 18px -8px var(--accent-2), inset 0 1px 0 rgb(255 255 255 / 45%);
    transform: translateY(-1px);
  }

  .rail a:hover {
    color: var(--ink);
  }

  .rail a:hover .glyph {
    transform: translateY(-2px) scale(1.06);
  }

  .rail a:not(.active):hover .glyph {
    background: var(--surface-2);
  }

  .label {
    font-size: 11.5px;
    font-weight: 600;
  }

  /* The foot: theme, Système, Atelier side by side, icons only (their
     names are in the tooltip), so the rail keeps room for the pages. */
  .rail-foot {
    margin-top: auto;
    display: flex;
    gap: 0;
    padding-top: 6px;
    border-top: 1px solid var(--line);
  }

  .rail-foot a,
  .rail-foot button {
    width: 26px;
    padding: 6px 0;
    border-radius: 10px;
  }

  .rail-foot .label {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }

  .rail-foot a:hover,
  .rail-foot button:hover {
    background: var(--surface-2);
  }

  /* A short screen: the page names make way, the icons stay (named in
     their tooltip and for screen readers). */
  @media (min-width: 761px) and (max-height: 940px) {
    .rail ul .label {
      font-size: 10.5px;
    }

    .rail ul a {
      padding: 3px 0 2px;
      gap: 1px;
    }

    .rail .glyph {
      height: 28px;
    }
  }

  .content {
    padding: 28px 36px 120px 14px;
    max-width: 1560px;
    width: 100%;
    min-width: 0;
    margin-inline: auto;
  }

  /* Moli, always one tap away: a glowing orb; its words open on hover so
     it hides less of the page. */
  .orb {
    position: fixed;
    right: 28px;
    bottom: 28px;
    z-index: 30;
    display: flex;
    align-items: center;
    gap: 0;
    padding: 9px;
    border: 0;
    border-radius: 999px;
    background: var(--glass);
    -webkit-backdrop-filter: var(--glass-blur);
    backdrop-filter: var(--glass-blur);
    box-shadow: var(--shadow-lift), inset 0 0 0 1px var(--glass-line), 0 0 0 0 rgb(176 95 216 / 35%);
    font-weight: 650;
    transition:
      transform 0.3s var(--ease),
      gap 0.3s var(--ease),
      padding 0.3s var(--ease);
  }

  .orb-text {
    max-width: 0;
    overflow: hidden;
    white-space: nowrap;
    opacity: 0;
    transition:
      max-width 0.4s var(--ease),
      opacity 0.3s var(--ease);
  }

  .orb:hover,
  .orb:focus-visible {
    transform: translateY(-3px);
    gap: 12px;
    padding-right: 22px;
  }

  .orb:hover .orb-text,
  .orb:focus-visible .orb-text {
    max-width: 220px;
    opacity: 1;
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
    color: #1c1407;
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
    color: #fff;
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
      padding: 6px 8px calc(6px + env(safe-area-inset-bottom));
      background: var(--glass);
      -webkit-backdrop-filter: var(--glass-blur);
      backdrop-filter: var(--glass-blur);
      box-shadow: var(--shadow-lift), inset 0 1px 0 var(--glass-line);
      z-index: 25;
    }

    .rail::before {
      display: none;
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
      scrollbar-width: none;
      justify-content: safe center;
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
      padding: 20px 16px 140px;
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
      bottom: 86px;
      padding: 8px;
    }

    .orb-text {
      display: none;
    }

    .notes {
      bottom: 150px;
      width: calc(100% - 32px);
    }
  }
</style>
