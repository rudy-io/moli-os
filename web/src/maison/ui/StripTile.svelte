<script>
  import { setPointLabel } from '../../lib/hub.svelte.js';
  import { device, isOn, pending, reachable, act, nameOf, outletsOf, unpowered, note } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import { canWrite } from '../lib/capabilities.js';
  import Icon from './Icon.svelte';

  /** A power strip, drawn: one socket per outlet, lit when it is on, a tap
   *  switches that one. The switch in the head switches them all (all on →
   *  all off; otherwise everything on), one outlet after the other: the
   *  device's own « every outlet » channel is never sent (on a Meross it
   *  reads on while outlets are off). A USB outlet (its name says so) is
   *  drawn as ports. `icons`: no names under the sockets (in their tooltip).
   *  The pencil names the outlets (kept in the device's label, `points`);
   *  meanwhile the sockets take no order. An empty name gives back the
   *  outlet's own. */
  let { id, icons = false } = $props();

  const d = $derived(device(id));
  const label = $derived(nameOf(id));
  const outlets = $derived(outletsOf(id));
  const live = $derived(reachable(id));
  const cut = $derived(!!d && unpowered(id));
  const lit = $derived(outlets.filter((o) => isOn(id, o.key)).length);
  const writable = $derived(outlets.filter((o) => canWrite(d, o.key)));
  const allOn = $derived(lit === outlets.length);

  const state = $derived(
    cut
      ? t('commun.piece.coupee')
      : !reachable(id)
        ? t('commun.piece.injoignable')
        : lit === 0
          ? t('commun.multiprise.tout_eteint')
          : allOn
            ? t('commun.multiprise.tout_allume')
            : t('commun.multiprise.allumees', { on: lit, count: outlets.length }),
  );

  const usb = (o) => /\busb\b/i.test(o.name);
  const said = (o) => t(isOn(id, o.key) ? 'commun.piece.allumee' : 'commun.piece.eteinte');

  function one(o) {
    if (!live || !canWrite(d, o.key)) return;
    if (cut) return note(t('commun.lampe.coupee_note', { label }));
    act(`${id}/${o.key}`, !isOn(id, o.key), `${label} · ${o.name}`);
  }

  let naming = $state(false);
  let saving = $state(null);

  async function rename(o, input) {
    const name = input.value.trim();
    // Its own name typed back: the name it has without one.
    const next = name === o.fallback ? '' : name;
    if ((next || o.fallback) === o.name) return;
    saving = o.key;
    if (!(await setPointLabel(id, o.key, next))) input.value = o.name;
    saving = null;
  }

  function keys(e, o) {
    if (e.key === 'Enter') e.currentTarget.blur();
    if (e.key === 'Escape') {
      e.currentTarget.value = o.name;
      e.currentTarget.blur();
    }
  }

  function all() {
    if (!live || !writable.length) return;
    if (cut) return note(t('commun.lampe.coupee_note', { label }));
    const on = !allOn;
    for (const o of writable) if (isOn(id, o.key) !== on) act(`${id}/${o.key}`, on, `${label} · ${o.name}`);
  }
</script>

<div class="tile" class:on={lit > 0} class:off-grid={!reachable(id)} class:icons>
  <div class="head">
    <span class="ico"><Icon name="socket" size={20} /></span>
    <span class="text">
      <b>{label}</b>
      <small>{state}</small>
    </span>
    {#if d}
      <button
        class="round"
        class:open={naming}
        onclick={() => (naming = !naming)}
        aria-expanded={naming}
        title={naming ? t('commun.multiprise.noms_finis') : t('commun.multiprise.nommer')}
        aria-label={t('commun.action', { label, action: naming ? t('commun.multiprise.noms_finis') : t('commun.multiprise.nommer') })}>
        <Icon name={naming ? 'ok' : 'pencil'} size={17} />
      </button>
    {/if}
    <button
      class="switch"
      class:on={allOn && reachable(id)}
      disabled={!live || !writable.length}
      onclick={all}
      aria-pressed={allOn && reachable(id)}
      title={allOn ? t('commun.multiprise.tout_eteindre') : t('commun.multiprise.tout_allumer')}
      aria-label={t('commun.action', { label, action: allOn ? t('commun.multiprise.tout_eteindre') : t('commun.multiprise.tout_allumer') })}><i></i></button>
  </div>

  <div class="strip" role="group" aria-label={label} style:--n={outlets.length}>
    {#each outlets as o (o.key)}
      {@const on = isOn(id, o.key)}
      <button
        class="outlet"
        class:on
        class:busy={pending(id, o.key)}
        disabled={naming || !live || !canWrite(d, o.key)}
        onclick={() => one(o)}
        aria-pressed={on}
        title={`${o.name} · ${said(o)}`}
        aria-label={t('commun.action', { label: `${label} · ${o.name}`, action: said(o) })}>
        <span class="led" aria-hidden="true"></span>
        <svg class="face" viewBox="0 0 48 48" aria-hidden="true">
          {#if usb(o)}
            <rect class="plate" x="4" y="4" width="40" height="40" rx="12" />
            <rect class="port" x="14" y="13" width="20" height="7" rx="1.5" />
            <rect class="port" x="14" y="28" width="20" height="7" rx="1.5" />
            <rect class="tongue" x="17" y="15" width="14" height="2.4" rx="1" />
            <rect class="tongue" x="17" y="30" width="14" height="2.4" rx="1" />
          {:else}
            <rect class="plate" x="4" y="4" width="40" height="40" rx="12" />
            <circle class="well" cx="24" cy="24" r="14" />
            <rect class="earth" x="22" y="10" width="4" height="5" rx="1.5" />
            <circle class="hole" cx="18" cy="25" r="2.6" />
            <circle class="hole" cx="30" cy="25" r="2.6" />
          {/if}
        </svg>
        {#if !icons}<small class="name">{o.name}</small>{/if}
      </button>
    {/each}
  </div>
  {#if naming}
    <div class="names">
      {#each outlets as o, i (o.key)}
        <label class:saving={saving === o.key}>
          <span class="num">{i + 1}</span>
          <input
            type="text"
            maxlength="60"
            value={o.name}
            placeholder={o.fallback}
            autocomplete="off"
            onchange={(e) => rename(o, e.currentTarget)}
            onkeydown={(e) => keys(e, o)}
            aria-label={t('commun.multiprise.nom_de', { n: i + 1 })} />
        </label>
      {/each}
      <small class="hint">{t('commun.multiprise.nom_aide')}</small>
    </div>
  {/if}
</div>

<style>
  .tile {
    border-radius: var(--r-md);
    background: var(--surface-2);
    padding: 14px 14px 12px;
    display: grid;
    gap: 12px;
    transition: background 0.35s var(--ease), box-shadow 0.35s var(--ease);
  }

  .tile.on {
    background: linear-gradient(155deg, var(--warm-soft) 0%, var(--surface-2) 75%);
    box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--warm) 22%, transparent);
  }

  .off-grid {
    opacity: 0.55;
  }

  .head {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .ico {
    flex: none;
    width: 34px;
    height: 34px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--surface);
    color: var(--ink-3);
    transition: all 0.35s var(--ease);
  }

  .on .ico {
    background: var(--warm);
    color: var(--warm-deep);
  }

  .text {
    flex: 1;
    min-width: 0;
    display: grid;
    gap: 1px;
  }

  .text b {
    font-weight: 650;
    font-size: 15px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .text small {
    color: var(--ink-3);
    font-size: 13px;
    font-weight: 550;
  }

  .on .text small {
    color: var(--warm-ink);
  }

  .round {
    all: unset;
    flex: none;
    width: 34px;
    height: 34px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--surface);
    color: var(--ink-3);
    cursor: pointer;
    transition: all 0.25s var(--ease);
  }

  .round.open {
    background: var(--warm);
    color: var(--warm-deep);
  }

  .round:focus-visible,
  .names input:focus-visible {
    outline: 3px solid var(--cool);
    outline-offset: 2px;
  }

  /* Naming: one line per outlet, its number then its name. */
  .names {
    display: grid;
    gap: 8px;
  }

  .names label {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .names .num {
    flex: none;
    width: 26px;
    height: 26px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--surface);
    color: var(--ink-3);
    font-size: 13px;
    font-weight: 700;
  }

  .names input {
    flex: 1;
    min-width: 0;
    box-sizing: border-box;
    padding: 9px 12px;
    border: none;
    border-radius: 12px;
    background: var(--surface);
    color: var(--ink);
    font: inherit;
    font-size: 15px;
  }

  .names .saving input {
    opacity: 0.6;
  }

  .hint {
    color: var(--ink-3);
    font-size: 12px;
  }

  /* The rocker: every outlet at once. */
  .switch {
    all: unset;
    flex: none;
    position: relative;
    width: 40px;
    height: 24px;
    border-radius: 999px;
    background: var(--surface-3);
    cursor: pointer;
    transition: background 0.3s var(--ease);
  }

  .switch i {
    position: absolute;
    top: 3px;
    left: 3px;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: var(--surface);
    box-shadow: 0 1px 3px rgb(0 0 0 / 20%);
    transition: transform 0.3s var(--ease);
  }

  .switch.on {
    background: var(--warm);
  }

  .switch.on i {
    transform: translateX(16px);
  }

  .switch:disabled {
    cursor: default;
  }

  .switch:focus-visible,
  .outlet:focus-visible {
    outline: 3px solid var(--cool);
    outline-offset: 2px;
  }

  /* The strip's body: a long rounded bar, its sockets in a row. */
  .strip {
    display: grid;
    grid-template-columns: repeat(var(--n), minmax(0, 1fr));
    gap: 6px;
    padding: 10px 8px 8px;
    border-radius: 18px;
    background: var(--surface);
    box-shadow:
      inset 0 -2px 0 rgb(0 0 0 / 6%),
      0 1px 2px rgb(0 0 0 / 8%);
  }

  .outlet {
    all: unset;
    box-sizing: border-box;
    min-width: 0;
    display: grid;
    justify-items: center;
    gap: 4px;
    padding: 2px 0 2px;
    border-radius: 12px;
    cursor: pointer;
    -webkit-tap-highlight-color: transparent;
  }

  .outlet:disabled {
    cursor: default;
  }

  /* A small light above each socket, as on the real strip. */
  .led {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--surface-3);
    transition: all 0.3s var(--ease);
  }

  .outlet.on .led {
    background: var(--warm);
    box-shadow: 0 0 6px 1px var(--warm-glow);
  }

  .face {
    width: min(100%, 52px);
    aspect-ratio: 1;
    transition: transform 0.2s var(--ease), filter 0.35s var(--ease);
  }

  .outlet:not(:disabled):active .face {
    transform: scale(0.94);
  }

  .plate {
    fill: var(--surface-2);
    stroke: color-mix(in srgb, var(--ink) 10%, transparent);
    stroke-width: 1;
    transition: fill 0.35s var(--ease);
  }

  .well {
    fill: var(--surface-3);
    transition: fill 0.35s var(--ease);
  }

  .hole,
  .port {
    fill: color-mix(in srgb, var(--ink) 55%, transparent);
  }

  .earth {
    fill: color-mix(in srgb, var(--ink) 30%, var(--surface-3));
  }

  .tongue {
    fill: var(--surface-3);
  }

  .outlet.on .plate {
    fill: var(--warm-soft);
    stroke: color-mix(in srgb, var(--warm) 55%, transparent);
  }

  .outlet.on .well {
    fill: var(--warm);
  }

  .outlet.on .hole,
  .outlet.on .port {
    fill: var(--warm-deep);
  }

  .outlet.on .tongue {
    fill: var(--warm);
  }

  .outlet.on .face {
    filter: drop-shadow(0 3px 8px var(--warm-glow));
  }

  .outlet.busy .face {
    animation: pulse 1s ease-in-out infinite;
  }

  @keyframes pulse {
    50% {
      opacity: 0.5;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .outlet.busy .face {
      animation: none;
    }
  }

  .name {
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 12px;
    font-weight: 600;
    color: var(--ink-3);
  }

  .outlet.on .name {
    color: var(--warm-ink);
  }
</style>
