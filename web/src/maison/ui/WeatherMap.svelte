<script module>
  // The land, decoded once for every map on the page.
  const lands = new Map();
</script>

<script>
  // The weather map around the house (lib/windmap.js) in a box of any
  // size: the house at (tx, ty) of the box, the hour shown (0 = now).
  import { onMount } from 'svelte';
  import { WindMap, viewAround, loadLand } from '../lib/windmap.js';

  let { map, hour = 0, tx = 0.5, ty = 0.5, house = true } = $props();

  let W = $state(0);
  let H = $state(0);
  let land;
  let air;
  let wm = $state(null);
  let lastMap;
  const view = $derived(W && H && map ? viewAround(map, W, H, tx, ty) : null);
  const pin = $derived(view && map ? [((map.house[0] - view.x0) / (view.x1 - view.x0)) * W, ((map.house[1] - view.y0) / (view.y1 - view.y0)) * H] : null);

  onMount(() => {
    const still = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    // A plain copy: the drawing reads it thousands of times a frame.
    lastMap = map;
    wm = new WindMap(land, air, map ? $state.snapshot(map) : null, { still });
    wm.start();
    const visibility = () => (document.hidden ? wm.stop() : wm.start());
    document.addEventListener('visibilitychange', visibility);
    return () => {
      wm.stop();
      document.removeEventListener('visibilitychange', visibility);
    };
  });

  $effect(() => {
    if (wm && map && map !== lastMap) {
      lastMap = map;
      wm.setData($state.snapshot(map));
    }
  });
  $effect(() => {
    if (wm && view) wm.resize(W, H, view);
  });
  $effect(() => {
    const land = map?.land;
    if (!wm || !land) return;
    const key = `${land.z}/${land.x0}/${land.y0}/${land.x1}/${land.y1}`;
    if (!lands.has(key)) lands.set(key, loadLand($state.snapshot(land)));
    lands
      .get(key)
      .then((decoded) => wm.setLand(decoded))
      .catch(() => lands.delete(key));
  });
  $effect(() => {
    if (wm) wm.setHour(hour);
  });
</script>

<div class="map" bind:clientWidth={W} bind:clientHeight={H}>
  <canvas bind:this={land}></canvas>
  <canvas bind:this={air}></canvas>
  {#if house && pin}
    <span class="dot" style="left:{pin[0]}px; top:{pin[1]}px" aria-hidden="true"></span>
  {/if}
</div>

<style>
  .map {
    position: absolute;
    inset: 0;
    overflow: hidden;
    background: #0d1b2c;
  }

  canvas {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
  }

  .dot {
    position: absolute;
    width: 10px;
    height: 10px;
    margin: -5px 0 0 -5px;
    border-radius: 50%;
    background: var(--warm);
    box-shadow:
      0 0 0 2px rgb(255 255 255 / 90%),
      0 0 14px 3px rgb(240 165 58 / 55%);
  }

  .dot::after {
    content: '';
    position: absolute;
    inset: -2px;
    border-radius: 50%;
    border: 2px solid var(--warm);
    animation: ping 3s cubic-bezier(0.22, 1, 0.36, 1) infinite;
  }

  @keyframes ping {
    from {
      transform: scale(1);
      opacity: 0.8;
    }
    to {
      transform: scale(3);
      opacity: 0;
    }
  }
</style>
