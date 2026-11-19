<script>
  import { hub, act, nameOf } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import { untrack } from 'svelte';
  import Icon from './Icon.svelte';
  import { MODES, candidates, draftOf, withoutMode, problems, unchanged } from '../lib/alarm.js';

  /** The alarm's settings, changed here: its modes (delays) and the sensors
   *  each one watches. Kept by Moli once the house's code is typed (the
   *  guard asks it each time); only while the alarm is disarmed. */
  let { id, setup, onclose } = $props();

  // A copy of the settings as they were when the editor opened.
  let draft = $state(draftOf(untrack(() => setup)));
  let picking = $state(false);
  let search = $state('');
  let busy = $state(false);

  const ICONS = { home: 'home', night: 'moon', away: 'walk' };
  const modeName = (m) => t(`alarme.mode.${m}`);
  const enabled = $derived(MODES.filter((m) => draft.modes[m]));
  const issues = $derived(problems(draft));
  // Nothing changed: nothing to keep (the alarm would show the same, and
  // the editor would wait for a change that never comes).
  const same = $derived(unchanged(draft, setup));
  const chosen = $derived(new Set(draft.sensors.map((s) => s.point)));
  const offered = $derived(
    candidates(Object.values(hub.devices), (d) => nameOf(d.id))
      .filter((c) => !chosen.has(c.point))
      .filter((c) => !search || c.name.toLowerCase().includes(search.toLowerCase())),
  );

  // A mode switched off shows its delays greyed, as they were: switched
  // back on, it gets them again.
  const DEFAULT_DELAYS = { exit: 0, entry: 0, trigger: 60 };
  let remembered = $state({});

  function toggleMode(m) {
    if (draft.modes[m]) {
      remembered[m] = { ...draft.modes[m] };
      draft = withoutMode(draft, m);
    } else draft.modes[m] = { ...(remembered[m] ?? DEFAULT_DELAYS) };
  }

  function setDelay(m, key, value) {
    const n = Math.round(Number(value));
    draft.modes[m][key] = Number.isFinite(n) ? n : 0;
  }

  function toggleSensorMode(s, m) {
    s.modes = s.modes.includes(m) ? s.modes.filter((x) => x !== m) : [...s.modes, m];
  }

  function add(c) {
    draft.sensors.push({ point: c.point, name: c.name, modes: [...enabled], immediate: false });
    picking = false;
    search = '';
  }

  async function save() {
    if (issues.length || same || busy) return;
    busy = true;
    const done = await act(`${id}/setup`, JSON.stringify(draft), t('alarme.reglages.enregistrer'));
    busy = false;
    if (done === 'ok') onclose();
    // Held for the code: the sheet asks it; the editor closes once the
    // alarm shows the new settings (a wrong code keeps the draft here).
    if (done === 'held') waiting = JSON.stringify(setup);
  }

  let waiting = $state(null);
  $effect(() => {
    if (waiting && JSON.stringify(setup) !== waiting) onclose();
  });
</script>

<section class="settings card" aria-labelledby="alarm-settings-title">
  <div class="head">
    <h3 id="alarm-settings-title"><Icon name="tune" size={18} />{t('alarme.reglages.titre')}</h3>
    <button class="ghost" onclick={onclose}>{t('commun.annuler')}</button>
  </div>
  <p class="hint">{t('alarme.reglages.aide')}</p>

  <h4>{t('alarme.modes')}</h4>
  <div class="modes">
    {#each MODES as m (m)}
      {@const d = draft.modes[m]}
      {@const shown = d ?? remembered[m] ?? DEFAULT_DELAYS}
      <div class="mode" class:off={!d}>
        <label class="switch-line">
          <input type="checkbox" checked={!!d} onchange={() => toggleMode(m)} />
          <Icon name={ICONS[m]} size={18} /><b>{modeName(m)}</b>
        </label>
        <div class="delays">
          {#each [['exit', 'alarme.sortie'], ['entry', 'alarme.entree'], ['trigger', 'alarme.sirene']] as [key, label] (key)}
            <label>
              <span>{t(label)}</span>
              <span class="num">
                <input type="number" min="0" max="3600" step="1" value={shown[key]} disabled={!d} oninput={(e) => setDelay(m, key, e.currentTarget.value)} />
                <small>s</small>
              </span>
            </label>
          {/each}
        </div>
        <small class="muted">{t(d ? 'alarme.reglages.sirene_zero' : 'alarme.reglages.mode_inactif')}</small>
      </div>
    {/each}
  </div>

  <h4>{t('alarme.capteurs')}</h4>
  {#if draft.sensors.length}
    <ul class="sensors">
      {#each draft.sensors as s, i (s.point)}
        <li>
          <div class="who">
            <input class="name" type="text" maxlength="80" bind:value={s.name} aria-label={t('alarme.reglages.nom')} />
            <small class="muted">{s.point}</small>
          </div>
          <div class="in" role="group" aria-label={t('alarme.reglages.surveille_en')}>
            {#each MODES as m (m)}
              {@const active = !!draft.modes[m]}
              <button
                class="chip"
                class:on={active && s.modes.includes(m)}
                aria-pressed={active && s.modes.includes(m)}
                disabled={!active}
                title={active ? undefined : t('alarme.reglages.mode_inactif_capteur', { mode: modeName(m) })}
                onclick={() => toggleSensorMode(s, m)}
              >
                {modeName(m)}
              </button>
            {/each}
            <label class="imm" title={t('alarme.reglages.immediat_aide')}>
              <input type="checkbox" bind:checked={s.immediate} />{t('alarme.reglages.immediat')}
            </label>
          </div>
          <button class="remove" onclick={() => draft.sensors.splice(i, 1)} aria-label={t('alarme.reglages.retirer', { nom: s.name ?? s.point })}>
            <Icon name="trash" size={16} />
          </button>
        </li>
      {/each}
    </ul>
    <p class="muted legend">{t('alarme.reglages.immediat_legende')}</p>
  {:else}
    <p class="muted">{t('alarme.reglages.aucun_capteur')}</p>
  {/if}

  {#if picking}
    <div class="picker">
      <input type="search" placeholder={t('alarme.reglages.chercher')} bind:value={search} aria-label={t('alarme.reglages.chercher')} />
      <ul>
        {#each offered.slice(0, 40) as c (c.point)}
          <li><button onclick={() => add(c)}><Icon name="plus" size={14} />{c.name}<small>{c.point}</small></button></li>
        {:else}
          <li class="muted">{t('alarme.reglages.rien_a_ajouter')}</li>
        {/each}
      </ul>
    </div>
  {:else}
    <button class="ghost add" onclick={() => (picking = true)}><Icon name="plus" size={16} />{t('alarme.reglages.ajouter')}</button>
  {/if}

  {#if issues.length}
    <ul class="issues">
      {#each issues as k (k)}<li><Icon name="warning" size={15} />{t(k)}</li>{/each}
    </ul>
  {/if}

  <div class="foot">
    <small class="muted"><Icon name="lock" size={14} />{t('alarme.reglages.code')}</small>
    <button class="save" onclick={save} disabled={issues.length > 0 || same || busy}>{t('alarme.reglages.enregistrer')}</button>
  </div>
</section>

<style>
  .settings {
    display: grid;
    gap: 12px;
  }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
  }

  h3 {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    font-size: 17px;
  }

  h4 {
    margin-top: 6px;
    font-size: 13px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--ink-3);
  }

  .hint,
  .muted {
    color: var(--ink-3);
    font-size: 13px;
  }

  .modes {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 240px), 1fr));
    gap: 10px;
  }

  .mode {
    display: grid;
    gap: 8px;
    padding: 12px;
    border-radius: var(--r-sm);
    background: var(--surface-2);
  }

  .mode.off {
    opacity: 0.7;
  }

  .switch-line {
    display: flex;
    align-items: center;
    gap: 8px;
    cursor: pointer;
  }

  .delays {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 8px;
  }

  .delays label {
    display: grid;
    gap: 3px;
    font-size: 12px;
    color: var(--ink-2);
  }

  .num {
    display: flex;
    align-items: center;
    gap: 4px;
  }

  input[type='number'],
  input[type='text'],
  input[type='search'] {
    width: 100%;
    min-width: 0;
    padding: 6px 8px;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--surface);
    color: var(--ink);
    font: inherit;
    font-size: 14px;
    font-variant-numeric: tabular-nums;
  }

  .sensors {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
  }

  .sensors li {
    display: grid;
    grid-template-columns: minmax(0, 1.4fr) minmax(0, 1fr) auto;
    align-items: center;
    gap: 10px;
    padding: 10px 0;
    border-top: 1px solid var(--line);
  }

  .sensors li:first-child {
    border-top: 0;
  }

  .who {
    display: grid;
    gap: 2px;
    min-width: 0;
  }

  .who small {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .in {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }

  .chip {
    padding: 4px 10px;
    border: 1px solid var(--line);
    border-radius: 999px;
    background: var(--surface);
    color: var(--ink-2);
    font: inherit;
    font-size: 12.5px;
    cursor: pointer;
  }

  .legend {
    margin: 6px 0 0;
    font-size: 12.5px;
  }

  .chip:disabled {
    border-style: dashed;
    opacity: 0.45;
    cursor: not-allowed;
  }

  .chip.on {
    border-color: var(--cool);
    background: var(--cool-soft);
    color: var(--ink);
    font-weight: 600;
  }

  .imm {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 12.5px;
    color: var(--ink-2);
  }

  .remove,
  .ghost {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    border: 0;
    border-radius: 999px;
    background: var(--surface-2);
    color: var(--ink-2);
    font: inherit;
    font-size: 14px;
    cursor: pointer;
  }

  .remove {
    width: 34px;
    height: 34px;
    justify-content: center;
  }

  .ghost {
    padding: 8px 14px;
  }

  .add {
    justify-self: start;
  }

  .picker {
    display: grid;
    gap: 8px;
  }

  .picker ul {
    list-style: none;
    margin: 0;
    padding: 0;
    max-height: 260px;
    overflow: auto;
    display: grid;
    gap: 4px;
  }

  .picker button {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 8px 10px;
    border: 0;
    border-radius: 8px;
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
    font-size: 14px;
    text-align: left;
    cursor: pointer;
  }

  .picker button small {
    margin-left: auto;
    color: var(--ink-3);
    font-size: 12px;
  }

  .issues {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 4px;
  }

  .issues li {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 10px;
    border-radius: var(--r-sm);
    background: var(--warm-soft);
    color: var(--warm-ink);
    font-size: 13px;
    font-weight: 600;
  }

  .foot {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
  }

  .foot small {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  .save {
    padding: 10px 20px;
    border: 0;
    border-radius: 999px;
    background: var(--cool);
    color: var(--surface);
    font: inherit;
    font-weight: 700;
    cursor: pointer;
  }

  .save:disabled {
    opacity: 0.5;
    cursor: default;
  }

  button:focus-visible,
  input:focus-visible {
    outline: 2px solid var(--cool);
    outline-offset: 2px;
  }

  @media (max-width: 640px) {
    .sensors li {
      grid-template-columns: minmax(0, 1fr) auto;
    }

    .in {
      grid-column: 1 / -1;
      grid-row: 2;
    }
  }
</style>
