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

<div class="strip" role="img" aria-label={label || t('energie.journee_tarif.aria')}>
  <div class="track">
    {#each pieces as p, i (i)}<i class="off" style:left="{p.left}%" style:width="{p.width}%"></i>{/each}
    {#each [6, 12, 18] as h (h)}<b class="hour" style:left="{(h / 24) * 100}%"></b>{/each}
    {#if now != null}<span class="now" style:left="{(now / DAY) * 100}%"></span>{/if}
  </div>
  <div class="marks" class:two={twoRows}>
    {#each marks as m (m.text)}
      <span class="mark num" class:low={m.row} class:start={m.at < 0.04} class:end={m.at > 0.96} style:left="{m.at * 100}%">{m.text}</span>
    {/each}
  </div>
</div>

<style>
  .strip {
    display: grid;
    gap: 3px;
  }

  .track {
    position: relative;
    height: 10px;
    border-radius: 3px;
    background: var(--warm);
    overflow: visible;
  }

  .off {
    position: absolute;
    top: 0;
    bottom: 0;
    background: var(--cool);
  }

  /* 6 h, noon, 18 h: faint notches to read the day by. */
  .hour {
    position: absolute;
    top: 2px;
    bottom: 2px;
    width: 1px;
    background: var(--surface);
    opacity: 0.55;
  }

  /* Now: a dark tick across the strip, ringed with the card's colour. */
  .now {
    position: absolute;
    top: -4px;
    bottom: -4px;
    width: 3px;
    border-radius: 2px;
    background: var(--ink);
    box-shadow: 0 0 0 1.5px var(--surface);
    transform: translateX(-50%);
  }

  .marks {
    position: relative;
    height: 15px;
    font-size: 11px;
    font-weight: 650;
    color: var(--ink-2);
  }

  .marks.two {
    height: 29px;
  }

  .mark {
    position: absolute;
    top: 0;
    transform: translateX(-50%);
    white-space: nowrap;
    line-height: 15px;
  }

  /* A thin stem joins a label to its change. */
  .mark::before {
    content: '';
    position: absolute;
    left: 50%;
    top: -3px;
    width: 1px;
    height: 3px;
    background: var(--ink-3);
  }

  .mark.low {
    top: 14px;
  }

  /* On the second row, the change on the strip just above says where. */
  .mark.low::before {
    display: none;
  }

  .mark.start {
    transform: none;
  }

  .mark.start::before {
    left: 0;
  }

  .mark.end {
    transform: translateX(-100%);
  }

  .mark.end::before {
    left: 100%;
  }
</style>
