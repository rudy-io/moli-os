<script>
  import { changeMarks } from '../lib/contracts.js';
  import { t } from '../../lib/i18n.svelte.js';

  /** The 24 hours of a day as one strip: off-peak windows in blue over the
   *  peak hours in orange (the colours of the « Heures creuses / pleines »
   *  chip), a mark at the minute it is (`now`, minutes of the day; none in
   *  the editor's preview), and the time of each change written under it,
   *  a label too close to the one before it on a second row. */
  let { windows = [], now = null, label = '' } = $props();

  const DAY = 1440;
  // Each window as one or two pieces (one past midnight is cut there).
  const pieces = $derived(
    windows
      .filter((w) => w.from != null && w.to != null && w.from !== w.to)
      .flatMap((w) => (w.from < w.to ? [[w.from, w.to]] : [[w.from, DAY], [0, w.to]]))
      .map(([a, b]) => ({ left: (a / DAY) * 100, width: ((b - a) / DAY) * 100 })),
  );
  const marks = $derived(changeMarks(windows.filter((w) => w.from != null && w.to != null && w.from !== w.to)));
  const twoRows = $derived(marks.some((m) => m.row));
</script>

<!-- Bar, stems and times all placed on the same width, in the same box:
     a stem falls exactly on the edge of its blue stretch. -->
<div class="strip" class:two={twoRows} role="img" aria-label={label || t('energie.journee_tarif.aria')}>
  <div class="track">
    {#each pieces as p, i (i)}<i class="off" style:left="{p.left}%" style:width="{p.width}%"></i>{/each}
  </div>
  {#if now != null}<span class="now" style:left="{(now / DAY) * 100}%"></span>{/if}
  {#each marks as m (m.text)}
    <b class="stem" class:low={m.row} style:left="{m.at * 100}%"></b>
    <span class="mark num" class:low={m.row} class:start={m.at < 0.04} class:end={m.at > 0.96} style:left="{m.at * 100}%">{m.text}</span>
  {/each}
</div>

<style>
  .strip {
    position: relative;
    height: 32px;
  }

  .strip.two {
    height: 47px;
  }

  .track {
    position: absolute;
    inset: 0 0 auto;
    height: 10px;
    border-radius: 3px;
    background: var(--warm);
    overflow: hidden;
  }

  .off {
    position: absolute;
    top: 0;
    bottom: 0;
    background: var(--cool);
  }

  /* Now: a dark tick across the strip, ringed with the card's colour. */
  .now {
    position: absolute;
    top: -4px;
    height: 18px;
    width: 3px;
    border-radius: 2px;
    background: var(--ink);
    box-shadow: 0 0 0 1.5px var(--surface);
    transform: translateX(-50%);
  }

  /* A stem from the edge of the bar down to its time, centred on the
     change (2 px wide, half each side of it). */
  .stem {
    position: absolute;
    top: 10px;
    width: 2px;
    height: 5px;
    margin-left: -1px;
    background: var(--ink-3);
  }

  .stem.low {
    height: 20px;
    opacity: 0.6;
  }

  .mark {
    position: absolute;
    top: 15px;
    transform: translateX(-50%);
    white-space: nowrap;
    font-size: 11px;
    font-weight: 650;
    line-height: 15px;
    color: var(--ink-2);
  }

  .mark.low {
    top: 30px;
  }

  .mark.start {
    transform: translateX(-1px);
  }

  .mark.end {
    transform: translateX(calc(-100% + 1px));
  }
</style>
