<script>
  /** A drink drawn in its own cup or glass, all at one scale on one floor,
   *  so that sizes read: an espresso's small cup on its saucer, a coffee's
   *  cup, a cappuccino's bowl under its foam, the tall glass of a latte
   *  macchiato in three layers, a barista's mug… Unknown drinks: a coffee. */
  let { drink, size = 32 } = $props();

  // A cup's body from (x, top), `w` wide and `h` tall, its bottom rounded.
  const body = (x, top, w, h, r = 3) => `M${x} ${top}h${w}v${h - r}a${r} ${r} 0 0 1 -${r} ${r}h-${w - 2 * r}a${r} ${r} 0 0 1 -${r} -${r}z`;
  // Its handle on the right, `s` high.
  const handle = (x, top, s = 2.4) => `M${x} ${top + 1.4}h1a${s} ${s} 0 0 1 0 ${2 * s}h-1`;
  const saucer = (cx, w) => `M${cx - w / 2} 29.2h${w}`;

  // Cups: [x, top, w, h, r, filling, crema band?]; glasses and mugs stand on
  // the floor (29), cups on a saucer.
  const CUPS = {
    espresso: [[11, 21, 10, 7, 3, 'coffee', true]],
    espresso_doppio: [[10.5, 19, 11, 9, 3, 'coffee', true]],
    two_espressi: [
      [3, 21, 9, 7, 3, 'coffee', true],
      [17, 21, 9, 7, 3, 'coffee', true],
    ],
    coffee: [[8, 16, 14, 12, 3.5, 'coffee', true]],
    two_coffees: [
      [1.5, 18, 11, 10, 3, 'coffee', true],
      [16.5, 18, 11, 10, 3, 'coffee', true],
    ],
    cappuccino: [[6, 18, 17, 10, 5, 'milky', false]],
    flat_white: [[8, 17, 14, 11, 4, 'milky', false]],
    cafe_barista: [[7, 12, 16, 17, 2.5, 'coffee', true]],
    barista_lungo: [[8.5, 7, 13, 22, 2.5, 'coffee', true]],
    hot_water: [[8, 14, 15, 15, 2.5, 'water', false]],
  };
  const SAUCERS = {
    espresso: [[16, 16]],
    espresso_doppio: [[16, 18]],
    two_espressi: [
      [7.5, 13],
      [21.5, 13],
    ],
    coffee: [[15, 22]],
    two_coffees: [
      [7, 14],
      [22, 14],
    ],
    cappuccino: [[14.5, 24]],
    flat_white: [[15, 22]],
  };
  // Glasses: [x, top, w], down to the floor, and their layers from the
  // bottom: [height, filling]; `dome`: foam above the rim.
  const GLASSES = {
    cortado: { at: [11, 17, 10], layers: [[5.5, 'coffee'], [4.5, 'milky'], [1, 'foam']] },
    espresso_macchiato: { at: [11.5, 19, 9], layers: [[5, 'coffee'], [4, 'foam']], dome: true },
    latte_macchiato: { at: [10.5, 3, 11], layers: [[10, 'milk'], [6.5, 'latte'], [8.5, 'foam']] },
    milk_foam: { at: [11, 15, 10], layers: [[13.4, 'foam']], dome: true },
  };

  const known = $derived(drink in CUPS || drink in GLASSES);
  const shape = $derived(known ? drink : 'coffee');
  const glass = $derived(GLASSES[shape]);

  // The drawing's box (strokes included), to set it in the middle of the
  // 32 × 32 square: the drawings share one floor, so left as they are they
  // sit low. A cup's handle is `handle(…)` wide; the foam of a cappuccino
  // rises above its rim, a hot water's steam above its cup.
  const LINE = 0.65;
  const SAUCER = 0.8;
  const EXTRA_TOP = { cappuccino: 18 - 3.25, hot_water: 3.5 };
  function extent(s) {
    const box = { x0: Infinity, y0: Infinity, x1: -Infinity, y1: -Infinity };
    const grow = (x0, y0, x1, y1, pad = LINE) => {
      box.x0 = Math.min(box.x0, x0 - pad);
      box.y0 = Math.min(box.y0, y0 - pad);
      box.x1 = Math.max(box.x1, x1 + pad);
      box.y1 = Math.max(box.y1, y1 + pad);
    };
    if (GLASSES[s]) {
      const [x, top, w] = GLASSES[s].at;
      grow(x, GLASSES[s].dome ? top - w / 6 : top, x + w, 29);
    } else {
      for (const [cx, w] of SAUCERS[s] ?? []) grow(cx - w / 2, 29.2, cx + w / 2, 29.2, SAUCER);
      for (const [x, top, w, h] of CUPS[s]) grow(x, top, x + w + 1 + (h > 14 ? 3.4 : h > 9 ? 2.6 : 1.9), top + h);
      if (EXTRA_TOP[s]) grow(box.x0 + LINE, EXTRA_TOP[s], box.x1 - LINE, EXTRA_TOP[s]);
    }
    return box;
  }
  const centred = $derived.by(() => {
    const b = extent(shape);
    return `translate(${(16 - (b.x0 + b.x1) / 2).toFixed(2)} ${(16 - (b.y0 + b.y1) / 2).toFixed(2)})`;
  });
</script>

<svg class="drink-icon" viewBox="0 0 32 32" width={size} height={size} aria-hidden="true" focusable="false">
  <g transform={centred}>
  {#if glass}
    {@const [x, top, w] = glass.at}
    {@const floor = 28.4}
    {#each glass.layers as [h, fill], i (i)}
      {@const below = glass.layers.slice(0, i).reduce((s, [b]) => s + b, 0)}
      <rect x={x + 0.6} y={floor - below - h} width={w - 1.2} height={h} class={fill} />
    {/each}
    {#if glass.dome}<path d="M{x} {top}q{w / 2} -{w / 3} {w} 0z" class="foam line" />{/if}
    {#if shape === 'milk_foam'}
      <circle cx="14" cy="19" r="1" class="bubble" /><circle cx="17.5" cy="22" r="1.2" class="bubble" /><circle cx="14.5" cy="25.5" r="0.9" class="bubble" /><circle cx="18" cy="17" r="0.7" class="bubble" />
    {/if}
    <path d={body(x, top, w, 29 - top, 1.2)} class="line" />
  {:else}
    {#each SAUCERS[shape] ?? [] as [cx, w] (cx)}
      <path d={saucer(cx, w)} class="line saucer" />
    {/each}
    {#each CUPS[shape] as [x, top, w, h, r, fill, crema] (x)}
      <path d={body(x, top + 1, w, h - 1, r)} class={fill} />
      {#if crema}<rect x={x + 0.6} y={top + 1} width={w - 1.2} height={1.4} class="crema" />{/if}
      <path d={handle(x + w, top, h > 14 ? 3.4 : h > 9 ? 2.6 : 1.9)} class="line" />
      <path d={body(x, top, w, h, r)} class="line" />
    {/each}
    {#if shape === 'cappuccino'}
      <path d="M6 18q8.5 -6.5 17 0z" class="foam line" />
    {:else if shape === 'flat_white'}
      <ellipse cx="15" cy="18.2" rx="3" ry="0.9" class="foam" />
    {:else if shape === 'hot_water'}
      <path d="M12 11.5q-1.5 -2 0 -4t0 -4M15.5 11.5q-1.5 -2 0 -4t0 -4M19 11.5q-1.5 -2 0 -4t0 -4" class="line steam" />
    {/if}
  {/if}
  </g>
</svg>

<style>
  .drink-icon {
    flex: none;
    display: block;
    --coffee: #5b3a22;
    --crema: #c58f55;
    --milky: #c9a27c;
    --latte: #9b6a3f;
    --milk: #f6efe4;
    --foam: #fff8ec;
    --water: #a9d8f2;
  }

  .line {
    fill: none;
    stroke: currentColor;
    stroke-width: 1.3;
    stroke-linejoin: round;
    stroke-linecap: round;
  }

  .saucer {
    stroke-width: 1.6;
  }

  .steam {
    stroke-width: 1.1;
    opacity: 0.6;
  }

  .coffee {
    fill: var(--coffee);
  }

  .crema {
    fill: var(--crema);
  }

  .milky {
    fill: var(--milky);
  }

  .latte {
    fill: var(--latte);
  }

  .milk {
    fill: var(--milk);
  }

  .foam {
    fill: var(--foam);
  }

  .water {
    fill: var(--water);
  }

  .bubble {
    fill: none;
    stroke: var(--milky);
    stroke-width: 0.7;
  }
</style>
