<script>
  import { home, hub, confirmHeld, dismissHeld, refreshSession } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** The code, asked when the house wants to be sure it is a person. A new
   *  one is chosen here too, typed twice and kept encrypted by Moli: freely
   *  by an owner (Cloudflare Access proves the e-mail), after the current
   *  code for anyone else. Then the action goes on. */
  let pin = $state('');
  let error = $state('');
  let busy = $state(false);
  // « enter » the code; or « new » then « confirm » a new one, after the
  // « current » one unless an owner chooses.
  let mode = $state(
    home.held?.newCode
      ? hub.session?.owner ? 'new' : 'current'
      : hub.session?.owner && !hub.session?.pin_configured ? 'new' : 'enter',
  );
  let first = '';
  let current = '';
  // The shortest code the house accepts (six digits without an owner).
  const min = $derived(hub.session?.min_pin ?? 4);

  // The guard's reason is the house's own words (French or English): the
  // patterns recognise it, the sentence shown is ours.
  const reason = $derived(
    mode === 'current'
      ? t('commun.code.actuel_d_abord')
      : mode !== 'enter'
      ? mode === 'new'
        ? t('commun.code.choisir', { min })
        : t('commun.code.seconde_fois')
      : /automatisme|automation/i.test(home.held?.reason ?? '')
        ? t('commun.code.raison_automatisme')
        : /calme|quiet/i.test(home.held?.reason ?? '')
          ? t('commun.code.raison_calme')
          : /prot/i.test(home.held?.reason ?? '')
            ? t('commun.code.raison_protegee')
            : t('commun.code.raison_defaut'),
  );

  async function press(digit) {
    if (busy || pin.length >= 8) return;
    error = '';
    pin += digit;
  }

  async function choose(code) {
    const res = await fetch('/api/session/pin', {
      method: 'PUT',
      headers: { 'content-type': 'application/json', 'x-moli-origin': 'ui' },
      body: JSON.stringify(current ? { pin: code, current } : { pin: code }),
    });
    if (!res.ok) return (await res.json().catch(() => ({}))).error ?? t('commun.code.erreur', { status: res.status });
    await refreshSession();
    return null;
  }

  async function submit() {
    if (pin.length < 4 || busy) return;
    if (mode === 'new' && pin.length < min) {
      error = t('commun.code.min_chiffres', { min });
      return;
    }
    if (mode === 'current') {
      current = pin;
      pin = '';
      mode = 'new';
      return;
    }
    if (mode === 'new') {
      first = pin;
      pin = '';
      mode = 'confirm';
      return;
    }
    busy = true;
    if (mode === 'confirm') {
      if (pin !== first) {
        error = t('commun.code.differents');
        mode = 'new';
      } else {
        error = (await choose(pin)) ?? '';
        if (error && current) {
          // A wrong current code: start again from it.
          mode = 'current';
          current = '';
        }
        if (!error) {
          current = '';
          mode = 'enter';
          // The new code opens the session, and what was waiting goes on.
          error = (await confirmHeld(pin)) ?? '';
        }
      }
      first = '';
    } else {
      error = (await confirmHeld(pin)) ?? '';
    }
    busy = false;
    pin = '';
  }

  function onkey(e) {
    if (/^[0-9]$/.test(e.key)) press(e.key);
    else if (e.key === 'Backspace') pin = pin.slice(0, -1);
    else if (e.key === 'Enter') submit();
    else if (e.key === 'Escape') dismissHeld();
  }
</script>

<svelte:window onkeydown={onkey} />

<div class="scrim" role="presentation" onclick={dismissHeld}></div>
<div class="sheet" role="dialog" aria-modal="true" aria-label={t('commun.code.confirmer')}>
  <span class="lock"><Icon name="lock" size={26} /></span>
  <h2>{mode === 'enter' ? (home.held?.label ?? t('commun.code.cette_action')) : mode === 'current' ? t('commun.code.actuel') : t('commun.code.nouveau')}</h2>
  <p class="why">{reason}</p>
  <div class="dots" aria-label={t('commun.code.saisis', { count: pin.length })}>
    {#each Array(Math.max(mode === 'enter' || mode === 'current' ? 6 : 4, pin.length)) as _, i (i)}
      <i class:filled={i < pin.length}></i>
    {/each}
  </div>
  <p class="error" role="alert">{error}</p>
  <div class="pad">
    {#each ['1', '2', '3', '4', '5', '6', '7', '8', '9'] as d (d)}
      <button onclick={() => press(d)}>{d}</button>
    {/each}
    <button class="soft" onclick={() => (pin = pin.slice(0, -1))} aria-label={t('commun.code.effacer')}>⌫</button>
    <button onclick={() => press('0')}>0</button>
    <button class="ok" onclick={submit} disabled={pin.length < 4 || busy} aria-label={t('commun.valider')}>OK</button>
  </div>
  {#if hub.session?.owner && mode === 'enter'}
    <button class="link" onclick={() => ((mode = 'new'), (pin = ''), (error = ''))}>{t('commun.code.nouveau_choisir')}</button>
  {/if}
  <button class="cancel" onclick={dismissHeld}>{t('commun.annuler')}</button>
</div>
<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: rgb(10 14 22 / 45%);
    backdrop-filter: blur(4px);
    z-index: 50;
  }

  .sheet {
    position: fixed;
    left: 50%;
    top: 50%;
    transform: translate(-50%, -50%);
    z-index: 51;
    width: min(380px, calc(100vw - 32px));
    background: var(--surface);
    border-radius: var(--r-xl);
    box-shadow: var(--shadow-lift);
    padding: 28px 24px 18px;
    display: grid;
    justify-items: center;
    text-align: center;
    gap: 8px;
  }

  .lock {
    width: 56px;
    height: 56px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--cool-soft);
    color: var(--cool);
  }

  h2 {
    font-size: 20px;
    font-weight: 700;
    margin-top: 6px;
  }

  .why {
    color: var(--ink-2);
    font-size: 14px;
    max-width: 280px;
  }

  .dots {
    display: flex;
    gap: 10px;
    margin: 14px 0 0;
  }

  .dots i {
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: var(--surface-3);
    transition: background 0.15s;
  }

  .dots i.filled {
    background: var(--ink);
  }

  .error {
    min-height: 20px;
    color: var(--alert);
    font-size: 14px;
    font-weight: 600;
  }

  .pad {
    display: grid;
    grid-template-columns: repeat(3, 72px);
    gap: 12px;
  }

  .pad button {
    height: 64px;
    border: 0;
    border-radius: 20px;
    background: var(--surface-2);
    font-size: 24px;
    font-weight: 600;
    transition: transform 0.08s, background 0.15s;
  }

  .pad button:active {
    transform: scale(0.95);
    background: var(--surface-3);
  }

  .pad .soft {
    background: none;
    color: var(--ink-3);
  }

  .pad .ok {
    background: var(--ink);
    color: var(--bg);
    font-size: 18px;
  }

  .pad .ok:disabled {
    opacity: 0.35;
  }

  .link {
    margin-top: 10px;
    border: 0;
    background: none;
    color: var(--cool);
    font-weight: 650;
    padding: 6px 12px;
  }

  .cancel {
    margin-top: 8px;
    border: 0;
    background: none;
    color: var(--ink-3);
    font-weight: 600;
    padding: 8px 16px;
  }
</style>
