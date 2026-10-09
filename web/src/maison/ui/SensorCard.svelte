<script>
  import { device, nameOf, reachable, since, relative, home, num } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** Any device shown by its readings, in words. */
  let { id, name = null } = $props();

  const d = $derived(device(id));

  // Said when read (t() inside), never once for all.
  const WORDS = {
    contact: (v) => (v ? t('commun.capteur.fermee') : t('commun.capteur.ouverte')),
    doorcontact_state: (v) => (v ? t('commun.capteur.ouverte') : t('commun.capteur.fermee')),
    occupancy: (v) => (v ? t('commun.capteur.quelquun') : t('commun.capteur.personne')),
    smoke: (v) => (v ? t('commun.capteur.fumee') : t('commun.capteur.rien')),
    water_leak: (v) => (v ? t('commun.capteur.fuite') : t('commun.capteur.au_sec')),
    battery_low: (v) => (v ? t('commun.capteur.pile_faible') : t('commun.capteur.pile_ok')),
    // Tuya's words.
    pir: (v) => (v === 'pir' ? t('commun.capteur.mouvement') : t('commun.capteur.rien')),
    watersensor_state: (v) => (v === 'alarm' ? t('commun.capteur.fuite') : t('commun.capteur.au_sec')),
    battery_state: (v) => {
      const key = { low: 'commun.capteur.faible', middle: 'commun.capteur.moyenne', high: 'commun.capteur.bonne' }[v];
      return key ? t(key) : v;
    },
  };

  function say(p, v) {
    if (v == null) return null;
    if (WORDS[p.key] && (typeof v === 'boolean' || typeof v === 'string')) return WORDS[p.key](v);
    if (typeof v === 'boolean') return v ? t('commun.oui') : t('commun.non');
    if (typeof v === 'number') return `${num(v, Math.abs(v) < 10 && !Number.isInteger(v) ? 1 : 0)}${p.unit ? ` ${p.unit}` : ''}`;
    return String(v).length > 32 ? `${String(v).slice(0, 32)}…` : String(v);
  }

  const rows = $derived(
    (d?.points ?? [])
      .map((p) => ({ p, text: say(p, d.state?.[p.key]?.value) }))
      .filter((r) => r.text != null && !/^(linkquality|update|identify|countdown|cycle_time|random_time|switch_inching|relay_status|light_mode|child_lock)/.test(r.p.key))
      .slice(0, 6),
  );
  const last = $derived(Math.max(0, ...(d?.points ?? []).map((p) => since(id, p.key) ?? 0)));
</script>

<section class="card sensor" class:off={!reachable(id)}>
  <div class="card-head">
    <h2><Icon name="info" size={18} />{nameOf(id, name)}</h2>
    {#if last}<span class="muted when">{relative(last, home.now)}</span>{/if}
  </div>
  {#if rows.length}
    <dl>
      {#each rows as r (r.p.key)}
        <div><dt>{r.p.label || r.p.key}</dt><dd class="num">{r.text}</dd></div>
      {/each}
    </dl>
  {:else}
    <p class="muted">{t('commun.capteur.pas_de_mesure')}</p>
  {/if}
</section>

<style>
  .off {
    opacity: 0.6;
  }

  .when {
    font-size: 13px;
  }

  dl {
    margin: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(130px, 1fr));
    gap: 14px 18px;
  }

  dt {
    font-size: 12px;
    font-weight: 650;
    color: var(--ink-3);
  }

  dd {
    margin: 2px 0 0;
    font-size: 18px;
    font-weight: 650;
  }
</style>
