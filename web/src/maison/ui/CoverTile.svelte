<script>
  import { device, value, act, nameOf, num } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** A shutter: open, stop, close (whatever the device offers). */
  let { id, name = null } = $props();

  const d = $derived(device(id));
  const label = $derived(nameOf(id, name));
  const pos = $derived(value(id, 'percent_control'));
  const offers = $derived(d?.points.find((p) => p.key === 'control')?.kind.values ?? []);
  // [command sent to the device, catalogue key of its button]
  const BUTTONS = [
    ['open', 'commun.volet.ouvrir'],
    ['stop', 'commun.volet.stop'],
    ['close', 'commun.volet.fermer'],
  ];
</script>

<div class="cover">
  <Icon name="shutter" size={22} />
  <span class="name">{label}<small class="muted">{pos != null ? t('commun.volet.ouvert_a', { pct: num(pos) }) : ''}</small></span>
  {#each BUTTONS.filter(([cmd]) => offers.includes(cmd)) as [cmd, key] (cmd)}
    <button onclick={() => act(`${id}/control`, cmd, t('commun.action', { label, action: t(key).toLowerCase() }))}>{t(key)}</button>
  {/each}
</div>

<style>
  .cover {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 12px 14px;
    background: var(--surface-2);
    border-radius: var(--r-md);
    color: var(--ink-2);
    flex-wrap: wrap;
  }

  .name {
    flex: 1;
    display: grid;
    font-weight: 650;
    color: var(--ink);
    min-width: 120px;
  }

  small {
    font-weight: 500;
  }

  button {
    border: 0;
    background: var(--surface);
    border-radius: 999px;
    padding: 8px 12px;
    font-size: 13px;
    font-weight: 650;
    box-shadow: var(--shadow);
  }
</style>
