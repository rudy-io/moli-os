<script>
  import { device, value, act, nameOf, num, pending, reachable, coverPosition, coverPositionKey } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import ShutterIcon from './ShutterIcon.svelte';

  /** A shutter: tap to toggle open/close, advanced controls for precise positioning. */
  let { id, name = null } = $props();

  const d = $derived(device(id));
  const label = $derived(nameOf(id, name));
  const ok = $derived(reachable(id));
  const pos = $derived(coverPosition(id));
  const posKey = $derived(coverPositionKey(id));
  const busy = $derived(pending(id, posKey) || pending(id, 'control'));
  const offers = $derived(d?.points.find((p) => p.key === 'control')?.kind.values ?? []);
  const hasFavRecall = $derived(offers.includes('favourite') || offers.includes('my'));
  const hasFavSet = $derived(!!d?.points.find((p) => p.key === 'favourite_position')?.access?.write);

  const pct = $derived(pos == null ? null : Math.max(0, Math.min(100, Math.round(Number(pos)))));

  const story = $derived.by(() => {
    if (!d) return t('commun.volet.introuvable');
    if (!ok) return t('commun.volet.injoignable');
    if (pct == null) return null;
    if (pct >= 95) return t('commun.volet.ouvert');
    if (pct <= 5) return t('commun.volet.ferme');
    return t('commun.volet.entre_ouvert');
  });

  const tilt = $derived(pct == null ? 0 : pct / 100);
  const shade = $derived(Math.min(90, pct == null ? 0 : pct * 0.6));
  const isOpen = $derived(pct != null && pct >= 50);
  const isClosed = $derived(pct != null && pct <= 5);

  let dragging = $state(null);

  function tap() {
    const cmd = isOpen ? 'close' : 'open';
    if (!offers.includes(cmd)) return;
    const action = t(isOpen ? 'commun.volet.fermer' : 'commun.volet.ouvrir').toLowerCase();
    act(`${id}/control`, cmd, t('commun.action', { label, action }));
  }

  function recallFav() {
    act(`${id}/control`, 'favourite', t('commun.action', { label, action: t('commun.volet.my').toLowerCase() }));
  }

  function setFav() {
    if (pct == null) return;
    act(`${id}/favourite_position`, pct, t('commun.action', { label, action: t('commun.volet.definir_my').toLowerCase() }));
  }

  function setLevel(e) {
    dragging = Number(e.currentTarget.value);
  }

  function commitLevel(e) {
    const v = Number(e.currentTarget.value);
    dragging = null;
    if (!posKey) return;
    act(`${id}/${posKey}`, v, t('commun.action', { label, action: t('commun.volet.position') }));
  }
</script>

<div class="cover" class:open={isOpen} class:closed={isClosed} class:busy class:off-grid={!ok || !d}>
  <button class="hit" onclick={tap} disabled={!d || !ok} aria-label={t('commun.action', { label, action: story })}>
    <ShutterIcon {id} size={46} />
    <span class="name">
      <b>{label}</b>
      {#if !d || !ok}
        <small>{story}</small>
      {:else if pct != null}
        <small>{story} · <b class="num">{num(pct)}%</b></small>
      {:else}
        <small></small>
      {/if}
    </span>
    <span class="toggle" aria-hidden="true"><i></i></span>
  </button>
  {#if pct != null && posKey}
    <div class="controls">
      <input
        class="level"
        type="range"
        min="0"
        max="100"
        value={dragging ?? pct}
        oninput={setLevel}
        onchange={commitLevel}
        aria-label={t('commun.volet.position_label', { label })} />
    </div>
  {/if}
  <div class="actions">
    {#if offers.includes('open')}
      <button class="icon-btn" onclick={() => act(`${id}/control`, 'open', t('commun.action', { label, action: t('commun.volet.ouvrir').toLowerCase() }))} title={t('commun.volet.ouvrir')} aria-label={t('commun.volet.ouvrir')}>
        <Icon name="chevron-up" size={20} />
      </button>
    {/if}
    {#if offers.includes('stop')}
      <button class="icon-btn" onclick={() => act(`${id}/control`, 'stop', t('commun.action', { label, action: t('commun.volet.stop').toLowerCase() }))} title={t('commun.volet.stop')} aria-label={t('commun.volet.stop')}>
        <Icon name="stop" size={20} />
      </button>
    {/if}
    {#if offers.includes('close')}
      <button class="icon-btn" onclick={() => act(`${id}/control`, 'close', t('commun.action', { label, action: t('commun.volet.fermer').toLowerCase() }))} title={t('commun.volet.fermer')} aria-label={t('commun.volet.fermer')}>
        <Icon name="chevron-down" size={20} />
      </button>
    {/if}
    {#if hasFavRecall}
      <button class="icon-btn" onclick={recallFav} title={t('commun.volet.my')} aria-label={t('commun.volet.my')}>
        <Icon name="stars" size={20} />
      </button>
    {/if}
    {#if hasFavSet}
      <button class="icon-btn ghost" onclick={setFav} title={t('commun.volet.definir_my')} aria-label={t('commun.volet.definir_my')}>
        <Icon name="heart" size={18} />
      </button>
    {/if}
  </div>
</div>

<style>
  .cover {
    position: relative;
    border-radius: var(--r-md);
    background: var(--surface-2);
    transition: background 0.35s var(--ease), box-shadow 0.35s var(--ease);
    overflow: hidden;
  }

  .cover.open {
    background: linear-gradient(155deg, var(--warm-soft) 0%, var(--surface) 70%);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--warm) 25%, transparent);
  }

  .cover.busy :global(.shutter-icon) {
    animation: pulse 1s ease-in-out infinite;
  }

  @keyframes pulse {
    50% {
      opacity: 0.55;
    }
  }

  .cover.off-grid {
    opacity: 0.55;
  }

  .hit {
    all: unset;
    box-sizing: border-box;
    width: 100%;
    min-height: 112px;
    padding: 16px;
    display: grid;
    grid-template-columns: auto 1fr;
    grid-template-rows: auto 1fr;
    gap: 10px 12px;
    cursor: pointer;
    transition: transform 0.15s var(--ease);
  }

  .hit:hover {
    transform: scale(1.01);
  }

  .hit:active {
    transform: scale(0.99);
  }

  .hit:disabled {
    cursor: not-allowed;
  }

  .hit:focus-visible {
    outline: 3px solid var(--cool);
    outline-offset: -3px;
    border-radius: var(--r-md);
  }

  .name {
    grid-column: 1 / -1;
    align-self: end;
    display: grid;
    gap: 2px;
  }

  .name b {
    font-weight: 650;
    font-size: 16px;
    color: var(--ink);
  }

  .name small {
    color: var(--ink-3);
    font-size: 13px;
    font-weight: 550;
    display: flex;
    gap: 6px;
    align-items: baseline;
  }

  .open .name small {
    color: var(--warm-ink);
  }

  /* Toggle switch top right */
  .toggle {
    position: absolute;
    top: 18px;
    right: 16px;
    width: 40px;
    height: 24px;
    border-radius: 999px;
    background: var(--surface-3);
    transition: background 0.3s var(--ease);
  }

  .toggle i {
    position: absolute;
    top: 3px;
    left: 3px;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: var(--surface);
    box-shadow: 0 1px 3px rgb(0 0 0 / 20%);
    transition: transform 0.3s var(--ease);
  }

  .open .toggle {
    background: var(--warm);
  }

  .open .toggle i {
    transform: translateX(16px);
  }

  .controls {
    display: flex;
    align-items: center;
    gap: 12px;
    margin: -6px 16px 8px;
  }

  .level {
    flex: 1;
    min-width: 0;
    margin: 0;
    accent-color: var(--warm);
    height: 28px;
  }

  .actions {
    display: flex;
    justify-content: center;
    align-items: center;
    gap: 8px;
    padding: 0 16px 16px;
  }

  .icon-btn {
    all: unset;
    box-sizing: border-box;
    width: 42px;
    height: 42px;
    border-radius: 50%;
    background: var(--surface);
    color: var(--ink);
    display: grid;
    place-items: center;
    cursor: pointer;
    box-shadow: var(--shadow);
    transition: all 0.2s var(--ease);
  }

  .icon-btn:hover {
    background: var(--surface-3);
    transform: scale(1.05);
  }

  .icon-btn:active {
    transform: scale(0.98);
  }

  .icon-btn.ghost {
    background: transparent;
    box-shadow: none;
    color: var(--ink-2);
  }

  .icon-btn.ghost:hover {
    background: var(--surface-2);
  }

  .icon-btn:focus-visible {
    outline: 3px solid var(--cool);
    outline-offset: 2px;
  }
</style>
