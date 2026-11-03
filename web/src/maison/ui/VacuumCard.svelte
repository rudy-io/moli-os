<script>
  import { value, act, num, nameOf, device } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import { canWrite, writableChoices } from '../lib/capabilities.js';

  /** A robot vacuum: what it is doing, start or pause, back to its base, ring,
   *  suction. Each control shows only where its point can be written. Lifting
   *  a pause keeps the session: `clean` would start another one. */
  let { id, name = null } = $props();

  const STATES = {
    idle: 'attente',
    standby: 'attente',
    sleep: 'veille',
    dormant: 'veille',
    totaling: 'nettoyage',
    sweep: 'nettoyage',
    mop: 'nettoyage',
    smart: 'nettoyage',
    cleaning: 'nettoyage',
    pause: 'pause',
    paused: 'pause',
    tocharge: 'retour',
    goto_charge: 'retour',
    chargring: 'charge',
    charging: 'charge',
    fullcharge: 'charge_finie',
    charge_done: 'charge_finie',
    fault: 'panne',
  };
  const SUCTION = ['quiet', 'gentle', 'normal', 'strong', 'auto', 'max'];

  const d = $derived(device(id));
  const state = $derived(value(id, 'state'));
  const word = $derived(STATES[state]);
  const started = $derived(value(id, 'clean') === true);
  const paused = $derived(value(id, 'pause') === true);
  const cleaning = $derived(started && !paused);
  const atBase = $derived(word === 'charge' || word === 'charge_finie');
  const fault = $derived(value(id, 'fault') === true);
  const area = $derived(value(id, 'clean_area'));
  const seconds = $derived(value(id, 'clean_time'));
  const minutes = $derived(Number.isFinite(seconds) ? Math.floor(seconds / 60) : null);
  const battery = $derived(value(id, 'battery'));
  const suction = $derived(value(id, 'fan_speed'));
  const speeds = $derived(writableChoices(d, 'fan_speed'));
  const speedName = (speed) => (SUCTION.includes(speed) ? t(`maison.robot.aspiration.${speed}`) : speed);
</script>

<section class="card robot">
  <div class="card-head">
    <h2><Icon name="robot-vacuum" size={18} />{nameOf(id, name)}</h2>
    {#if state != null}
      <span class="chip {fault ? 'alert' : cleaning ? 'warm' : ''}">{word ? t(`maison.robot.etat.${word}`) : state}</span>
    {/if}
  </div>
  <p class="facts muted">
    {#if battery != null}<span><Icon name="battery" size={16} />{num(battery)} %</span>{/if}
    {#if started && area != null}<span>{t('maison.robot.surface', { area: num(area) })}</span>{/if}
    {#if started && minutes != null}<span>{t('maison.robot.duree', { minutes: num(minutes) })}</span>{/if}
  </p>
  {#if fault}
    <p class="problem">{t('maison.robot.probleme', { detail: value(id, 'detail') ?? '' })}</p>
  {/if}
  <div class="orders">
    {#if cleaning && canWrite(d, 'pause')}
      <button class="main" onclick={() => act(`${id}/pause`, true, t('maison.robot.pause'))}><Icon name="pause" size={20} />{t('maison.robot.pause')}</button>
    {:else if paused && canWrite(d, 'pause')}
      <button class="main" onclick={() => act(`${id}/pause`, false, t('maison.robot.reprendre'))}><Icon name="play" size={20} />{t('maison.robot.reprendre')}</button>
    {:else if canWrite(d, 'clean')}
      <button class="main" onclick={() => act(`${id}/clean`, true, t('maison.robot.demarrer'))}><Icon name="play" size={20} />{t('maison.robot.demarrer')}</button>
    {/if}
    {#if started && canWrite(d, 'clean')}
      <button onclick={() => act(`${id}/clean`, false, t('maison.robot.arreter'))}><Icon name="stop" size={20} />{t('maison.robot.arreter')}</button>
    {/if}
    {#if !atBase && canWrite(d, 'dock')}
      <button onclick={() => act(`${id}/dock`, true, t('maison.robot.base'))}><Icon name="home" size={20} />{t('maison.robot.base')}</button>
    {/if}
    {#if canWrite(d, 'locate')}
      <button onclick={() => act(`${id}/locate`, true, t('maison.robot.sonner'))}><Icon name="bell" size={20} />{t('maison.robot.sonner')}</button>
    {/if}
  </div>
  {#if speeds.length}
    <div class="speeds" role="group" aria-label={t('maison.robot.aspiration')}>
      {#each speeds as speed (speed)}
        <button class:on={speed === suction} aria-pressed={speed === suction} onclick={() => speed !== suction && act(`${id}/fan_speed`, speed, t('maison.robot.aspiration'))}>{speedName(speed)}</button>
      {/each}
    </div>
  {/if}
</section>

<style>
  .facts {
    display: flex;
    flex-wrap: wrap;
    gap: 14px;
    margin: 0;
    min-height: 20px;
  }

  .facts span {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }

  .problem {
    color: var(--alert);
    margin: 10px 0 0;
  }

  .orders {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-top: 14px;
  }

  .orders button {
    flex: 1 1 auto;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    min-height: 48px;
    padding: 0 14px;
    border: 0;
    border-radius: var(--r-md, 12px);
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
  }

  .orders button.main {
    background: var(--ink);
    color: var(--bg);
    font-weight: 650;
  }

  .speeds {
    display: flex;
    gap: 6px;
    margin-top: 10px;
  }

  .speeds button {
    flex: 1;
    min-height: 40px;
    padding: 0 6px;
    border: 0;
    border-radius: var(--r-md, 12px);
    background: transparent;
    color: var(--ink-2);
    font: inherit;
  }

  .speeds button.on {
    background: var(--surface-2);
    color: var(--ink);
    font-weight: 650;
  }
</style>
