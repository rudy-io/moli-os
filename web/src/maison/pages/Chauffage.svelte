<script>
  import { hub, device, deviceKind, value, act, nameOf, reachable, num } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from '../ui/Icon.svelte';
  import Spark from '../ui/infra/Spark.svelte';

  const heaters = $derived(Object.values(hub.devices).filter((d) => deviceKind(d) === 'heating'));
  const pacs = $derived(heaters.filter((d) => d.points.some((p) => p.key === 'compressor_active')));
  const circuits = $derived(heaters.filter((d) => !pacs.includes(d)).sort((a, b) => a.id.localeCompare(b.id)));

  const MODES = ['dhw', 'dhwAndHeating', 'standby'];
  const PROGRAMS = ['comfort', 'normal', 'reduced', 'standby'];
  const PHASES = ['off', 'ready'];
  const label = (family, list, v) => (list.includes(v) ? t(`maison.chauffage.${family}_${v}`) : v);

  const deg = (v) => (typeof v === 'number' ? `${num(v, v % 1 ? 1 : 0)}°` : '—');
  const pointOf = (id, key) => device(id)?.points.find((p) => p.key === key);
  const writable = (id, key) => !!pointOf(id, key)?.access?.write;

  function nudge(id, key, delta, what) {
    const cur = value(id, key);
    const kind = pointOf(id, key)?.kind ?? {};
    if (typeof cur !== 'number' || !reachable(id)) return;
    const next = Math.min(kind.max ?? Infinity, Math.max(kind.min ?? -Infinity, cur + delta * (kind.step ?? 1)));
    if (next !== cur) act(`${id}/${key}`, next, t('maison.chauffage.action_consigne', { name: nameOf(id), what }));
  }

  const send = (id, key, v, text) => reachable(id) && act(`${id}/${key}`, v, text);
</script>

{#snippet fact(title, text)}
  <div class="fact"><small>{title}</small><b class="num">{text}</b></div>
{/snippet}

{#snippet setpoint(id, key, title)}
  {#if value(id, key) != null}
    <div class="ctrl">
      <span>{title}</span>
      <div class="setpoint">
        {#if writable(id, key)}<button onclick={() => nudge(id, key, -1, title)} aria-label={t('commun.clim.moins_chaud')}>−</button>{/if}
        <b class="num">{deg(value(id, key))}</b>
        {#if writable(id, key)}<button onclick={() => nudge(id, key, 1, title)} aria-label={t('commun.clim.plus_chaud')}>+</button>{/if}
      </div>
    </div>
  {/if}
{/snippet}

<div class="chauffage">
  <header>
    <h1 class="page-title">{t('maison.chauffage.titre')}</h1>
    <p class="page-sub">{t('maison.chauffage.sous_titre')}</p>
  </header>

  {#if !heaters.length}
    <p class="muted">{t('maison.chauffage.aucun')}</p>
  {/if}

  <div class="grid">
    {#each pacs as p (p.id)}
      {@const id = p.id}
      {@const on = value(id, 'compressor_active') === true}
      {@const phase = value(id, 'compressor_phase')}
      <section class="card" class:unreachable={!reachable(id)}>
        <div class="card-head">
          <h2><Icon name="fire" size={18} />{nameOf(id)}</h2>
          <span class="state" class:on>
            {on ? t('maison.chauffage.compresseur_actif') : t('maison.chauffage.compresseur_arret')}{on && typeof value(id, 'compressor_load') === 'number' ? ` · ${num(value(id, 'compressor_load'))}%` : ''}
          </span>
        </div>

        <h3>{t('maison.chauffage.temperatures')}</h3>
        <div class="facts">
          {@render fact(t('maison.chauffage.exterieur'), deg(value(id, 'outside_temperature')))}
          {@render fact(t('maison.chauffage.depart'), deg(value(id, 'supply_temperature')))}
          {@render fact(t('maison.chauffage.retour'), deg(value(id, 'return_temperature')))}
          {@render fact(t('maison.chauffage.source'), deg(value(id, 'source_temperature')))}
          {@render fact(t('maison.chauffage.tampon'), deg(value(id, 'buffer_temperature')))}
          {@render fact(t('maison.chauffage.ecs'), deg(value(id, 'dhw_temperature')))}
        </div>

        <h3>{t('maison.chauffage.compresseur')}</h3>
        <div class="facts">
          {#if phase != null}{@render fact(t('maison.chauffage.phase'), label('phase', PHASES, phase))}{/if}
          {@render fact(t('maison.chauffage.charge'), value(id, 'compressor_load') != null ? `${num(value(id, 'compressor_load'))}%` : '—')}
          {@render fact(t('maison.chauffage.demarrages'), num(value(id, 'compressor_starts')))}
          {@render fact(t('maison.chauffage.heures'), num(value(id, 'compressor_hours'), 0))}
        </div>

        <h3>{t('maison.chauffage.cop')}</h3>
        <div class="facts">
          {@render fact(t('maison.chauffage.cop_chauffage'), num(value(id, 'cop_heating'), 1))}
          {@render fact(t('maison.chauffage.cop_ecs'), num(value(id, 'cop_dhw'), 1))}
          {@render fact(t('maison.chauffage.cop_total'), num(value(id, 'cop_total'), 1))}
        </div>

        <h3>{t('maison.chauffage.eau_chaude')}</h3>
        <div class="row">
          {@render setpoint(id, 'dhw_target', t('maison.chauffage.consigne_ecs'))}
          {@render setpoint(id, 'dhw_target2', t('maison.chauffage.consigne_ecs2'))}
          {#if writable(id, 'dhw_boost')}
            {@const boost = value(id, 'dhw_boost') === true}
            <button class="pill" disabled={boost} aria-pressed={boost} onclick={() => send(id, 'dhw_boost', true, t('maison.chauffage.charge_unique_action', { name: nameOf(id) }))}>
              <Icon name="flash" size={16} />{boost ? t('maison.chauffage.charge_unique_en_cours') : t('maison.chauffage.charge_unique')}
            </button>
          {/if}
        </div>
        {#if value(id, 'dhw_charging') === true}<p class="muted">{t('maison.chauffage.ecs_en_charge')}</p>{/if}

        <h3>{t('maison.chauffage.historique')}</h3>
        <div class="sparks">
          <Spark point="{id}/outside_temperature" hours={24} label={t('maison.chauffage.exterieur')} unit="°" />
          <Spark point="{id}/dhw_temperature" hours={24} label={t('maison.chauffage.ecs')} unit="°" />
        </div>
      </section>
    {/each}

    {#each circuits as c (c.id)}
      {@const id = c.id}
      {@const mode = value(id, 'mode')}
      {@const program = value(id, 'program')}
      <section class="card" class:unreachable={!reachable(id)}>
        <div class="card-head">
          <h2><Icon name="fire" size={18} />{nameOf(id)}</h2>
          {#if program != null}<span class="state">{t('maison.chauffage.programme', { program: label('programme', PROGRAMS, program) })}</span>{/if}
        </div>

        <div class="facts">
          {#if value(id, 'room_temperature') != null}{@render fact(t('maison.chauffage.piece'), deg(value(id, 'room_temperature')))}{/if}
          {#if value(id, 'supply_temperature') != null}{@render fact(t('maison.chauffage.depart'), deg(value(id, 'supply_temperature')))}{/if}
          {#if value(id, 'flow_target') != null}{@render fact(t('maison.chauffage.consigne_depart'), deg(value(id, 'flow_target')))}{/if}
          {#if value(id, 'circulation_pump') != null}
            {@render fact(t('maison.chauffage.pompe'), value(id, 'circulation_pump') ? t('commun.oui') : t('commun.non'))}
          {/if}
          {#if value(id, 'frost_protection') != null}
            {@render fact(t('maison.chauffage.hors_gel'), value(id, 'frost_protection') ? t('commun.oui') : t('commun.non'))}
          {/if}
        </div>

        {#if writable(id, 'mode')}
          <div class="ctrl">
            <span>{t('maison.chauffage.mode')}</span>
            <div class="modes">
              {#each pointOf(id, 'mode')?.kind?.values ?? [] as m (m)}
                <button class:active={mode === m} onclick={() => m !== mode && send(id, 'mode', m, t('maison.chauffage.mode_action', { name: nameOf(id), mode: label('mode', MODES, m) }))}>
                  {label('mode', MODES, m)}
                </button>
              {/each}
            </div>
          </div>
        {/if}

        <div class="row">
          {@render setpoint(id, 'comfort_temperature', t('maison.chauffage.consigne_confort'))}
          {@render setpoint(id, 'normal_temperature', t('maison.chauffage.consigne_normale'))}
          {@render setpoint(id, 'reduced_temperature', t('maison.chauffage.consigne_reduite'))}
        </div>

        {#if writable(id, 'comfort_active')}
          {@const comfortOn = value(id, 'comfort_active') === true}
          <div class="row">
            <button class="pill" aria-pressed={comfortOn} onclick={() => send(id, 'comfort_active', !comfortOn, t('maison.chauffage.confort_action', { name: nameOf(id) }))}>
              <Icon name="fire" size={16} />{comfortOn ? t('maison.chauffage.confort_actif') : t('maison.chauffage.confort_activer')}
            </button>
          </div>
        {/if}
      </section>
    {/each}
  </div>
</div>

<style>
  .chauffage {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 20px;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 420px), 1fr));
    gap: 20px;
    align-items: start;
  }
  .card {
    display: grid;
    gap: 12px;
    padding: 16px 18px;
    border-radius: var(--r-md);
    background: var(--surface-2);
  }
  .unreachable {
    opacity: 0.5;
  }
  .card-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    flex-wrap: wrap;
  }
  .card-head h2 {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 17px;
    font-weight: 650;
  }
  h3 {
    font-size: 11.5px;
    font-weight: 650;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--ink-3);
    margin-top: 4px;
  }
  .state {
    font-size: 12.5px;
    color: var(--ink-3);
  }
  .state.on {
    color: var(--warm-ink);
    font-weight: 650;
  }
  .facts {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(96px, 1fr));
    gap: 10px 14px;
  }
  .fact {
    display: grid;
  }
  .fact small {
    font-size: 11px;
    color: var(--ink-3);
    font-weight: 650;
  }
  .fact b {
    font-size: 17px;
  }
  .row {
    display: flex;
    align-items: flex-end;
    gap: 18px;
    flex-wrap: wrap;
  }
  .ctrl {
    display: grid;
    gap: 6px;
  }
  .ctrl > span {
    font-size: 12.5px;
    color: var(--ink-2);
    font-weight: 600;
  }
  .setpoint {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .setpoint b {
    min-width: 44px;
    text-align: center;
    font-size: 16px;
  }
  .setpoint button,
  .modes button {
    height: 32px;
    border: none;
    background: var(--surface);
    color: var(--ink);
    cursor: pointer;
  }
  .setpoint button {
    width: 32px;
    border-radius: 50%;
    font-size: 18px;
  }
  .modes {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .modes button {
    padding: 0 12px;
    border-radius: 999px;
    font: inherit;
    font-size: 13px;
  }
  .modes button.active {
    background: var(--warm-soft);
    color: var(--warm-ink);
  }
  .pill {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    border: none;
    border-radius: 999px;
    padding: 8px 12px;
    background: var(--surface);
    color: var(--ink);
    font: inherit;
    font-size: 13px;
    cursor: pointer;
  }
  .pill[aria-pressed='true'] {
    background: var(--warm-soft);
    color: var(--warm-ink);
  }
  .pill:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .sparks {
    display: grid;
    gap: 12px;
  }
</style>
