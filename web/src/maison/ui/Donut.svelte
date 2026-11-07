<script>
  import { onMount } from 'svelte';

  /** A ring of shares, drawn as a thick coin seen slightly from above (3D):
   *  each part sweeps in when shown; pointing at one lifts it and the
   *  middle says its name and value. Look only, nothing is computed here
   *  but the angles.
   *  items: [{ id, label, value, color }] — values ≥ 0, any unit.
   *  center / sub: what the middle says when nothing is pointed at.
   *  format(value): the value as the page writes it. */
  let { items = [], size = 176, thickness = 24, center = '', sub = '', format = (v) => String(v), label = '' } = $props();

  const R = 50 - 12;
  const C = 2 * Math.PI * R;
  const total = $derived(items.reduce((t, i) => t + Math.max(0, i.value), 0));
  const arcs = $derived.by(() => {
    let at = 0;
    return items
      .filter((i) => i.value > 0)
      .map((i) => {
        const len = total ? (i.value / total) * C : 0;
        const arc = { ...i, len, at, pct: total ? Math.round((i.value / total) * 100) : 0 };
        at += len;
        return arc;
      });
  });

  let shown = $state(false);
  let focus = $state(null);
  onMount(() => {
    // Two frames: the empty ring is painted first, so the sweep is seen.
    let id = requestAnimationFrame(() => (id = requestAnimationFrame(() => (shown = true))));
    return () => cancelAnimationFrame(id);
  });
  const picked = $derived(arcs.find((a) => a.id === focus));
  // A thin gap between parts, only when there are several.
  const gap = $derived(arcs.length > 1 ? 0.9 : 0);
  const sw = $derived((thickness / size) * 100);
</script>

<figure class="donut" style:--size="{size}px" aria-label={label || undefined}>
  <div class="coin">
    <!-- The coin's edge: the same ring, darker, a little lower. -->
    <svg class="edge" viewBox="0 0 100 100" aria-hidden="true">
      {#each arcs as a (a.id)}
        <circle
          cx="50"
          cy="50"
          r={R}
          stroke={a.color}
          stroke-width={sw}
          stroke-dasharray="{shown ? Math.max(0, a.len - gap) : 0} {C}"
          stroke-dashoffset={-a.at}
        />
      {/each}
    </svg>
    <svg class="face" viewBox="0 0 100 100" aria-hidden="true">
      <circle class="track" cx="50" cy="50" r={R} stroke-width={sw} />
      {#each arcs as a, n (a.id)}
        <circle
          role="presentation"
          class:dim={focus && focus !== a.id}
          class:lift={focus === a.id}
          cx="50"
          cy="50"
          r={R}
          stroke={a.color}
          stroke-width={sw}
          stroke-dasharray="{shown ? Math.max(0, a.len - gap) : 0} {C}"
          stroke-dashoffset={-a.at}
          style:transition-delay="{n * 70}ms"
          onpointerenter={(e) => e.pointerType === 'mouse' && (focus = a.id)}
          onpointerleave={(e) => e.pointerType === 'mouse' && (focus = null)}
          onclick={() => (focus = focus === a.id ? null : a.id)}
        />
      {/each}
    </svg>
  </div>
  <figcaption class="middle">
    {#if picked}
      <b class="num">{format(picked.value)}</b>
      <span>{picked.label} · {picked.pct} %</span>
    {:else}
      <b class="num">{center}</b>
      {#if sub}<span>{sub}</span>{/if}
    {/if}
  </figcaption>
</figure>

<style>
  .donut {
    position: relative;
    width: var(--size);
    height: var(--size);
    margin: 0;
    flex: none;
    perspective: 700px;
  }

  .coin {
    position: absolute;
    inset: 0;
    transform: rotateX(22deg);
    transform-style: preserve-3d;
    transition: transform 0.6s cubic-bezier(0.22, 1, 0.36, 1);
  }

  .donut:hover .coin {
    transform: rotateX(8deg);
  }

  svg {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    transform: rotate(-90deg);
    overflow: visible;
  }

  circle {
    fill: none;
    transition:
      stroke-dasharray 1s cubic-bezier(0.22, 1, 0.36, 1),
      opacity 0.25s ease,
      stroke-width 0.25s ease;
  }

  .edge {
    pointer-events: none;
    transform: rotate(-90deg) translate(-6px, 0);
    filter: brightness(0.62) saturate(1.1);
  }

  .face circle:not(.track) {
    cursor: pointer;
    filter: drop-shadow(0 0 0.6px rgb(0 0 0 / 15%));
  }

  .track {
    stroke: color-mix(in srgb, var(--ink) 7%, transparent);
  }

  .dim {
    opacity: 0.35;
  }

  .lift {
    stroke-width: 15;
  }

  .middle {
    position: absolute;
    inset: 22%;
    display: grid;
    place-content: center;
    text-align: center;
    gap: 2px;
    pointer-events: none;
  }

  .middle b {
    font-size: calc(var(--size) * 0.13);
    font-weight: 750;
    letter-spacing: -0.02em;
    line-height: 1.05;
  }

  .middle span {
    font-size: 11.5px;
    color: var(--ink-3);
    font-weight: 600;
    line-height: 1.2;
  }

  @media (prefers-reduced-motion: reduce) {
    .coin,
    .donut:hover .coin {
      transform: none;
    }

    .edge {
      display: none;
    }
  }
</style>
