<script>
  import { act, value, reachable, savePref, num } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** The living-room remote. Keys go straight to the TV, one press each. */
  let { tv } = $props();

  let mode = $state(readMode());
  let flash = $state(null);
  let busy = $state(false);

  function readMode() {
    try {
      return localStorage.getItem('maison-remote') === 'pad' ? 'pad' : 'keys';
    } catch {
      return 'keys';
    }
  }

  const on = $derived(value(tv, 'power') === true);
  const ok = $derived(reachable(tv));
  const volume = $derived(value(tv, 'volume'));
  const muted = $derived(value(tv, 'mute') === true);

  async function key(name) {
    if (!on) return;
    flash = name;
    setTimeout(() => flash === name && (flash = null), 180);
    if (navigator.vibrate) navigator.vibrate(8);
    await act(`${tv}/key`, name, t('salon.tele.label'));
  }

  async function power() {
    busy = true;
    await act(`${tv}/power`, !on, on ? t('salon.tele.eteindre') : t('salon.tele.allumer'));
    busy = false;
  }

  function setMode(m) {
    mode = m;
    savePref('maison-remote', m);
  }

  // Touchpad: a swipe moves, a tap validates. Long swipes repeat.
  let start = null;
  let moved = false;
  const STEP = 38;

  function down(e) {
    e.currentTarget.setPointerCapture(e.pointerId);
    start = { x: e.clientX, y: e.clientY };
    moved = false;
  }

  function move(e) {
    if (!start) return;
    const dx = e.clientX - start.x;
    const dy = e.clientY - start.y;
    if (Math.max(Math.abs(dx), Math.abs(dy)) < STEP) return;
    moved = true;
    key(Math.abs(dx) > Math.abs(dy) ? (dx > 0 ? 'CursorRight' : 'CursorLeft') : dy > 0 ? 'CursorDown' : 'CursorUp');
    start = { x: e.clientX, y: e.clientY };
  }

  function up() {
    if (start && !moved) key('Confirm');
    start = null;
  }

  function onkey(e) {
    if (e.defaultPrevented || e.target.closest?.('input, textarea, select, button, [contenteditable]')) return;
    const map = { ArrowUp: 'CursorUp', ArrowDown: 'CursorDown', ArrowLeft: 'CursorLeft', ArrowRight: 'CursorRight', Enter: 'Confirm', Backspace: 'Back' };
    if (map[e.key]) {
      e.preventDefault();
      key(map[e.key]);
    }
  }
</script>

<svelte:window onkeydown={onkey} />

<section class="remote" class:on aria-label={t('salon.tele.region')}>
  <header>
    <div class="modes" role="group" aria-label={t('salon.tele.navigation')}>
      <button aria-pressed={mode === 'keys'} onclick={() => setMode('keys')}>{t('salon.tele.touches')}</button>
      <button aria-pressed={mode === 'pad'} onclick={() => setMode('pad')}>{t('salon.tele.pave')}</button>
    </div>
    <button class="power" class:lit={on} onclick={power} disabled={busy} aria-label={on ? t('salon.tele.eteindre_television') : t('salon.tele.allumer_television')}>
      <Icon name="power" size={24} />
    </button>
  </header>

  {#if mode === 'keys'}
    <div class="dpad" role="group" aria-label={t('salon.tele.fleches')}>
      <button class="up" class:flash={flash === 'CursorUp'} onclick={() => key('CursorUp')} disabled={!on} aria-label={t('salon.tele.haut')}><Icon name="chevron-up" size={30} /></button>
      <button class="left" class:flash={flash === 'CursorLeft'} onclick={() => key('CursorLeft')} disabled={!on} aria-label={t('salon.tele.gauche')}><Icon name="chevron-left" size={30} /></button>
      <button class="ok" class:flash={flash === 'Confirm'} onclick={() => key('Confirm')} disabled={!on} aria-label={t('salon.tele.valider')}>OK</button>
      <button class="right" class:flash={flash === 'CursorRight'} onclick={() => key('CursorRight')} disabled={!on} aria-label={t('salon.tele.droite')}><Icon name="chevron-right" size={30} /></button>
      <button class="down" class:flash={flash === 'CursorDown'} onclick={() => key('CursorDown')} disabled={!on} aria-label={t('salon.tele.bas')}><Icon name="chevron-down" size={30} /></button>
    </div>
  {:else}
    <div
      class="pad"
      class:disabled={!on}
      role="button"
      tabindex="0"
      aria-label={t('salon.tele.pave_aide')}
      onpointerdown={down}
      onpointermove={move}
      onpointerup={up}
      onpointercancel={() => (start = null)}>
      <span class="pad-hint">
        <b>OK</b>
        <small>{t('salon.tele.glisser')}<br />{t('salon.tele.toucher')}</small>
      </span>
      {#if flash}<i class="ripple"></i>{/if}
    </div>
  {/if}

  <div class="row three">
    <button onclick={() => key('Back')} disabled={!on} aria-label={t('salon.tele.retour')}><Icon name="back" size={24} /></button>
    <button onclick={() => key('Home')} disabled={!on} aria-label={t('salon.tele.accueil')}><Icon name="home" size={24} /></button>
    <button onclick={() => key('Options')} disabled={!on} aria-label={t('salon.tele.options')}><Icon name="dots" size={24} /></button>
  </div>

  <div class="row three">
    <button onclick={() => key('Previous')} disabled={!on} aria-label={t('salon.tele.precedent')}><Icon name="previous" size={24} /></button>
    <button class="primary" onclick={() => key('PlayPause')} disabled={!on} aria-label={t('salon.tele.lecture_pause')}><Icon name="play-pause" size={26} /></button>
    <button onclick={() => key('Next')} disabled={!on} aria-label={t('salon.tele.suivant')}><Icon name="next" size={24} /></button>
  </div>

  <div class="volume">
    <button onclick={() => key('VolumeDown')} disabled={!on} aria-label={t('salon.tele.volume_moins')}><Icon name="volume-minus" size={24} /></button>
    <div class="readout">
      <small>{t('salon.tele.volume_tv')}</small>
      <b class="num">{muted ? t('salon.tele.muet') : volume != null ? num(volume) : '—'}</b>
    </div>
    <button onclick={() => key('VolumeUp')} disabled={!on} aria-label={t('salon.tele.volume_plus')}><Icon name="volume-plus" size={24} /></button>
    <button class="mute" class:active={muted} onclick={() => key('Mute')} disabled={!on} aria-label={t('salon.couper_son')}><Icon name={muted ? 'volume-off' : 'volume-high'} size={22} /></button>
  </div>

  <p class="status" role="status">{busy ? t('salon.tele.un_instant') : !ok ? t('salon.tele.veille_profonde') : on ? t('salon.tele.allumee') : t('salon.tele.en_veille')}</p>
</section>

<style>
  /* A porcelain remote: soft body, keys that press in. */
  .remote {
    --key: var(--surface);
    --key-shadow: 0 3px 0 color-mix(in srgb, var(--ink) 10%, transparent), 0 8px 18px -8px color-mix(in srgb, var(--ink) 30%, transparent);
    background: linear-gradient(180deg, var(--surface-2), var(--surface-3));
    border-radius: 44px;
    padding: 22px 22px 18px;
    box-shadow: var(--shadow-lift), inset 0 1px 0 rgb(255 255 255 / 60%);
    display: grid;
    gap: 18px;
    max-width: 380px;
    width: 100%;
    margin: 0 auto;
    touch-action: manipulation;
  }

  header {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }

  .modes {
    display: inline-flex;
    background: var(--surface-3);
    border-radius: 999px;
    padding: 3px;
  }

  .modes button {
    border: 0;
    background: none;
    padding: 7px 12px;
    border-radius: 999px;
    font-size: 12px;
    font-weight: 650;
    color: var(--ink-3);
  }

  .modes button[aria-pressed='true'] {
    background: var(--surface);
    color: var(--ink);
    box-shadow: var(--shadow);
  }

  button {
    border: 0;
    background: var(--key);
    border-radius: 18px;
    box-shadow: var(--key-shadow);
    display: grid;
    place-items: center;
    color: var(--ink-2);
    transition: transform 0.08s, box-shadow 0.08s, background 0.2s;
  }

  button:active:not(:disabled),
  .flash {
    transform: translateY(3px);
    box-shadow: 0 0 0 transparent;
  }

  button:disabled {
    opacity: 0.45;
  }

  .modes button:active {
    transform: none;
  }

  .power {
    width: 54px;
    height: 54px;
    border-radius: 50%;
    color: var(--alert);
  }

  .power.lit {
    background: var(--alert);
    color: #fff;
    box-shadow: 0 3px 0 color-mix(in srgb, var(--alert) 60%, #000), 0 8px 22px -6px var(--alert);
  }

  .dpad {
    position: relative;
    width: 250px;
    height: 250px;
    margin: 4px auto;
    border-radius: 50%;
    background: var(--surface);
    box-shadow: var(--key-shadow), inset 0 0 0 10px var(--surface-2);
  }

  .dpad button {
    position: absolute;
    width: 74px;
    height: 74px;
    background: none;
    box-shadow: none;
    border-radius: 50%;
  }

  .dpad button:hover:not(:disabled) {
    background: var(--surface-2);
  }

  .dpad .up {
    top: 10px;
    left: 88px;
  }

  .dpad .down {
    bottom: 10px;
    left: 88px;
  }

  .dpad .left {
    left: 10px;
    top: 88px;
  }

  .dpad .right {
    right: 10px;
    top: 88px;
  }

  .dpad .ok {
    left: 70px;
    top: 70px;
    width: 110px;
    height: 110px;
    background: var(--surface-2);
    box-shadow: var(--key-shadow);
    font-weight: 750;
    font-size: 20px;
    color: var(--ink);
  }

  .pad {
    position: relative;
    height: 250px;
    border-radius: 34px;
    background: radial-gradient(circle at 50% 40%, var(--surface), var(--surface-2));
    box-shadow: inset 0 2px 6px color-mix(in srgb, var(--ink) 10%, transparent);
    display: grid;
    place-items: center;
    touch-action: none;
    user-select: none;
    overflow: hidden;
  }

  .pad.disabled {
    opacity: 0.45;
    pointer-events: none;
  }

  .pad-hint {
    display: grid;
    text-align: center;
    gap: 6px;
    color: var(--ink-3);
    pointer-events: none;
  }

  .pad-hint b {
    font-size: 22px;
    color: var(--ink-2);
  }

  .pad-hint small {
    font-size: 12px;
  }

  .ripple {
    position: absolute;
    inset: 30%;
    border-radius: 50%;
    background: var(--cool);
    opacity: 0.15;
    animation: ripple 0.3s ease-out;
  }

  @keyframes ripple {
    from {
      transform: scale(0.4);
      opacity: 0.35;
    }
  }

  .row {
    display: grid;
    gap: 14px;
  }

  .row.three {
    grid-template-columns: repeat(3, 1fr);
  }

  .row button {
    height: 58px;
  }

  .row .primary {
    background: var(--ink);
    color: var(--bg);
  }

  .volume {
    display: grid;
    grid-template-columns: 58px 1fr 58px 50px;
    gap: 10px;
    align-items: center;
  }

  .volume button {
    height: 58px;
  }

  .volume .mute {
    height: 50px;
    border-radius: 50%;
  }

  .volume .mute.active {
    color: var(--alert);
  }

  .readout {
    text-align: center;
    display: grid;
  }

  .readout small {
    font-size: 10px;
    letter-spacing: 0.12em;
    color: var(--ink-3);
    font-weight: 700;
  }

  .readout b {
    font-size: 22px;
    font-weight: 650;
  }

  .status {
    text-align: center;
    font-size: 13px;
    color: var(--ink-3);
    font-weight: 600;
  }
</style>
