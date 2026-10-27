<script>
  import { onMount } from 'svelte';
  import { readPref, savePref } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** Offers to put the dashboard on a phone's home screen. Nothing on a
   *  computer, in the Moli app (it is one already), once installed, or after
   *  "don't show again". Over plain http on the home network the browser sends
   *  no install event (Android) or none at all (iOS): the steps are told
   *  instead. The server names the page (`document.title`). */
  const KEY = 'maison-install-hidden';

  /** `null`: nothing to offer; `'prompt'`: the browser installs on a tap;
   *  otherwise the steps of that system. */
  let how = $state(null);
  let deferred = null;

  onMount(() => {
    if (readPref(KEY, '')) return;
    const ua = navigator.userAgent;
    if (ua.includes('MoliNative')) return;
    if (matchMedia('(display-mode: standalone)').matches || navigator.standalone) return;
    if (/iPhone|iPad|iPod/.test(ua) || (ua.includes('Macintosh') && navigator.maxTouchPoints > 1)) how = 'ios';
    else if (/Android/.test(ua)) how = 'android';
    const offered = (e) => {
      e.preventDefault();
      deferred = e;
      how = 'prompt';
    };
    const installed = () => (how = null);
    addEventListener('beforeinstallprompt', offered);
    addEventListener('appinstalled', installed);
    return () => {
      removeEventListener('beforeinstallprompt', offered);
      removeEventListener('appinstalled', installed);
    };
  });

  async function install() {
    const event = deferred;
    deferred = null;
    how = null;
    await event?.prompt();
  }

  function hide() {
    savePref(KEY, '1');
    how = null;
  }
</script>

{#if how}
  <section class="card install">
    <span class="icon"><Icon name="phone" size={20} /></span>
    <div class="what">
      <b>{t('commun.installer.titre', { nom: document.title })}</b>
      <span class="muted">{how === 'prompt' ? t('commun.installer.texte') : t('commun.installer.' + how)}</span>
    </div>
    {#if how === 'prompt'}
      <button class="primary" onclick={install}>{t('commun.installer.bouton')}</button>
    {/if}
    <button onclick={hide}>{t('commun.installer.masquer')}</button>
  </section>
{/if}

<style>
  .install {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 12px 16px;
    margin-bottom: 16px;
  }
  .what {
    display: flex;
    flex-direction: column;
    gap: 2px;
    flex: 1 1 14rem;
  }
</style>
