<script>
  // The clouds of the last two hours, full screen: play, pause, or slide
  // through time by hand. The house pinned where it is.
  import { onMount } from 'svelte';
  import { sky, frameUrl, frameClock } from '../lib/sky.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  let { onclose } = $props();

  const frames = $derived(sky.satellite?.frames ?? []);
  const house = $derived(sky.satellite?.house ?? [0.5, 0.5]);
  const size = $derived(sky.satellite?.size ?? [960, 720]);
  let at = $state(Math.max(0, (sky.satellite?.frames?.length ?? 1) - 1));
  let playing = $state(true);
  let dialog;

  onMount(() => {
    dialog.showModal();
    let timer;
    const tick = () => {
      if (playing && frames.length) at = (at + 1) % frames.length;
      timer = setTimeout(tick, at === frames.length - 1 ? 1800 : 450);
    };
    timer = setTimeout(tick, 600);
    return () => clearTimeout(timer);
  });
</script>

<dialog bind:this={dialog} onclose={onclose} onclick={(e) => e.target === dialog && dialog.close()} aria-label={t('maison.meteo.nuages_titre')}>
  <div class="frame" style="aspect-ratio:{size[0]} / {size[1]}; width:min(100%, calc((96dvh - 140px) * {size[0] / size[1]}))">
    {#each frames as f, i (f)}
      <img src={frameUrl(f)} alt={i === at ? t('maison.meteo.nuages_alt', { time: frameClock(f) }) : ''} class:on={i === at} />
    {/each}
    <span class="dot" style="left:{house[0] * 100}%; top:{house[1] * 100}%" aria-hidden="true"></span>
    <button class="close" onclick={() => dialog.close()} aria-label={t('maison.meteo.fermer')}><Icon name="close" size={22} /></button>
  </div>
  <div class="controls">
    <button class="play" onclick={() => (playing = !playing)} aria-label={playing ? t('maison.meteo.pause') : t('maison.meteo.lecture')}>
      <Icon name={playing ? 'pause' : 'play'} size={22} />
    </button>
    <input
      type="range"
      min="0"
      max={Math.max(0, frames.length - 1)}
      bind:value={at}
      oninput={() => (playing = false)}
      aria-label={t('maison.meteo.moment')}
    />
    <strong class="num">{frames[at] ? frameClock(frames[at]) : ''}</strong>
  </div>
  <p class="credit">{t('maison.meteo.credit', { source: sky.satellite?.source ?? 'EUMETSAT' })}</p>
</dialog>

<style>
  dialog {
    border: 0;
    padding: 14px;
    width: min(1100px, 96vw);
    max-height: 96dvh;
    border-radius: var(--r-xl);
    background: #0b0f18;
    color: #fff;
    box-shadow: 0 30px 80px rgb(0 0 0 / 55%);
  }

  dialog::backdrop {
    background: rgb(4 6 12 / 72%);
    backdrop-filter: blur(4px);
  }

  .frame {
    position: relative;
    margin: 0 auto;
    border-radius: var(--r-lg);
    overflow: hidden;
    background: #000;
  }

  .frame img {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: cover;
    opacity: 0;
    transition: opacity 0.35s linear;
  }

  .frame img.on {
    opacity: 1;
  }

  .dot {
    position: absolute;
    width: 14px;
    height: 14px;
    margin: -7px 0 0 -7px;
    border-radius: 50%;
    background: var(--warm);
    box-shadow:
      0 0 0 3px rgb(255 255 255 / 90%),
      0 0 18px 4px rgb(240 165 58 / 60%);
  }

  .close {
    position: absolute;
    top: 10px;
    right: 10px;
    width: 40px;
    height: 40px;
    border: 0;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: rgb(6 10 20 / 55%);
    color: #fff;
  }

  .controls {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 12px 6px 4px;
  }

  .play {
    width: 44px;
    height: 44px;
    flex: none;
    border: 0;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: rgb(255 255 255 / 12%);
    color: #fff;
  }

  input {
    flex: 1;
    accent-color: var(--warm);
  }

  strong {
    min-width: 52px;
    text-align: right;
    font-size: 18px;
  }

  .credit {
    font-size: 12px;
    opacity: 0.6;
    padding: 0 6px;
  }
</style>
