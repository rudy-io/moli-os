<script>
  import { act, value, reachable, savePref, num, hub, home } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** The living-room remote. Keys go straight to the TV, one press each;
   *  an arrow held (or a swipe kept under the finger) is held on the TV too,
   *  as with its own remote: a video seeks faster, a list scrolls.
   *  `speaker`: a soundbar beside it; while it plays the TV (its `source`
   *  is `tv`), the volume is the speaker's: the TV's own then sets its
   *  silent speakers. By default, the living room's of the layout. */
  let { tv, speaker = null } = $props();
  const soundbar = $derived(speaker ?? (home.config?.salon?.tv === tv ? home.config?.salon?.speaker : null));
  const sound = $derived(soundbar && value(soundbar, 'source') === 'tv' ? soundbar : null);
  const target = $derived(sound ?? tv);

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
  const volume = $derived(value(target, 'volume'));
  const muted = $derived(value(target, 'mute') === true);
  const points = $derived(hub.devices[tv]?.points ?? []);
  const maxVolume = $derived((hub.devices[target]?.points ?? []).find((p) => p.key === 'volume')?.kind?.max ?? 100);
  const canWrite = $derived(points.some((p) => p.key === 'text'));
  const canHold = $derived(points.some((p) => p.key === 'hold'));
  const volumeLabel = $derived(sound ? (hub.devices[sound]?.model ?? hub.devices[sound]?.native_name ?? '').toUpperCase() : t('salon.tele.volume_tv'));

  /** One step, or mute: the speaker's own when it plays the TV. */
  function step(delta) {
    if (!sound) return key(delta > 0 ? 'VolumeUp' : 'VolumeDown');
    if (shown == null) return;
    const level = Math.min(maxVolume, Math.max(0, shown + delta));
    held = level;
    sent = null;
    sendVolume(level);
  }

  function toggleMute() {
    if (!sound) return key('Mute');
    act(`${sound}/mute`, !muted, t('salon.couper_son'));
  }

  // The volume rail: slid from the bottom up, the TV follows as it moves
  // (an exact level, a few times a second), the last one when let go.
  let sliding = $state(null);
  let held = $state(null);
  let sent = null;
  let sentAt = 0;
  let later = null;
  const shown = $derived(sliding ?? held ?? volume);
  const fill = $derived(shown != null && maxVolume > 0 ? Math.min(1, Math.max(0, shown / maxVolume)) : 0);

  function sendVolume(level) {
    if (level === sent) return;
    sent = level;
    sentAt = Date.now();
    act(`${target}/volume`, level, volumeLabel);
  }

  function slideTo(level) {
    sliding = level;
    clearTimeout(later);
    const wait = 250 - (Date.now() - sentAt);
    if (wait <= 0) sendVolume(level);
    else later = setTimeout(() => sendVolume(level), wait);
  }

  function levelAt(e, rail) {
    const box = rail.getBoundingClientRect();
    const part = 1 - (e.clientY - box.top) / box.height;
    return Math.round(Math.min(1, Math.max(0, part)) * maxVolume);
  }

  function railDown(e) {
    if (!on) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    sent = null;
    slideTo(levelAt(e, e.currentTarget));
  }

  function railMove(e) {
    if (sliding != null) slideTo(levelAt(e, e.currentTarget));
  }

  function railUp() {
    if (sliding == null) return;
    clearTimeout(later);
    sendVolume(sliding);
    // Shown as set until the TV reports it (it is read every few seconds).
    held = sliding;
    sliding = null;
    setTimeout(() => (held = null), 6000);
  }

  $effect(() => {
    if (held != null && volume === held) held = null;
  });

  function railKey(e) {
    const step = { ArrowUp: 1, ArrowRight: 1, ArrowDown: -1, ArrowLeft: -1, PageUp: 5, PageDown: -5 }[e.key];
    if (!step || !on || shown == null) return;
    e.preventDefault();
    const level = Math.min(maxVolume, Math.max(0, shown + step));
    held = level;
    sent = null;
    sendVolume(level);
  }

  // The keyboard: what is typed goes into the text field open on the TV
  // (a search), a moment after the last key; Enter starts the search.
  let typing = $state(false);
  let text = $state('');
  let typed = null;
  let field = $state(null);

  function toggleTyping() {
    typing = !typing;
    if (typing) setTimeout(() => field?.focus(), 0);
  }

  function onText() {
    clearTimeout(typed);
    if (!text.trim()) return;
    typed = setTimeout(() => act(`${tv}/text`, text, t('salon.tele.clavier')), 350);
  }

  function search(e) {
    e.preventDefault();
    clearTimeout(typed);
    if (!on) return;
    act(`${tv}/text`, `${text}\n`, t('salon.tele.clavier'));
    text = '';
  }

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

  // Held keys: after HOLD_AFTER still pressed, the TV holds the key until
  // let go (`release`; the TV lets go by itself if that never comes).
  const HOLD_AFTER = 450;
  let holdTimer = null;
  let holding = null;
  let suppress = false;

  function holdSoon(name) {
    letGo();
    suppress = false;
    if (!canHold || !on) return;
    holdTimer = setTimeout(() => {
      holdTimer = null;
      holding = name;
      flash = name;
      if (navigator.vibrate) navigator.vibrate(15);
      act(`${tv}/hold`, name, t('salon.tele.label'));
    }, HOLD_AFTER);
  }

  /** Stops a hold (or the wait before one); true when a key was held. */
  function letGo() {
    clearTimeout(holdTimer);
    holdTimer = null;
    if (!holding) return false;
    if (flash === holding) flash = null;
    holding = null;
    // The click that follows the pointer's release is not one more press.
    suppress = true;
    act(`${tv}/hold`, 'release', t('salon.tele.label'));
    return true;
  }

  function tap(name) {
    if (suppress) {
      suppress = false;
      return;
    }
    key(name);
  }

  // Touchpad: a swipe moves, a tap validates. Long swipes repeat; a swipe
  // kept under the finger holds its arrow.
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
    const dir = Math.abs(dx) > Math.abs(dy) ? (dx > 0 ? 'CursorRight' : 'CursorLeft') : dy > 0 ? 'CursorDown' : 'CursorUp';
    start = { x: e.clientX, y: e.clientY };
    // Already held that way: the finger going on does not press it again.
    if (holding === dir) return;
    letGo();
    key(dir);
    holdSoon(dir);
  }

  function up() {
    const held = letGo();
    if (start && !moved && !held) key('Confirm');
    start = null;
  }

  function cancel() {
    letGo();
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
    {#if canWrite}
      <button class="typing" class:lit={typing} onclick={toggleTyping} disabled={!on} aria-pressed={typing} aria-label={t('salon.tele.clavier')}>
        <Icon name="keyboard" size={22} />
      </button>
    {/if}
    <button class="power" class:lit={on} onclick={power} disabled={busy} aria-label={on ? t('salon.tele.eteindre_television') : t('salon.tele.allumer_television')}>
      <Icon name="power" size={24} />
    </button>
  </header>

  {#if typing && canWrite}
    <form class="write" onsubmit={search}>
      <input
        bind:this={field}
        bind:value={text}
        oninput={onText}
        placeholder={t('salon.tele.clavier_placeholder')}
        aria-label={t('salon.tele.clavier')}
        enterkeyhint="search"
        autocomplete="off"
        spellcheck="false"
        disabled={!on} />
      <button class="go" disabled={!on} aria-label={t('salon.tele.chercher')}><Icon name="send" size={20} /></button>
      <small>{t('salon.tele.clavier_aide')}</small>
    </form>
  {/if}

  <div class="middle">
  {#if mode === 'keys'}
    <div class="dpad" role="group" aria-label={t('salon.tele.fleches')} oncontextmenu={(e) => e.preventDefault()}>
      <button class="up" class:flash={flash === 'CursorUp'} onclick={() => tap('CursorUp')} onpointerdown={() => holdSoon('CursorUp')} onpointerup={letGo} onpointerleave={letGo} onpointercancel={letGo} disabled={!on} aria-label={t('salon.tele.haut')}><Icon name="chevron-up" size={30} /></button>
      <button class="left" class:flash={flash === 'CursorLeft'} onclick={() => tap('CursorLeft')} onpointerdown={() => holdSoon('CursorLeft')} onpointerup={letGo} onpointerleave={letGo} onpointercancel={letGo} disabled={!on} aria-label={t('salon.tele.gauche')}><Icon name="chevron-left" size={30} /></button>
      <button class="ok" class:flash={flash === 'Confirm'} onclick={() => key('Confirm')} disabled={!on} aria-label={t('salon.tele.valider')}>OK</button>
      <button class="right" class:flash={flash === 'CursorRight'} onclick={() => tap('CursorRight')} onpointerdown={() => holdSoon('CursorRight')} onpointerup={letGo} onpointerleave={letGo} onpointercancel={letGo} disabled={!on} aria-label={t('salon.tele.droite')}><Icon name="chevron-right" size={30} /></button>
      <button class="down" class:flash={flash === 'CursorDown'} onclick={() => tap('CursorDown')} onpointerdown={() => holdSoon('CursorDown')} onpointerup={letGo} onpointerleave={letGo} onpointercancel={letGo} disabled={!on} aria-label={t('salon.tele.bas')}><Icon name="chevron-down" size={30} /></button>
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
      onpointercancel={cancel}
      oncontextmenu={(e) => e.preventDefault()}>
      <span class="pad-hint">
        <b>OK</b>
        <small>{t('salon.tele.glisser')}<br />{t('salon.tele.toucher')}{#if canHold}<br />{t('salon.tele.garder')}{/if}</small>
      </span>
      {#if flash}<i class="ripple"></i>{/if}
    </div>
  {/if}
    <div
      class="rail"
      class:off={!on}
      class:muted
      class:moving={sliding != null}
      role="slider"
      tabindex={on ? 0 : -1}
      aria-label={t('salon.tele.volume_jauge')}
      aria-orientation="vertical"
      aria-valuemin="0"
      aria-valuemax={maxVolume}
      aria-valuenow={shown ?? 0}
      aria-disabled={!on}
      onpointerdown={railDown}
      onpointermove={railMove}
      onpointerup={railUp}
      onpointercancel={railUp}
      onkeydown={railKey}>
      <i class="level" style:height="{fill * 100}%"></i>
      <b class="num" style:bottom="calc({fill * 100}% - {fill * 30}px)">{shown != null ? num(shown) : '—'}</b>
    </div>
  </div>

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
    <button onclick={() => step(-1)} disabled={!on} aria-label={t('salon.tele.volume_moins')}><Icon name="volume-minus" size={24} /></button>
    <div class="readout">
      <small>{volumeLabel}</small>
      <b class="num">{muted ? t('salon.tele.muet') : volume != null ? num(volume) : '—'}</b>
    </div>
    <button onclick={() => step(1)} disabled={!on} aria-label={t('salon.tele.volume_plus')}><Icon name="volume-plus" size={24} /></button>
    <button class="mute" class:active={muted} onclick={toggleMute} disabled={!on} aria-label={t('salon.couper_son')}><Icon name={muted ? 'volume-off' : 'volume-high'} size={22} /></button>
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

  .middle {
    display: grid;
    grid-template-columns: 1fr 58px;
    gap: 12px;
    align-items: stretch;
  }

  .rail {
    position: relative;
    min-height: 180px;
    margin: 4px 0;
    border-radius: 29px;
    background: var(--surface);
    box-shadow: inset 0 2px 6px color-mix(in srgb, var(--ink) 12%, transparent);
    overflow: hidden;
    touch-action: none;
    user-select: none;
    cursor: ns-resize;
  }

  .rail .level {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    background: linear-gradient(0deg, var(--cool), color-mix(in srgb, var(--cool) 60%, var(--surface)));
    transition: height 0.15s ease-out;
  }

  .rail.moving .level {
    transition: none;
  }

  .rail.muted .level {
    background: var(--ink-3);
    opacity: 0.4;
  }

  .rail .num {
    position: absolute;
    left: 0;
    right: 0;
    text-align: center;
    font-size: 15px;
    font-weight: 750;
    color: var(--ink);
    pointer-events: none;
    padding: 6px 0;
  }

  .rail.off {
    opacity: 0.45;
    cursor: default;
  }

  .rail:focus-visible {
    outline: 2px solid var(--cool);
    outline-offset: 2px;
  }

  .typing {
    width: 46px;
    height: 46px;
    border-radius: 50%;
    margin-left: auto;
    margin-right: 10px;
  }

  .typing.lit {
    background: var(--ink);
    color: var(--bg);
  }

  .write {
    display: grid;
    grid-template-columns: 1fr 50px;
    gap: 8px;
    align-items: center;
  }

  .write input {
    font: inherit;
    font-size: 16px;
    height: 50px;
    padding: 0 16px;
    border-radius: 16px;
    border: 0;
    background: var(--surface);
    color: var(--ink);
    box-shadow: inset 0 2px 6px color-mix(in srgb, var(--ink) 10%, transparent);
    min-width: 0;
  }

  .write input:focus-visible {
    outline: 2px solid var(--cool);
  }

  .write .go {
    height: 50px;
    border-radius: 16px;
  }

  .write small {
    grid-column: 1 / -1;
    font-size: 12px;
    color: var(--ink-3);
  }

  .dpad {
    position: relative;
    width: min(250px, 100%);
    aspect-ratio: 1;
    margin: 4px auto;
    border-radius: 50%;
    background: var(--surface);
    box-shadow: var(--key-shadow), inset 0 0 0 10px var(--surface-2);
  }

  .dpad button {
    position: absolute;
    width: 29.6%;
    height: 29.6%;
    background: none;
    box-shadow: none;
    border-radius: 50%;
    /* Held down: no text selection, no callout menu on a phone. */
    user-select: none;
    -webkit-user-select: none;
    -webkit-touch-callout: none;
  }

  .dpad button:hover:not(:disabled) {
    background: var(--surface-2);
  }

  .dpad .up {
    top: 4%;
    left: 35.2%;
  }

  .dpad .down {
    bottom: 4%;
    left: 35.2%;
  }

  .dpad .left {
    left: 4%;
    top: 35.2%;
  }

  .dpad .right {
    right: 4%;
    top: 35.2%;
  }

  .dpad .ok {
    left: 28%;
    top: 28%;
    width: 44%;
    height: 44%;
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
