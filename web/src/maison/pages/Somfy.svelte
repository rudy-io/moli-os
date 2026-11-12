<script>
  import { t } from '../../lib/i18n.svelte.js';
  import SomfyCloud from '../ui/SomfyCloud.svelte';
  import Icon from '../ui/Icon.svelte';
  import { hub, deviceKind, hidden } from '../lib/home.svelte.js';
  const hasCovers = $derived(Object.values(hub.devices).some(d => !hidden(d.id) && deviceKind(d) === 'cover'));
</script>

<div class="somfy-page">
  <header><h1>{t('systeme.somfy.page_title')}</h1><p>{t('systeme.somfy.page_intro')}</p></header>
  <a class="equipment" href="#/pieces/volets">
    <span class="symbol"><Icon name="shutter" size={28} /></span>
    <span><strong>{t('systeme.somfy.equipment')}</strong><small>{t(hasCovers ? 'systeme.somfy.equipment_hint' : 'systeme.somfy.equipment_missing')}</small></span>
    <Icon name="chevron-right" size={22} />
  </a>
  <SomfyCloud />
</div>

<style>
  .somfy-page { max-width: 1100px; display: grid; gap: 24px; margin-inline: auto; }
  h1 { margin: 0; letter-spacing: -.025em; }
  header p { margin: 8px 0 0; color: var(--ink-2); line-height: 1.5; }
  .equipment { display: flex; align-items: center; gap: 16px; padding: 20px; border-radius: var(--r-lg); border: 1px solid var(--line); background: var(--surface); color: var(--ink); text-decoration: none; transition: background 160ms ease; }
  .equipment > span:nth-child(2) { flex: 1; min-width: 0; }
  .symbol { padding: 12px; border-radius: var(--r-md); color: var(--blue); background: var(--surface-2); }
  strong, small { display: block; }
  small { margin-top: 5px; line-height: 1.5; color: var(--ink-2); font-size: 14px; }
  .equipment:hover { background: var(--surface-2); }
  .equipment:focus-visible { outline: 3px solid var(--blue); outline-offset: 4px; }
  @media (prefers-reduced-motion: reduce) { .equipment { transition: none; } }
</style>
