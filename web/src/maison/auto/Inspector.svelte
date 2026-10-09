<script>
  import Icon from '../ui/Icon.svelte';
  import PointPicker from './PointPicker.svelte';
  import Fields from './Fields.svelte';
  import { CATALOG, pointSpec, ruleText } from '../lib/auto.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';

  /** Settings of the selected step. Every change goes through `onupdate`
   *  (a patch), so the editor knows what changed. */
  let { node, problems = [], onupdate, onremove, onclose } = $props();

  const meta = $derived(CATALOG[node.type]);
  const set = (patch) => onupdate?.(patch);

  function defaultValue(point) {
    const spec = pointSpec(point);
    if (!spec) return null;
    if (spec.kind.type === 'binary') return true;
    if (spec.kind.type === 'enum') return spec.kind.values?.[0] ?? null;
    if (spec.kind.type === 'numeric') return spec.kind.min ?? 0;
    return '';
  }

  // Rules (if, wait_for).
  function setRule(i, patch) {
    const rules = node.rules.map((r, j) => (j === i ? { ...r, ...patch } : r));
    set({ rules });
  }
  function ruleOf(kind) {
    if (kind === 'sun') return { kind: 'sun', is: 'night' };
    if (kind === 'time') return { kind: 'time', after: '22:00', before: '06:00', days: [] };
    return { kind: 'state', point: '', op: 'eq', value: true };
  }
  const OPS = [
    ['eq', '='],
    ['ne', '≠'],
    ['gt', '>'],
    ['ge', '≥'],
    ['lt', '<'],
    ['le', '≤'],
  ];
</script>

{#snippet rule(r, change)}
  <div class="rule">
    <div class="seg small">
      {#each ['state', 'time', 'sun'] as k (k)}
        <button class:on={r.kind === k} onclick={() => change(ruleOf(k), true)}>{t('automatismes.inspecteur.regle.' + k)}</button>
      {/each}
    </div>
    {#if r.kind === 'state'}
      <PointPicker point={r.point} onpick={(p) => change({ point: p, value: defaultValue(p) })} />
      {#if r.point}
        <div class="row">
          {#if pointSpec(r.point)?.kind.type === 'numeric'}
            <select value={r.op ?? 'eq'} onchange={(e) => change({ op: e.currentTarget.value })} aria-label={t('automatismes.inspecteur.comparaison')}>
              {#each OPS as [op, sign] (op)}<option value={op}>{sign}</option>{/each}
            </select>
          {:else}
            <select value={r.op ?? 'eq'} onchange={(e) => change({ op: e.currentTarget.value })} aria-label={t('automatismes.inspecteur.comparaison')}>
              <option value="eq">{t('automatismes.inspecteur.est')}</option>
              <option value="ne">{t('automatismes.inspecteur.nest_pas')}</option>
            </select>
          {/if}
          <Fields kind="value" point={r.point} value={r.value} onchange={(v) => change({ value: v })} />
        </div>
      {/if}
    {:else if r.kind === 'time'}
      <div class="row">
        <span>{t('automatismes.inspecteur.entre')}</span>
        <Fields kind="time" value={r.after} onchange={(v) => change({ after: v })} />
        <span>{t('automatismes.inspecteur.et')}</span>
        <Fields kind="time" value={r.before} onchange={(v) => change({ before: v })} />
      </div>
      <Fields kind="days" value={r.days} onchange={(v) => change({ days: v })} />
    {:else}
      <div class="seg">
        <button class:on={r.is === 'day'} onclick={() => change({ is: 'day' })}><Icon name="sun" size={16} />{t('automatismes.inspecteur.il_fait_jour')}</button>
        <button class:on={r.is === 'night'} onclick={() => change({ is: 'night' })}><Icon name="moon" size={16} />{t('automatismes.inspecteur.il_fait_nuit')}</button>
      </div>
    {/if}
  </div>
{/snippet}

<aside class="inspector">
  <header class={meta?.family}>
    <span class="tile"><Icon name={meta?.icon ?? 'info'} size={22} /></span>
    <h3>{meta?.label ?? node.type}</h3>
    <button class="icon" onclick={onclose} aria-label={t('commun.fermer')}><Icon name="close" size={18} /></button>
  </header>

  {#each problems as p, i (i)}
    <p class="problem {p.level}"><Icon name="warning" size={16} />{p.message}</p>
  {/each}

  <div class="fields">
    {#if node.type === 'when_state'}
      <span class="lbl">{t('automatismes.inspecteur.quel_appareil')}</span>
      <PointPicker point={node.point} onpick={(p) => set({ point: p, to: defaultValue(p) })} />
      {#if node.point}
        <span class="lbl">{t('automatismes.inspecteur.quand_devient')}</span>
        <Fields kind="value" point={node.point} value={node.to} onchange={(v) => set({ to: v })} />
        <button class="link" onclick={() => set({ to: null })}>{node.to == null ? '✓ ' : ''}{t('automatismes.inspecteur.tout_changement')}</button>
      {/if}
      <span class="lbl">{t('automatismes.inspecteur.pendant_au_moins')} <small>{t('automatismes.inspecteur.zero_tout_de_suite')}</small></span>
      {#if node.for_s}
        <Fields kind="duration" value={node.for_s} onchange={(v) => set({ for_s: v })} />
        <button class="link" onclick={() => set({ for_s: 0 })}>{t('automatismes.inspecteur.tout_de_suite')}</button>
      {:else}
        <button class="link" onclick={() => set({ for_s: 60 })}>{t('automatismes.inspecteur.seulement_si_dure')}</button>
      {/if}
    {:else if node.type === 'when_threshold'}
      <span class="lbl">{t('automatismes.inspecteur.quelle_mesure')}</span>
      <PointPicker point={node.point} mode="number" onpick={(p) => set({ point: p })} />
      <span class="lbl">{t('automatismes.inspecteur.au_dessus')} <small>{t('automatismes.inspecteur.vide_sans')}</small></span>
      <input type="number" value={node.above ?? ''} onchange={(e) => set({ above: e.currentTarget.value === '' ? null : Number(e.currentTarget.value) })} />
      <span class="lbl">{t('automatismes.inspecteur.en_dessous')} <small>{t('automatismes.inspecteur.vide_sans')}</small></span>
      <input type="number" value={node.below ?? ''} onchange={(e) => set({ below: e.currentTarget.value === '' ? null : Number(e.currentTarget.value) })} />
      <span class="lbl">{t('automatismes.inspecteur.pendant_au_moins')}</span>
      <Fields kind="duration" value={node.for_s || 60} onchange={(v) => set({ for_s: v })} />
      <button class="link" onclick={() => set({ for_s: 0 })}>{t('automatismes.inspecteur.tout_de_suite')}</button>
    {:else if node.type === 'at_time'}
      <span class="lbl">{t('automatismes.inspecteur.quelle_heure')}</span>
      <Fields kind="time" value={node.at} onchange={(v) => set({ at: v })} />
      <span class="lbl">{t('automatismes.inspecteur.quels_jours')}</span>
      <Fields kind="days" value={node.days} onchange={(v) => set({ days: v })} />
    {:else if node.type === 'at_sun'}
      <div class="seg">
        <button class:on={node.event === 'rise'} onclick={() => set({ event: 'rise' })}><Icon name="sunrise" size={16} />{t('automatismes.inspecteur.lever')}</button>
        <button class:on={node.event === 'set'} onclick={() => set({ event: 'set' })}><Icon name="sunset" size={16} />{t('automatismes.inspecteur.coucher')}</button>
      </div>
      <span class="lbl">{t('automatismes.inspecteur.decalage')} <small>{t('automatismes.inspecteur.decalage_aide')}</small></span>
      <input type="number" value={node.offset_min ?? 0} onchange={(e) => set({ offset_min: Number(e.currentTarget.value) || 0 })} />
      <span class="lbl">{t('automatismes.inspecteur.quels_jours')}</span>
      <Fields kind="days" value={node.days} onchange={(v) => set({ days: v })} />
    {:else if node.type === 'every'}
      <span class="lbl">{t('automatismes.inspecteur.toutes_les')}</span>
      <input type="number" min="1" value={node.minutes ?? 15} onchange={(e) => set({ minutes: Math.max(1, Number(e.currentTarget.value) || 1) })} />
    {:else if node.type === 'on_start' || node.type === 'manual'}
      <p class="muted">{node.type === 'manual' ? t('automatismes.inspecteur.lance_manuel') : t('automatismes.inspecteur.lance_demarrage')}</p>
    {:else if node.type === 'if'}
      <div class="seg">
        <button class:on={node.all !== false} onclick={() => set({ all: true })}>{t('automatismes.inspecteur.toutes_conditions')}</button>
        <button class:on={node.all === false} onclick={() => set({ all: false })}>{t('automatismes.inspecteur.au_moins_une')}</button>
      </div>
      {#each node.rules as r, i (i)}
        <div class="card-rule">
          {@render rule(r, (patch, replace) => (replace ? set({ rules: node.rules.map((x, j) => (j === i ? patch : x)) }) : setRule(i, patch)))}
          {#if node.rules.length > 1}
            <button class="link danger" onclick={() => set({ rules: node.rules.filter((_, j) => j !== i) })}>{t('automatismes.inspecteur.retirer', { regle: ruleText(r) })}</button>
          {/if}
        </div>
      {/each}
      <button class="link" onclick={() => set({ rules: [...node.rules, ruleOf('state')] })}>{t('automatismes.inspecteur.autre_condition')}</button>
    {:else if node.type === 'set'}
      <span class="lbl">{t('automatismes.inspecteur.quel_appareil')}</span>
      <PointPicker point={node.point} mode="write" onpick={(p) => set({ point: p, value: defaultValue(p) })} />
      {#if node.point}
        <span class="lbl">{t('automatismes.inspecteur.mettre_sur')}</span>
        <Fields kind="value" point={node.point} value={node.value} onchange={(v) => set({ value: v })} />
      {/if}
    {:else if node.type === 'toggle'}
      <span class="lbl">{t('automatismes.inspecteur.quel_appareil')}</span>
      <PointPicker point={node.point} mode="write" onpick={(p) => set({ point: p })} />
    {:else if node.type === 'wait'}
      <span class="lbl">{t('automatismes.inspecteur.attendre')}</span>
      <Fields kind="duration" value={node.seconds} onchange={(v) => set({ seconds: v })} />
    {:else if node.type === 'wait_for'}
      <span class="lbl">{t('automatismes.inspecteur.jusqua')}</span>
      {@render rule(node.rule ?? ruleOf('state'), (patch, replace) => set({ rule: replace ? patch : { ...node.rule, ...patch } }))}
      <span class="lbl">{t('automatismes.inspecteur.au_plus')}</span>
      <Fields kind="duration" value={node.timeout_s} onchange={(v) => set({ timeout_s: v })} />
    {:else if node.type === 'notify'}
      <span class="lbl">{t('automatismes.inspecteur.ou')}</span>
      <div class="seg">
        {#each ['maison', 'voix', 'telephone', 'telegram'] as c (c)}
          <button
            class:on={(node.channels ?? []).includes(c)}
            onclick={() => {
              const has = (node.channels ?? []).includes(c);
              const next = has ? node.channels.filter((x) => x !== c) : [...(node.channels ?? []), c];
              set({ channels: next.length ? next : ['maison'] });
            }}>{t('automatismes.inspecteur.canal.' + c)}</button>
        {/each}
      </div>
      <span class="lbl">{t('automatismes.inspecteur.titre')} <small>{t('automatismes.inspecteur.facultatif')}</small></span>
      <input value={node.title ?? ''} onchange={(e) => set({ title: e.currentTarget.value || null })} />
      <span class="lbl">{t('automatismes.inspecteur.message')}</span>
      <textarea rows="3" value={node.message ?? ''} onchange={(e) => set({ message: e.currentTarget.value })}></textarea>
      <p class="hint">{t('automatismes.inspecteur.aide_message', { texte: '{{texte}}', heure: '{{heure}}' })}</p>
    {:else if node.type === 'write'}
      <span class="lbl">{t('automatismes.inspecteur.ecrire')}</span>
      <textarea rows="4" value={node.prompt ?? ''} onchange={(e) => set({ prompt: e.currentTarget.value })} placeholder={t('automatismes.inspecteur.ecrire_exemple')}></textarea>
      <p class="hint">{t('automatismes.inspecteur.aide_ecrire', { texte: '{{texte}}' })}</p>
    {/if}
  </div>

  <button class="remove" onclick={onremove}><Icon name="trash" size={16} />{t('automatismes.inspecteur.supprimer')}</button>
</aside>

<style>
  .inspector {
    display: grid;
    align-content: start;
    gap: 14px;
    padding: 18px;
    border-radius: var(--r-lg);
    background: var(--surface);
    box-shadow: var(--shadow);
    max-height: 100%;
    overflow-y: auto;
  }

  header {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  h3 {
    flex: 1;
    font-size: 17px;
    font-weight: 700;
  }

  .tile {
    width: 42px;
    height: 42px;
    border-radius: 13px;
    display: grid;
    place-items: center;
    background: var(--surface-2);
    color: var(--ink-2);
  }

  .trigger .tile {
    background: color-mix(in srgb, #e9a23b 16%, var(--surface));
    color: #e9a23b;
  }
  .logic .tile {
    background: color-mix(in srgb, #4b74f2 14%, var(--surface));
    color: #4b74f2;
  }
  .action .tile {
    background: color-mix(in srgb, #2c9a88 14%, var(--surface));
    color: #2c9a88;
  }
  .notify .tile {
    background: color-mix(in srgb, #b05fd8 14%, var(--surface));
    color: #b05fd8;
  }
  .moli .tile {
    background: conic-gradient(from 200deg, #f5c84b, #f0a53a, #ef7f5a, #8f8cf5, #5fb4f0, #f5c84b);
    color: #fff;
  }

  .icon {
    width: 36px;
    height: 36px;
    border-radius: 50%;
    border: 0;
    background: var(--surface-2);
    display: grid;
    place-items: center;
  }

  .fields {
    display: grid;
    gap: 8px;
  }

  .lbl {
    font-size: 13px;
    font-weight: 700;
    color: var(--ink-2);
    margin-top: 6px;
  }

  .lbl small {
    font-weight: 500;
    color: var(--ink-3);
  }

  input,
  textarea,
  select {
    border: 0;
    border-radius: 12px;
    padding: 10px 12px;
    background: var(--surface-2);
    font: inherit;
    color: var(--ink);
    width: 100%;
  }

  select {
    width: auto;
  }

  textarea {
    resize: vertical;
  }

  .seg {
    display: flex;
    gap: 4px;
    padding: 4px;
    border-radius: 14px;
    background: var(--surface-2);
  }

  .seg button {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    border: 0;
    border-radius: 10px;
    padding: 9px 10px;
    background: none;
    color: var(--ink-2);
    font-weight: 650;
    font-size: 13.5px;
  }

  .seg button.on {
    background: var(--surface);
    color: var(--ink);
    box-shadow: var(--shadow);
  }

  .seg.small button {
    padding: 7px 8px;
    font-size: 12.5px;
  }

  .rule {
    display: grid;
    gap: 8px;
  }

  .card-rule {
    display: grid;
    gap: 6px;
    padding: 12px;
    border-radius: 16px;
    box-shadow: inset 0 0 0 1px var(--line);
  }

  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }

  .link {
    justify-self: start;
    border: 0;
    background: none;
    padding: 4px 0;
    color: var(--cool);
    font-weight: 650;
    font-size: 13.5px;
  }

  .link.danger {
    color: var(--alert);
  }

  .hint {
    font-size: 12.5px;
    color: var(--ink-3);
  }

  .problem {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 12px;
    border-radius: 12px;
    font-size: 13.5px;
    background: var(--alert-soft);
    color: var(--alert);
  }

  .problem.warning {
    background: var(--warm-soft);
    color: var(--warm-ink);
  }

  .remove {
    justify-self: start;
    display: flex;
    align-items: center;
    gap: 8px;
    border: 0;
    border-radius: 999px;
    padding: 9px 14px;
    background: var(--alert-soft);
    color: var(--alert);
    font-weight: 650;
    margin-top: 6px;
  }
</style>
