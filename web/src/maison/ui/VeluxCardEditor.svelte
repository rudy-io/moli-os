<script>
  // One VELUX automation, edited or created: its name, when (a time of day,
  // or minutes around sunrise or sunset), an airing that ends (then all is
  // closed), its days, and where each window or blind goes. A setting is
  // copied to the other windows (or blinds) in one tap; nothing leaves
  // before « Save ».
  import { onMount } from 'svelte';
  import { dayName } from '../lib/program.js';
  import { WEEK, WEEKDAYS, WEEKEND, SUN_SPAN, hhmm, fromHhmm } from '../lib/velux-plans.js';
  import { t, locale } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  let { card, covers, openness, when, isNew, onsave, ondelete, onduplicate, onclose } = $props();

  let dialog;
  // Opened on the dialog itself: no field (a phone's keyboard), no ring.
  onMount(() => {
    dialog.showModal();
    dialog.focus();
  });

  // The draft (a copy: nothing changes until saved).
  const start = card ?? { name: '', days: [...WEEK], ref: 'time', min: 8 * 60, set: {} };
  let name = $state(start.name);
  let days = $state([...start.days]);
  let ref = $state(start.ref);
  let min = $state(start.min);
  let end = $state(start.end ?? null);
  let set = $state({ ...start.set });
  let off = $state(!!start.off);
  let confirming = $state(false);

  const sun = $derived(ref !== 'time');
  // From a time to the sun and back: the sun's minutes are an offset.
  function pickRef(next) {
    if ((next === 'time') !== (ref === 'time')) {
      min = next === 'time' ? 8 * 60 : 0;
      end = end == null ? null : next === 'time' ? min + 30 : 30;
    }
    ref = next;
  }
  const SUN_STEPS = Array.from({ length: (2 * SUN_SPAN) / 5 + 1 }, (_, i) => i * 5 - SUN_SPAN);

  const toggleDay = (d) => (days = days.includes(d) ? days.filter((x) => x !== d) : [...days, d].sort((a, b) => a - b));
  const preset = (list) => (days = [...list]);
  const isPreset = (list) => days.length === list.length && list.every((d) => days.includes(d));

  // A window from 5 to 15 % airs at 10 %, as on its tile.
  const settle = (cover, pct) => (cover.window && pct > 0 && pct <= 15 ? 10 : pct);
  const include = (cover, yes) => {
    const next = { ...set };
    if (yes) next[cover.id] = cover.window ? 100 : 0;
    else delete next[cover.id];
    set = next;
  };
  const place = (cover, pct) => (set = { ...set, [cover.id]: settle(cover, pct) });
  // This setting for every other window (or blind): the same kind only.
  const others = (cover) => covers.filter((c) => c.id !== cover.id && c.window === cover.window);
  function spread(cover) {
    const next = { ...set };
    for (const c of others(cover)) next[c.id] = set[cover.id];
    set = next;
  }

  const windows = $derived(covers.filter((c) => c.window));
  const blinds = $derived(covers.filter((c) => !c.window));
  const anyWindow = $derived(Object.keys(set).some((id) => covers.find((c) => c.id === id)?.window));

  const problem = $derived(
    !name.trim()
      ? t('maison.fenetres.auto.manque_nom')
      : !days.length
        ? t('maison.fenetres.auto.manque_jour')
        : !Object.keys(set).length
          ? t('maison.fenetres.auto.manque_appareil')
          : end != null && end <= min
            ? t('maison.fenetres.auto.fin_avant')
            : null,
  );

  function draft() {
    const out = { name: name.trim(), days: [...days], ref, min, set: { ...set } };
    if (card?.id != null) out.id = card.id;
    if (end != null) out.end = end;
    if (off) out.off = true;
    return out;
  }
  const save = () => !problem && onsave(draft());
</script>

<dialog bind:this={dialog} onclose={onclose} aria-labelledby="velux-card-title" tabindex="-1">
  <header>
    <div>
      <h2 id="velux-card-title">{isNew ? t('maison.fenetres.auto.nouveau') : t('maison.fenetres.auto.modifier_titre')}</h2>
      <p class="sub">{when(ref, min)}{end != null ? ` · ${t('maison.fenetres.auto.jusqu', { when: when(ref, end, { inner: true }) })}` : ''}</p>
    </div>
    <button class="round" onclick={() => dialog.close()} aria-label={t('commun.fermer')}><Icon name="close" size={18} /></button>
  </header>

  <label class="field">
    <span>{t('maison.fenetres.auto.nom')}</span>
    <input bind:value={name} maxlength="60" placeholder={t('maison.fenetres.auto.nom_exemple')} />
  </label>

  <fieldset>
    <legend>{t('maison.fenetres.auto.quand')}</legend>
    <div class="seg" role="radiogroup">
      {#each [['time', 'clock', 'maison.fenetres.auto.heure'], ['sunrise', 'sunrise', 'maison.fenetres.auto.lever'], ['sunset', 'sunset', 'maison.fenetres.auto.coucher']] as [key, icon, label] (key)}
        <button role="radio" aria-checked={ref === key} class:active={ref === key} onclick={() => pickRef(key)}><Icon name={icon} size={15} />{t(label)}</button>
      {/each}
    </div>
    {#if sun}
      <label class="field inline">
        <span>{t('maison.fenetres.auto.decalage')}</span>
        <select value={min} onchange={(e) => (min = Number(e.currentTarget.value))}>
          {#each SUN_STEPS as m (m)}<option value={m}>{when(ref, m, { short: true })}</option>{/each}
        </select>
      </label>
    {:else}
      <label class="field inline">
        <span>{t('maison.fenetres.auto.a')}</span>
        <input type="time" step="300" value={hhmm(min)} onchange={(e) => (min = fromHhmm(e.currentTarget.value) ?? min)} />
      </label>
    {/if}
    {#if anyWindow || end != null}
      <label class="check">
        <input type="checkbox" checked={end != null} onchange={(e) => (end = e.currentTarget.checked ? (sun ? Math.min(SUN_SPAN, min + 30) : Math.min(24 * 60 - 5, min + 30)) : null)} />
        <span>{t('maison.fenetres.auto.puis_fermer')}</span>
      </label>
      {#if end != null}
        <label class="field inline">
          <span>{t('maison.fenetres.auto.fin')}</span>
          {#if sun}
            <select value={end} onchange={(e) => (end = Number(e.currentTarget.value))}>
              {#each SUN_STEPS.filter((m) => m > min) as m (m)}<option value={m}>{when(ref, m, { short: true })}</option>{/each}
            </select>
          {:else}
            <input type="time" step="300" value={hhmm(end)} onchange={(e) => (end = fromHhmm(e.currentTarget.value) ?? end)} />
          {/if}
        </label>
      {/if}
    {/if}
  </fieldset>

  <fieldset>
    <legend>{t('maison.fenetres.auto.jours')}</legend>
    <div class="chips">
      {#each WEEK as d (d)}
        <button class="d" class:active={days.includes(d)} aria-pressed={days.includes(d)} onclick={() => toggleDay(d)} title={dayName(d, locale(), 'long')}>{dayName(d, locale(), 'short')}</button>
      {/each}
    </div>
    <div class="chips small">
      <button class:active={isPreset(WEEK)} onclick={() => preset(WEEK)}>{t('maison.fenetres.auto.tous_les_jours')}</button>
      <button class:active={isPreset(WEEKDAYS)} onclick={() => preset(WEEKDAYS)}>{t('maison.fenetres.auto.en_semaine')}</button>
      <button class:active={isPreset(WEEKEND)} onclick={() => preset(WEEKEND)}>{t('maison.fenetres.auto.week_end')}</button>
    </div>
  </fieldset>

  {#each [[windows, 'maison.fenetres.auto.fenetres'], [blinds, 'maison.fenetres.auto.stores']] as [list, title] (title)}
    {#if list.length}
      <fieldset>
        <legend>{t(title)}</legend>
        <ul class="covers">
          {#each list as cover (cover.id)}
            {@const pct = set[cover.id]}
            <li class:in={pct != null}>
              <label class="check">
                <input type="checkbox" checked={pct != null} onchange={(e) => include(cover, e.currentTarget.checked)} />
                <Icon name={cover.window ? 'roof-window' : 'shutter'} size={16} />
                <span class="name">{cover.name}</span>
                {#if pct != null}<b>{openness(cover.id, pct)}</b>{/if}
              </label>
              {#if pct != null}
                <div class="level">
                  <!-- A blind runs the other way, as on its tile: rolled up on the left. -->
                  <input
                    type="range"
                    min="0"
                    max="100"
                    step="5"
                    value={cover.blind ? 100 - pct : pct}
                    oninput={(e) => place(cover, cover.blind ? 100 - Number(e.currentTarget.value) : Number(e.currentTarget.value))}
                    aria-label={t('commun.volet.niveau', { label: cover.name })}
                    aria-valuetext={openness(cover.id, pct)} />
                  <div class="ends" aria-hidden="true">
                    <span>{t(cover.blind ? 'commun.volet.ouvert' : 'commun.volet.ferme')}</span><span>{t(cover.blind ? 'commun.volet.ferme' : 'commun.volet.ouvert')}</span>
                  </div>
                  {#if others(cover).length}
                    <button class="link" onclick={() => spread(cover)}><Icon name="repeat" size={14} />{t(cover.window ? 'maison.fenetres.auto.copier_fenetres' : 'maison.fenetres.auto.copier_stores')}</button>
                  {/if}
                </div>
              {/if}
            </li>
          {/each}
        </ul>
      </fieldset>
    {/if}
  {/each}

  <label class="check">
    <input type="checkbox" checked={off} onchange={(e) => (off = e.currentTarget.checked)} />
    <span>{t('maison.fenetres.auto.en_pause')}</span>
  </label>

  {#if problem}<p class="problem">{problem}</p>{/if}

  <footer>
    {#if !isNew}
      {#if confirming}
        <button class="danger" onclick={ondelete}>{t('maison.fenetres.auto.supprimer_oui')}</button>
      {:else}
        <button class="ghost" onclick={() => (confirming = true)}><Icon name="trash" size={15} />{t('maison.fenetres.auto.supprimer')}</button>
      {/if}
      <button class="ghost" disabled={!!problem} onclick={() => onduplicate(draft())}>{t('maison.fenetres.auto.dupliquer')}</button>
    {/if}
    <span class="push"></span>
    <button class="ghost" onclick={() => dialog.close()}>{t('commun.annuler')}</button>
    <button class="primary" disabled={!!problem} onclick={save}>{t('maison.fenetres.auto.enregistrer')}</button>
  </footer>
</dialog>

<style>
  dialog {
    border: 0;
    padding: 18px;
    width: min(620px, 96vw);
    max-height: 94dvh;
    overflow-y: auto;
    border-radius: var(--r-xl);
    background: var(--surface);
    color: var(--ink);
    box-shadow: 0 30px 80px rgb(0 0 0 / 40%);
  }

  dialog:focus {
    outline: none;
  }

  dialog::backdrop {
    background: rgb(4 6 12 / 60%);
    backdrop-filter: blur(4px);
  }

  header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 14px;
  }

  h2 {
    margin: 0;
    font-size: 21px;
    font-weight: 750;
  }

  .sub {
    margin: 2px 0 0;
    font-size: 13px;
    color: var(--ink-3);
    font-weight: 600;
  }

  button {
    font: inherit;
    cursor: pointer;
  }

  .round {
    width: 40px;
    height: 40px;
    flex: none;
    border: 0;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--surface-2);
    color: var(--ink);
  }

  fieldset {
    border: 0;
    margin: 0 0 16px;
    padding: 0;
    display: grid;
    gap: 10px;
  }

  legend {
    padding: 0;
    margin-bottom: 8px;
    font-size: 12.5px;
    font-weight: 750;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--ink-3);
  }

  .field {
    display: grid;
    gap: 6px;
    margin-bottom: 16px;
    font-size: 13px;
    font-weight: 650;
    color: var(--ink-2);
  }

  .field.inline {
    grid-template-columns: auto 1fr;
    align-items: center;
    gap: 12px;
    margin: 0;
  }

  input:not([type]),
  input[type='time'],
  select {
    font: inherit;
    font-size: 15px;
    padding: 9px 12px;
    border: 1px solid var(--line);
    border-radius: 12px;
    background: var(--surface-2);
    color: var(--ink);
    min-width: 0;
  }

  .seg,
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .seg button,
  .chips button {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    border: 0;
    border-radius: 999px;
    padding: 7px 12px;
    background: var(--surface-2);
    color: var(--ink-2);
    font-size: 13.5px;
    font-weight: 650;
  }

  .d {
    text-transform: capitalize;
  }

  .chips.small button {
    font-size: 12.5px;
    padding: 5px 10px;
  }

  .seg button.active,
  .chips button.active {
    background: var(--cool);
    color: #fff;
  }

  .check {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 14px;
    color: var(--ink-2);
    cursor: pointer;
  }

  .check input {
    width: 18px;
    height: 18px;
    accent-color: var(--cool);
  }

  .covers {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 6px;
  }

  .covers li {
    display: grid;
    gap: 8px;
    padding: 10px 12px;
    border-radius: var(--r-md);
    background: var(--surface-2);
  }

  .covers .name {
    flex: 1;
    min-width: 0;
    overflow-wrap: anywhere;
  }

  .covers b {
    flex: none;
    font-size: 13px;
    font-weight: 650;
    color: var(--cool);
  }

  .level {
    display: grid;
    gap: 4px;
  }

  .level input {
    width: 100%;
    accent-color: var(--cool);
  }

  .ends {
    display: flex;
    justify-content: space-between;
    font-size: 11px;
    color: var(--ink-3);
  }

  .link {
    justify-self: start;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    border: 0;
    padding: 4px 0;
    background: none;
    color: var(--cool);
    font-size: 13px;
    font-weight: 650;
  }

  .problem {
    margin: 12px 0 0;
    font-size: 13px;
    color: var(--warm-ink);
  }

  footer {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-top: 16px;
  }

  .push {
    flex: 1;
  }

  .primary,
  .ghost,
  .danger {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    border: 0;
    border-radius: 999px;
    padding: 9px 16px;
    font-size: 14px;
    font-weight: 650;
  }

  .primary {
    background: var(--ink);
    color: var(--surface);
  }

  .ghost {
    background: var(--surface-2);
    color: var(--ink-2);
  }

  .danger {
    background: #c0392b;
    color: #fff;
  }

  .primary:disabled,
  .ghost:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
