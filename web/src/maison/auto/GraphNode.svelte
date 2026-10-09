<script>
  import Icon from '../ui/Icon.svelte';
  import { CATALOG, NODE_W, NODE_H, PORT_LABEL, ports, portY, nodeText, isTrigger } from '../lib/auto.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';

  /** One step on the canvas. Ports on the right lead on; « + » adds the
   *  next step right there. */
  let { node, selected = false, status = null, live = false, problem = false, onpress, onport, onadd } = $props();

  const meta = $derived(CATALOG[node.type] ?? { family: 'action', label: node.type, icon: 'info' });
  const outs = $derived(ports(node.type));
  // The icon of each status; its word is `automatismes.badge.<status>`.
  const STATUS = {
    done: 'ok',
    yes: 'ok',
    no: 'ko',
    failed: 'ko',
    simulated: 'flask',
    timeout: 'timer',
    waiting: 'timer',
  };
</script>

<div
  class="node {meta.family}"
  class:selected
  class:live
  class:problem
  class:trigger={isTrigger(node.type)}
  style="left:{node.x}px; top:{node.y}px; width:{NODE_W}px; height:{NODE_H}px"
  role="button"
  tabindex="0"
  aria-label={t('automatismes.etape.aria', { label: meta.label, texte: nodeText(node) })}
  onpointerdown={(e) => onpress?.(e, node)}>
  {#if !isTrigger(node.type)}<span class="port in" aria-hidden="true"></span>{/if}
  <span class="tile"><Icon name={meta.icon} size={22} /></span>
  <span class="text">
    <b>{meta.short ?? meta.label}</b>
    <small>{nodeText(node)}</small>
  </span>
  {#if status && STATUS[status]}
    <span class="badge {status}" title={t('automatismes.badge.' + status)}><Icon name={STATUS[status]} size={14} /></span>
  {/if}
  {#each outs as port, i (port)}
    <span
      class="port out"
      style="top:{portY(node.type, port) - 7}px"
      role="button"
      tabindex="-1"
      aria-label={t('automatismes.etape.relier', { sortie: PORT_LABEL[port] || t('automatismes.sortie.defaut') })}
      onpointerdown={(e) => {
        e.stopPropagation();
        onport?.(e, node, port);
      }}>
      {#if PORT_LABEL[port]}<em class={port}>{PORT_LABEL[port]}</em>{/if}
    </span>
    <button
      class="add"
      style="top:{portY(node.type, port) - 11}px"
      aria-label={t('automatismes.etape.ajouter')}
      onpointerdown={(e) => e.stopPropagation()}
      onclick={(e) => {
        e.stopPropagation();
        onadd?.(node, port);
      }}><Icon name="plus" size={14} /></button>
  {/each}
</div>

<style>
  .node {
    position: absolute;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 0 16px 0 12px;
    border-radius: 18px;
    background: var(--surface);
    box-shadow: var(--shadow), inset 0 0 0 1px var(--line);
    cursor: grab;
    user-select: none;
    touch-action: none;
    transition: box-shadow 0.2s var(--ease), transform 0.2s var(--ease);
    --fam: var(--ink-3);
    --fam-soft: var(--surface-2);
  }

  .node:active {
    cursor: grabbing;
  }

  .trigger {
    border-radius: 34px 18px 18px 34px;
  }

  .selected {
    box-shadow: var(--shadow-lift), inset 0 0 0 2px var(--fam);
  }

  .problem {
    box-shadow: var(--shadow), inset 0 0 0 2px var(--alert);
  }

  .live {
    animation: glow 1.2s ease-in-out infinite;
  }

  @keyframes glow {
    50% {
      box-shadow: 0 0 0 6px color-mix(in srgb, var(--fam) 25%, transparent), var(--shadow-lift);
    }
  }

  .trigger {
    --fam: #e9a23b;
    --fam-soft: color-mix(in srgb, #e9a23b 16%, var(--surface));
  }
  .logic {
    --fam: #4b74f2;
    --fam-soft: color-mix(in srgb, #4b74f2 14%, var(--surface));
  }
  .action {
    --fam: #2c9a88;
    --fam-soft: color-mix(in srgb, #2c9a88 14%, var(--surface));
  }
  .wait {
    --fam: #8a92a4;
    --fam-soft: color-mix(in srgb, #8a92a4 16%, var(--surface));
  }
  .notify {
    --fam: #b05fd8;
    --fam-soft: color-mix(in srgb, #b05fd8 14%, var(--surface));
  }
  .moli {
    --fam: #ef7f5a;
    --fam-soft: color-mix(in srgb, #ef7f5a 14%, var(--surface));
  }

  .tile {
    flex: none;
    width: 44px;
    height: 44px;
    border-radius: 14px;
    display: grid;
    place-items: center;
    background: var(--fam-soft);
    color: var(--fam);
  }

  .moli .tile {
    background: conic-gradient(from 200deg, #f5c84b, #f0a53a, #ef7f5a, #8f8cf5, #5fb4f0, #f5c84b);
    color: #fff;
  }

  .text {
    display: grid;
    min-width: 0;
    gap: 1px;
  }

  .text b {
    font-size: 14px;
    font-weight: 700;
  }

  .text small {
    font-size: 12.5px;
    color: var(--ink-2);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .port {
    position: absolute;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: var(--surface);
    box-shadow: inset 0 0 0 2.5px var(--fam);
  }

  .port.in {
    left: -7px;
    top: calc(50% - 7px);
  }

  .port.out {
    right: -7px;
    cursor: crosshair;
  }

  .port.out:hover {
    background: var(--fam);
  }

  .port em {
    position: absolute;
    right: 18px;
    top: -3px;
    font-style: normal;
    font-size: 11px;
    font-weight: 700;
    color: var(--ink-3);
    white-space: nowrap;
  }

  .port em.yes,
  .port em.ok {
    color: var(--good);
  }

  .port em.no,
  .port em.timeout {
    color: var(--alert);
  }

  .add {
    position: absolute;
    right: -40px;
    width: 22px;
    height: 22px;
    border-radius: 50%;
    border: 0;
    display: grid;
    place-items: center;
    background: var(--fam);
    color: #fff;
    opacity: 0;
    transform: scale(0.7);
    transition: all 0.18s var(--ease);
  }

  .node:hover .add,
  .selected .add {
    opacity: 1;
    transform: scale(1);
  }

  .badge {
    position: absolute;
    top: -9px;
    right: 12px;
    height: 22px;
    min-width: 22px;
    padding: 0 6px;
    border-radius: 999px;
    display: grid;
    place-items: center;
    color: #fff;
    background: var(--good);
    box-shadow: 0 0 0 3px var(--surface);
  }

  .badge.no,
  .badge.failed,
  .badge.timeout {
    background: var(--alert);
  }

  .badge.simulated,
  .badge.waiting {
    background: var(--cool);
  }
</style>
