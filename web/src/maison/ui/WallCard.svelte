<script>
  import { onMount } from 'svelte';
  import { home } from '../lib/home.svelte.js';
  import { wallAvailable, wallAsk, wallSend, onWallState, hold, sleepKind, IDLE_CHOICES } from '../lib/native-wall.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** This tablet as a wall tablet (Moli app on Android, lib/native-wall.js):
   *  Moli alone on screen, woken by someone passing in front of the camera,
   *  asleep when nobody is there. Shown only where it is possible. */
  const available = wallAvailable();
  let state = $state(null);
  let busy = $state(false);

  onMount(() => {
    if (!available) return;
    const off = onWallState((s) => (state = s));
    wallAsk('state').then((s) => (state = s));
    return off;
  });

  /** A change of settings. Turning the wall tablet off lifts the kiosk and
   *  the home screen it imposes, for good: behind the house's code, like
   *  `release()`. */
  function configure(change) {
    if (change.enabled === false && state?.enabled) {
      home.held = {
        label: t('systeme.tablette.desactiver'),
        reason: 'code',
        custom: () => apply(change),
        onCancel: () => {},
      };
      return;
    }
    apply(change);
  }

  async function apply(change) {
    if (!state || busy) return;
    busy = true;
    const next = await wallAsk('configure', {
      options: { enabled: state.enabled, camera: state.camera, sensitivity: state.sensitivity, idle: state.idle, ...change },
    });
    if (next) state = next;
    busy = false;
  }

  /** Upkeep (Settings, another app): Moli leaves the front until it opens
   *  again. Behind the house's code, like any change a passer-by must not make. */
  function release() {
    home.held = {
      label: t('systeme.tablette.sortir'),
      reason: 'code',
      custom: async () => {
        const next = await wallAsk('release');
        if (next) state = next;
      },
      onCancel: () => {},
    };
  }

  const held = $derived(hold(state));
  const sleeps = $derived(sleepKind(state));
  const idleWord = (m) => (m === 0 ? t('systeme.tablette.jamais') : t('systeme.tablette.minutes', { n: m }));
</script>

{#if available && state}
  <section class="card wall" aria-label={t('systeme.tablette.titre')}>
    <div class="head">
      <span class="icon"><Icon name="tablet" size={20} /></span>
      <div class="what">
        <b>{t('systeme.tablette.titre')}</b>
        <span class="muted">{t('systeme.tablette.aide')}</span>
      </div>
      <button
        class="switch"
        role="switch"
        aria-checked={state.enabled}
        aria-label={t('systeme.tablette.activer')}
        disabled={busy}
        onclick={() => configure({ enabled: !state.enabled })}
      ><span class="knob"></span></button>
    </div>

    {#if state.enabled}
      <div class="rows">
        <div class="row">
          <span>
            <b>{t('systeme.tablette.camera')}</b>
            <small class="muted">{state.camera && !state.cameraAllowed ? t('systeme.tablette.camera_refusee') : t('systeme.tablette.camera_aide')}</small>
          </span>
          <button
            class="switch"
            role="switch"
            aria-checked={state.camera && state.cameraAllowed}
            aria-label={t('systeme.tablette.camera')}
            disabled={busy}
            onclick={() => configure({ camera: !(state.camera && state.cameraAllowed) })}
          ><span class="knob"></span></button>
        </div>

        {#if state.camera && state.cameraAllowed}
          <label class="row">
            <span>
              <b>{t('systeme.tablette.sensibilite')}</b>
              <small class="muted">{t(`systeme.tablette.sensibilite_${state.sensitivity}`)}</small>
            </span>
            <input
              type="range"
              min="1"
              max="5"
              step="1"
              value={state.sensitivity}
              disabled={busy}
              onchange={(e) => configure({ sensitivity: Number(e.currentTarget.value) })}
            />
          </label>
        {/if}

        <div class="row">
          <span>
            <b>{t('systeme.tablette.veille')}</b>
            <small class="muted">{sleeps === 'off' ? t('systeme.tablette.veille_eteint') : t('systeme.tablette.veille_noir')}</small>
          </span>
          <div class="choices" role="radiogroup" aria-label={t('systeme.tablette.veille')}>
            {#each IDLE_CHOICES as m (m)}
              <button role="radio" aria-checked={state.idle === m} class:on={state.idle === m} disabled={busy} onclick={() => configure({ idle: m })}>{idleWord(m)}</button>
            {/each}
          </div>
        </div>
      </div>

      <ul class="steps">
        <li class:ok={sleeps === 'off'}>
          <Icon name={sleeps === 'off' ? 'ok' : 'warning'} size={16} />
          <span>
            {sleeps === 'off' ? t('systeme.tablette.admin_ok') : t('systeme.tablette.admin_non')}
          </span>
          {#if sleeps !== 'off'}<button class="link" onclick={() => wallSend('admin')}>{t('systeme.tablette.autoriser')}</button>{/if}
        </li>
        <li class:ok={held !== 'free'}>
          <Icon name={held === 'free' ? 'warning' : 'ok'} size={16} />
          <span>
            {held === 'owner' ? t('systeme.tablette.tenue_proprietaire') : held === 'pinned' ? t('systeme.tablette.tenue_epinglee') : t('systeme.tablette.tenue_libre')}
          </span>
          {#if held === 'free'}<button class="link" onclick={() => wallSend('pin', { on: true })}>{t('systeme.tablette.epingler')}</button>{/if}
        </li>
      </ul>

      {#if held === 'owner'}
        <button class="ghost" onclick={release}><Icon name="lock-open" size={16} />{t('systeme.tablette.sortir')}</button>
      {/if}
    {/if}
  </section>
{/if}

<style>
  .wall {
    display: grid;
    gap: 14px;
  }

  .head {
    display: flex;
    align-items: center;
    gap: 12px 16px;
  }

  .icon {
    display: grid;
    flex: none;
    place-items: center;
    width: 42px;
    height: 42px;
    border-radius: 50%;
    background: var(--cool-soft);
    color: var(--cool);
  }

  .what {
    display: grid;
    flex: 1;
    min-width: 0;
    gap: 2px;
    font-size: 14px;
  }

  .rows {
    display: grid;
    gap: 12px;
  }

  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 8px 16px;
    font-size: 14px;
  }

  .row > span {
    display: grid;
    gap: 2px;
    flex: 1;
    min-width: 200px;
  }

  .row small {
    font-size: 12.5px;
  }

  input[type='range'] {
    width: min(220px, 100%);
    accent-color: var(--cool);
  }

  .choices {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .choices button {
    min-height: 36px;
    padding: 0 12px;
    border: 1px solid var(--line);
    border-radius: 999px;
    background: var(--surface);
    color: var(--ink);
    font: inherit;
    font-size: 13px;
    font-weight: 600;
  }

  .choices button.on {
    border-color: var(--cool);
    background: var(--cool-soft);
    color: var(--cool);
  }

  .steps {
    display: grid;
    gap: 8px;
    margin: 0;
    padding: 12px 0 0;
    border-top: 1px solid var(--line);
    list-style: none;
    font-size: 13.5px;
  }

  .steps li {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    color: var(--warm-ink);
  }

  .steps li.ok {
    color: var(--good);
  }

  .steps li span {
    flex: 1;
    min-width: 180px;
    color: var(--ink);
  }

  .link {
    border: 0;
    background: none;
    color: var(--cool);
    font: inherit;
    font-weight: 700;
    cursor: pointer;
  }

  .ghost {
    justify-self: start;
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-height: 40px;
    padding: 0 16px;
    border: 1px solid var(--line);
    border-radius: 999px;
    background: var(--surface);
    color: var(--ink);
    font: inherit;
    font-size: 14px;
    font-weight: 600;
  }

  .switch {
    width: 52px;
    height: 32px;
    flex: none;
    border: 0;
    border-radius: 999px;
    background: var(--surface-3);
    padding: 3px;
    display: flex;
    transition: background 0.25s var(--ease);
  }

  .switch .knob {
    width: 26px;
    height: 26px;
    border-radius: 50%;
    background: var(--surface);
    box-shadow: 0 1px 3px rgb(0 0 0 / 25%);
    transition: transform 0.25s var(--ease);
  }

  .switch[aria-checked='true'] {
    background: var(--cool);
  }

  .switch[aria-checked='true'] .knob {
    transform: translateX(20px);
  }
</style>
