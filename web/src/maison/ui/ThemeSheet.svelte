<script>
  import { t } from '../../lib/i18n.svelte.js';
  import { mdiThemeLightDark, mdiWhiteBalanceSunny, mdiMoonWaningCrescent } from '@mdi/js';
  import Icon from './Icon.svelte';
  import SegmentedControl from './SegmentedControl.svelte';
  import Orb from '../moli/Orb.svelte';
  import { KEPT } from '../lib/tabs.js';

  /** This screen's theme: day or night, and which tabs the menu keeps.
   *  `tabs` are those the house has something for; a tab it has nothing for
   *  (no camera yet) is not listed and comes back on its own. */
  let { tabs, hidden, theme, ontheme, ontoggle, onclose } = $props();

  function onkey(e) {
    if (e.key === 'Escape') onclose();
  }
</script>

<svelte:window onkeydown={onkey} />

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

  <section>
    <h3>{t('commun.theme_sheet.onglets')}</h3>
    <p class="hint">{t('commun.theme_sheet.onglets_aide')}</p>
    <ul>
      {#each tabs as tab (tab.path)}
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
