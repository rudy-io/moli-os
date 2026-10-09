<script>
  import { onMount } from 'svelte';
  import { hub, value, deviceKind, isOn, isFixture, doorOpen, leak, motion } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';

  /** The drawn house in 3D (three.js, loaded on demand), alive like the plan. */
  let { plan, floorId, skin, keep = () => true, ontap = () => {} } = $props();

  let host = $state(null);
  let scene = $state(null);
  let failed = $state(null);

  onMount(() => {
    let gone = false;
    let mounted = null;
    import('./scene3d.js')
      .then(({ mount }) => {
        if (gone) return;
        mounted = mount(host, { plan: $state.snapshot(plan), onTap: (id) => ontap(id), shapeOf: (id) => shapeOf(hub.devices[id]) });
        scene = mounted;
      })
      .catch((e) => {
        failed = e?.message ?? t('maison.plan3d.pas_demarre');
      });
    return () => {
      gone = true;
      mounted?.dispose();
    };
  });

  const dims = (d) => d.points.some((p) => ['brightness', 'color', 'color_temp'].includes(p.key));

  /** What a device is in 3D: a lamp (a bulb that dims or tints, a
   *  fixture), a switch (a relay behind a wall switch: it drives a plain
   *  light), a socket, a camera, a clim, a TV or speaker, or a sensor. */
  function shapeOf(d) {
    if (!d) return 'sensor';
    const kind = deviceKind(d);
    if (kind === 'light' || kind === 'group') return isFixture(d) || dims(d) ? 'lamp' : 'switch';
    if (kind === 'plug') return 'plug';
    if (kind === 'camera') return 'camera';
    if (kind === 'climate') return 'climate';
    if (kind === 'tv' || kind === 'speaker') return 'media';
    return 'sensor';
  }

  /** What each placed device looks like now. */
  const devices = $derived(
    plan.floors.flatMap((f) =>
      (f.devices ?? []).map((s) => {
        const d = hub.devices[s.id];
        if (!d) return { id: s.id, shape: 'sensor', state: 'off' };
        const shape = shapeOf(d);
        // Filtered out: no marker (a lit lamp still lights the room).
        const hidden = !keep(d);
        const kind = deviceKind(d);
        const shines = kind === 'light';
        if (d.online === false) return { id: s.id, shape, state: 'off', hidden };
        if (shape === 'lamp') {
          const b = value(d.id, 'brightness');
          return { id: s.id, shape, state: isOn(d.id) ? 'lit' : 'idle', level: typeof b === 'number' ? Math.max(0.15, b / 100) : 1, hidden, shines };
        }
        if (shape === 'sensor') {
          const state = value(d.id, 'smoke') === true || leak(d.id) ? 'alarm' : doorOpen(d.id) === true || motion(d.id) ? 'open' : 'idle';
          return { id: s.id, shape, state, hidden };
        }
        const on = shape === 'media' ? value(d.id, 'playing') === true || isOn(d.id) : isOn(d.id);
        return { id: s.id, shape, state: on ? 'on' : 'idle', hidden, shines };
      }),
    ),
  );

  $effect(() => {
    scene?.update({ skin, floorId, devices });
  });
</script>

<div class="house3d" style="background:{skin.bg}">
  <div class="host" bind:this={host}></div>
  {#if failed}
    <p class="failed">{t('maison.plan3d.indisponible', { reason: failed })}</p>
  {:else if !scene}
    <p class="loading">{t('maison.plan3d.construction')}</p>
  {/if}
  {#if scene}
    <button class="reset" onclick={() => scene.reset()} aria-label={t('maison.plan3d.vue_ensemble')}>⤢</button>
  {/if}
</div>

<style>
  .house3d {
    position: relative;
    width: 100%;
    /* What is left of the screen under the page's header and tabs: the
       whole house in view without scrolling, the page still scrollable. */
    height: clamp(300px, calc(100dvh - 380px), 640px);
    overflow: hidden;
    border-radius: var(--r-lg);
    box-shadow: var(--shadow);
  }

  .host {
    position: absolute;
    inset: 0;
  }

  .loading,
  .failed {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    margin: 0;
    padding: 24px;
    color: var(--ink-2);
    text-align: center;
  }

  .reset {
    position: absolute;
    right: 10px;
    bottom: 10px;
    width: 36px;
    height: 36px;
    border-radius: 50%;
    border: none;
    background: var(--surface);
    color: var(--ink);
    box-shadow: var(--shadow-lift);
    font: inherit;
    font-size: 18px;
    font-weight: 700;
    cursor: pointer;
  }
</style>
