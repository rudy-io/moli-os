<script>
  import { onMount } from 'svelte';
  import Icon from '../ui/Icon.svelte';
  import GraphNode from './GraphNode.svelte';
  import { NODE_W, NODE_H, portY, isTrigger, layout } from '../lib/auto.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';

  /** The automation's graph: drag nodes, drag from a port to link (or drop
   *  in the void to add a step there), pan, zoom, delete. */
  let {
    graph = $bindable(),
    selected = $bindable(null),
    statuses = {},
    live = {},
    problems = new Set(),
    onchange,
    onadd,
  } = $props();

  let vp;
  let view = $state({ x: 60, y: 60, k: 1 });
  let link = $state(null);
  let hoverTarget = $state(null);
  let drag = null;
  const pointers = new Map();
  let pinch = null;

  const byId = $derived(Object.fromEntries(graph.nodes.map((n) => [n.id, n])));

  function world(e) {
    const r = vp.getBoundingClientRect();
    return { x: (e.clientX - r.left - view.x) / view.k, y: (e.clientY - r.top - view.y) / view.k };
  }

  function nodeAt(p) {
    return graph.nodes.findLast((n) => p.x >= n.x - 12 && p.x <= n.x + NODE_W + 12 && p.y >= n.y - 12 && p.y <= n.y + NODE_H + 12);
  }

  function pressNode(e, node) {
    if (e.button !== 0) return;
    e.stopPropagation();
    selected = { kind: 'node', id: node.id };
    const p = world(e);
    drag = { mode: 'node', node, dx: p.x - node.x, dy: p.y - node.y, moved: false };
    vp.setPointerCapture(e.pointerId);
  }

  function pressPort(e, node, port) {
    const p = world(e);
    link = { from: node.id, port, x: p.x, y: p.y };
    drag = { mode: 'link' };
    vp.setPointerCapture(e.pointerId);
  }

  function pressBackground(e) {
    pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (pointers.size === 2) {
      const [a, b] = [...pointers.values()];
      pinch = { d: Math.hypot(a.x - b.x, a.y - b.y), k: view.k };
      drag = null;
      return;
    }
    selected = null;
    drag = { mode: 'pan', sx: e.clientX - view.x, sy: e.clientY - view.y };
    vp.setPointerCapture(e.pointerId);
  }

  function move(e) {
    if (pointers.has(e.pointerId)) pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (pinch && pointers.size === 2) {
      const [a, b] = [...pointers.values()];
      const d = Math.hypot(a.x - b.x, a.y - b.y);
      zoomAt((a.x + b.x) / 2, (a.y + b.y) / 2, (pinch.k * d) / pinch.d / view.k);
      return;
    }
    if (!drag) return;
    if (drag.mode === 'pan') {
      view.x = e.clientX - drag.sx;
      view.y = e.clientY - drag.sy;
    } else if (drag.mode === 'node') {
      const p = world(e);
      const x = Math.round((p.x - drag.dx) / 10) * 10;
      const y = Math.round((p.y - drag.dy) / 10) * 10;
      if (x !== drag.node.x || y !== drag.node.y) {
        drag.node.x = x;
        drag.node.y = y;
        drag.moved = true;
      }
    } else if (drag.mode === 'link') {
      const p = world(e);
      link.x = p.x;
      link.y = p.y;
      const target = nodeAt(p);
      hoverTarget = target && target.id !== link.from && !isTrigger(target.type) ? target.id : null;
    }
  }

  function up(e) {
    pointers.delete(e.pointerId);
    if (pointers.size < 2) pinch = null;
    if (drag?.mode === 'node' && drag.moved) onchange?.('move');
    if (drag?.mode === 'link' && link) {
      const p = world(e);
      const target = nodeAt(p);
      if (target && target.id !== link.from) {
        connect(link.from, link.port, target.id);
      } else if (!target) {
        onadd?.(byId[link.from], link.port, { x: p.x, y: p.y - NODE_H / 2 });
      }
    }
    drag = null;
    link = null;
    hoverTarget = null;
  }

  function connect(from, port, to) {
    const target = byId[to];
    if (!target || isTrigger(target.type)) return;
    if (graph.edges.some((e) => e.from === from && e.port === port && e.to === to)) return;
    // No cycles: `to` must not lead back to `from`.
    const seen = new Set();
    const stack = [to];
    while (stack.length) {
      const n = stack.pop();
      if (n === from) return;
      if (seen.has(n)) continue;
      seen.add(n);
      graph.edges.filter((e) => e.from === n).forEach((e) => stack.push(e.to));
    }
    graph.edges.push({ from, port, to });
    onchange?.('edit');
  }

  function wheel(e) {
    e.preventDefault();
    if (e.ctrlKey || e.metaKey) {
      zoomAt(e.clientX, e.clientY, Math.exp(-e.deltaY * 0.01));
    } else {
      view.x -= e.deltaX;
      view.y -= e.deltaY;
    }
  }

  function zoomAt(cx, cy, factor) {
    const r = vp.getBoundingClientRect();
    const k = Math.min(1.8, Math.max(0.35, view.k * factor));
    const px = cx - r.left;
    const py = cy - r.top;
    view.x = px - ((px - view.x) * k) / view.k;
    view.y = py - ((py - view.y) * k) / view.k;
    view.k = k;
  }

  export function fit() {
    if (!vp) return;
    const r = vp.getBoundingClientRect();
    if (!graph.nodes.length) {
      view = { x: r.width / 2 - NODE_W / 2, y: r.height / 2 - NODE_H, k: 1 };
      return;
    }
    const xs = graph.nodes.map((n) => n.x);
    const ys = graph.nodes.map((n) => n.y);
    const minX = Math.min(...xs) - 60;
    const minY = Math.min(...ys) - 60;
    const w = Math.max(...xs) + NODE_W + 80 - minX;
    const h = Math.max(...ys) + NODE_H + 60 - minY;
    // Readable first: below 0.78 the words get too small; then start from the left.
    const k = Math.max(0.78, Math.min(1.1, r.width / w, r.height / h));
    const x = w * k > r.width ? 24 - minX * k : (r.width - w * k) / 2 - minX * k;
    view = { x, y: (r.height - h * k) / 2 - minY * k, k };
  }

  function tidy() {
    layout(graph);
    onchange?.('move');
    requestAnimationFrame(fit);
  }

  function remove() {
    if (!selected) return;
    if (selected.kind === 'node') {
      graph.nodes = graph.nodes.filter((n) => n.id !== selected.id);
      graph.edges = graph.edges.filter((e) => e.from !== selected.id && e.to !== selected.id);
    } else {
      graph.edges = graph.edges.filter((_, i) => i !== selected.index);
    }
    selected = null;
    onchange?.('edit');
  }

  function key(e) {
    if (e.target.closest?.('input, textarea, select, [contenteditable]')) return;
    if ((e.key === 'Delete' || e.key === 'Backspace') && selected) {
      e.preventDefault();
      remove();
    }
  }

  onMount(() => requestAnimationFrame(fit));

  function path(x1, y1, x2, y2) {
    const dx = Math.max(50, Math.abs(x2 - x1) / 2);
    return `M ${x1} ${y1} C ${x1 + dx} ${y1}, ${x2 - dx} ${y2}, ${x2} ${y2}`;
  }

  const edges = $derived(
    graph.edges
      .map((e, index) => {
        const a = byId[e.from];
        const b = byId[e.to];
        if (!a || !b) return null;
        const x1 = a.x + NODE_W;
        const y1 = a.y + portY(a.type, e.port);
        const x2 = b.x;
        const y2 = b.y + NODE_H / 2;
        const passed = statuses[e.from] && statuses[e.to];
        return { e, index, d: path(x1, y1, x2, y2), mx: (x1 + x2) / 2, my: (y1 + y2) / 2, passed };
      })
      .filter(Boolean),
  );
</script>

<svelte:window onkeydown={key} />

<div
  class="viewport"
  bind:this={vp}
  role="application"
  aria-label={t('automatismes.graphe.aria')}
  onpointerdown={pressBackground}
  onpointermove={move}
  onpointerup={up}
  onpointercancel={up}
  onwheel={wheel}
  style="--k:{view.k}; background-position:{view.x}px {view.y}px; background-size:{22 * view.k}px {22 * view.k}px">
  <div class="world" style="transform: translate({view.x}px, {view.y}px) scale({view.k})">
    <svg class="edges" aria-hidden="true">
      {#each edges as edge (edge.index + edge.e.from + edge.e.port + edge.e.to)}
        <g class="edge {edge.e.port}" class:passed={edge.passed} class:chosen={selected?.kind === 'edge' && selected.index === edge.index}>
          <path class="hit" d={edge.d} role="button" tabindex="-1" aria-label={t('automatismes.graphe.lien')} onpointerdown={(ev) => {
            ev.stopPropagation();
            selected = { kind: 'edge', index: edge.index };
          }} />
          <path class="line" d={edge.d} />
        </g>
      {/each}
      {#if link}
        {@const a = byId[link.from]}
        {#if a}<path class="line drawing" d={path(a.x + NODE_W, a.y + portY(a.type, link.port), link.x, link.y)} />{/if}
      {/if}
    </svg>

    {#each graph.nodes as node (node.id)}
      <GraphNode
        {node}
        selected={selected?.kind === 'node' && selected.id === node.id}
        status={statuses[node.id]}
        live={!!live[node.id]}
        problem={problems.has(node.id) || hoverTarget === node.id}
        onpress={pressNode}
        onport={pressPort}
        onadd={(n, port) => onadd?.(n, port, { x: n.x + NODE_W + 80, y: n.y })} />
    {/each}

    {#each edges as edge (edge.index)}
      {#if selected?.kind === 'edge' && selected.index === edge.index}
        <button class="cut" style="left:{edge.mx - 15}px; top:{edge.my - 15}px" onpointerdown={(e) => e.stopPropagation()} onclick={remove} aria-label={t('automatismes.graphe.supprimer_lien')}>
          <Icon name="close" size={16} />
        </button>
      {/if}
    {/each}
  </div>

  {#if !graph.nodes.length}
    <div class="empty">
      <p>{t('automatismes.graphe.vide')}</p>
      <button onclick={() => onadd?.(null, null, { x: 0, y: 0 })}><Icon name="plus" size={18} />{t('automatismes.graphe.choisir_depart')}</button>
    </div>
  {/if}

  <div class="tools" role="toolbar" tabindex="-1" aria-label={t('automatismes.graphe.vue')} onpointerdown={(e) => e.stopPropagation()}>
    <button onclick={() => zoomAt(vp.getBoundingClientRect().left + vp.clientWidth / 2, vp.getBoundingClientRect().top + vp.clientHeight / 2, 1.2)} aria-label={t('automatismes.graphe.zoomer')}><Icon name="zoom-in" size={18} /></button>
    <button onclick={() => zoomAt(vp.getBoundingClientRect().left + vp.clientWidth / 2, vp.getBoundingClientRect().top + vp.clientHeight / 2, 1 / 1.2)} aria-label={t('automatismes.graphe.dezoomer')}><Icon name="zoom-out" size={18} /></button>
    <button onclick={fit} aria-label={t('automatismes.graphe.tout_voir')}><Icon name="fit" size={18} /></button>
    <button onclick={tidy} aria-label={t('automatismes.graphe.ranger')} title={t('automatismes.graphe.ranger_titre')}><Icon name="magic" size={18} /></button>
    {#if selected}
      <button class="danger" onclick={remove} aria-label={t('automatismes.graphe.supprimer_selection')}><Icon name="trash" size={18} /></button>
    {/if}
  </div>
</div>

<style>
  .viewport {
    position: relative;
    overflow: hidden;
    height: 100%;
    min-height: 420px;
    border-radius: var(--r-lg);
    background-color: var(--surface-2);
    background-image: radial-gradient(circle, color-mix(in srgb, var(--ink-3) 35%, transparent) 1px, transparent 1.2px);
    touch-action: none;
    cursor: default;
  }

  .world {
    position: absolute;
    left: 0;
    top: 0;
    transform-origin: 0 0;
  }

  .edges {
    position: absolute;
    left: 0;
    top: 0;
    width: 1px;
    height: 1px;
    overflow: visible;
  }

  .line {
    fill: none;
    stroke: color-mix(in srgb, var(--ink-3) 70%, transparent);
    stroke-width: 2.5;
    pointer-events: none;
  }

  .edge.yes .line,
  .edge.ok .line {
    stroke: color-mix(in srgb, var(--good) 75%, transparent);
  }

  .edge.no .line,
  .edge.timeout .line {
    stroke: color-mix(in srgb, var(--alert) 70%, transparent);
  }

  .edge.passed .line {
    stroke: var(--warm);
    stroke-width: 3.5;
    stroke-dasharray: 8 6;
    animation: flow 0.8s linear infinite;
  }

  .edge.chosen .line {
    stroke: var(--cool);
    stroke-width: 3.5;
  }

  @keyframes flow {
    to {
      stroke-dashoffset: -14;
    }
  }

  .hit {
    fill: none;
    stroke: transparent;
    stroke-width: 16;
    cursor: pointer;
    pointer-events: stroke;
  }

  .drawing {
    stroke: var(--cool);
    stroke-dasharray: 6 5;
  }

  .cut {
    position: absolute;
    width: 30px;
    height: 30px;
    border-radius: 50%;
    border: 0;
    background: var(--alert);
    color: #fff;
    display: grid;
    place-items: center;
    box-shadow: var(--shadow-lift);
  }

  .empty {
    position: absolute;
    inset: 0;
    display: grid;
    place-content: center;
    justify-items: center;
    gap: 12px;
    color: var(--ink-3);
    font-weight: 600;
    pointer-events: none;
  }

  .empty button {
    pointer-events: auto;
    display: flex;
    align-items: center;
    gap: 8px;
    border: 0;
    border-radius: 999px;
    padding: 12px 20px;
    background: #e9a23b;
    color: #fff;
    font-weight: 700;
    box-shadow: var(--shadow-lift);
  }

  .tools {
    position: absolute;
    left: 14px;
    bottom: 14px;
    display: flex;
    gap: 4px;
    padding: 4px;
    border-radius: 14px;
    background: var(--surface);
    box-shadow: var(--shadow-lift);
  }

  .tools button {
    width: 36px;
    height: 36px;
    border-radius: 10px;
    border: 0;
    background: none;
    color: var(--ink-2);
    display: grid;
    place-items: center;
  }

  .tools button:hover {
    background: var(--surface-2);
  }

  .tools .danger {
    color: var(--alert);
  }
</style>
