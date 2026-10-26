<script>
  import { device, value, reachable, nameOf, act, num } from '../lib/home.svelte.js';
  import { STATE_TONE, stateWord, drinkName, programName, settingWord, whyNot } from '../lib/coffee.js';
  import { parseRecipes } from '../lib/coffee-recipes.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';
  import CoffeeBrew from './CoffeeBrew.svelte';
  import DrinkIcon from './DrinkIcon.svelte';
  import CoffeeRecipes from './CoffeeRecipes.svelte';

  /** One coffee machine, as J.O.E.'s cockpit: its state and what it waits
   *  for, a button per drink, the recipes side by side, the maintenance
   *  programs, how close each maintenance is, the counters, and its
   *  settings. A drink opens its window (its recipe, changeable for the cup,
   *  then « Brew »); a program asks twice, the tablet at hand. A machine
   *  that cannot take an order now (its menu open, busy, waiting for a
   *  hand, off) says why under its name, and a tap opens a window that
   *  says what to do.
   *  No cancel: J.O.E.'s cancel is acknowledged by an E8 and stops nothing. */
  let { id } = $props();

  const d = $derived(device(id));
  const v = (k) => value(id, k);
  const point = (k) => d?.points.find((p) => p.key === k);
  const ok = $derived(reachable(id));
  const status = $derived(ok ? v('status') : null);
  const idle = $derived(status === 'ready' || status === 'energy_saving');
  const busy = $derived(ok && !idle && status != null);
  // Why it cannot take an order now, if it cannot.
  const why = $derived(whyNot(ok, status));
  const progress = $derived(Number(v('progress') ?? 0));
  const doing = $derived(v('activity') || '');
  const doingName = $derived(!doing ? '' : point('brew')?.kind.values?.includes(doing) ? drinkName(doing) : programName(doing));

  const ALERTS = ['water_empty', 'grounds_full', 'drip_tray_full', 'drip_tray_missing', 'grounds_container_missing', 'beans_empty', 'machine_error', 'filter_due', 'cleaning_due', 'descaling_due', 'milk_rinse_due', 'milk_cleaning_due'];
  const alerts = $derived(ok ? ALERTS.filter((k) => v(k) === true).map((k) => ({ k, label: point(k)?.label ?? k, due: k.endsWith('_due') })) : []);

  const drinks = $derived(point('brew')?.kind.values ?? []);
  // The recipes the machine keeps, and whether a cup may change them.
  const recipes = $derived(parseRecipes(v('recipes')));
  const custom = $derived(!!point('brew_custom')?.access?.write);
  let picked = $state(null);
  const programs = $derived(point('maintenance')?.kind.values ?? []);
  const NEEDS = ['cleaning_need', 'descaling_need', 'filter_wear'];
  const needs = $derived(NEEDS.filter((k) => typeof v(k) === 'number').map((k) => ({ k, label: point(k)?.label ?? k, n: v(k) })));
  const COUNTS = ['cleanings', 'descalings', 'filter_changes', 'milk_rinses', 'milk_cleanings', 'coffee_rinses'];
  const counts = $derived(COUNTS.filter((k) => typeof v(k) === 'number').map((k) => ({ k, label: point(k)?.label ?? k, n: v(k) })));
  const brews = $derived(
    (d?.points ?? [])
      .filter((p) => p.key.startsWith('brews.') && Number(v(p.key)) > 0)
      .map((p) => ({ k: p.key, name: drinkName(p.key.slice(6)), n: v(p.key) }))
      .sort((a, b) => b.n - a.n),
  );
  const settings = $derived((d?.points ?? []).filter((p) => p.semantic === 'config' && p.access?.write));
  const filter = $derived(v('filter_fitted') === true);

  // Asked twice: the first tap names it, the second sends it.
  let asked = $state(null);
  let askedTimer;
  function twice(key, fire) {
    if (asked !== key) {
      asked = key;
      clearTimeout(askedTimer);
      askedTimer = setTimeout(() => (asked = null), 5000);
      return;
    }
    asked = null;
    fire();
  }
  // The window that says why, and what to do.
  let dialog;
  const explain = () => dialog?.showModal();
  // An order only when the machine can take it; refused on the way (its
  // state changed since the last reading), the window too.
  async function send(point, val, label) {
    if (why) return explain(), 'error';
    const done = await act(point, val, label);
    if (done === 'error' && why) explain();
    return done;
  }
  const brew = (slug) => (why ? explain() : (picked = slug));
  // From the drink's window: as the machine keeps it, or changed for this cup.
  function brewPicked(changes) {
    const slug = picked;
    const label = t('salon.cafe.act_preparer', { boisson: drinkName(slug), nom: nameOf(id) });
    if (changes && custom) return send(`${id}/brew_custom`, JSON.stringify({ drink: slug, ...changes }), label);
    return send(`${id}/brew`, slug, label);
  }
  const program = (p) => (why ? explain() : twice(`program:${p}`, () => send(`${id}/maintenance`, p, t('salon.cafe.act_entretien', { programme: programName(p), nom: nameOf(id) }))));
  function set(p, input) {
    const val = p.kind.type === 'numeric' ? Number(input.value) : input.value;
    if (why) {
      input.value = v(p.key) ?? '';
      return explain();
    }
    if (val === v(p.key) || (p.kind.type === 'numeric' && !Number.isFinite(val))) return;
    send(`${id}/${p.key}`, val, t('salon.cafe.act_reglage', { reglage: p.label, nom: nameOf(id) }));
  }
  // A setting not to be changed now: the window instead of the list.
  function hold(e) {
    if (!why) return;
    e.preventDefault();
    explain();
  }
</script>

<section class="card coffee" data-state={status ?? 'off'}>
  <div class="head">
    <span class="logo"><Icon name="coffee-maker" size={22} /></span>
    <div class="title">
      <h2>{nameOf(id)}</h2>
      {#if d?.model || d?.manufacturer}<span class="muted">{[d.manufacturer, d.model].filter(Boolean).join(' ')}</span>{/if}
    </div>
    <span class="chip {STATE_TONE[status] ?? ''}">{stateWord(status)}</span>
  </div>

  {#if why && !(busy && doingName)}
    <button class="why" onclick={explain}><Icon name="info" size={16} />{t(`salon.cafe.pourquoi.${why}`)}</button>
  {/if}

  {#if busy && doingName}
    <div class="doing">
      <b>{doingName}</b>
      <div class="bar" role="progressbar" aria-valuenow={progress} aria-valuemin="0" aria-valuemax="100"><i style:width="{Math.min(100, progress)}%"></i></div>
      <strong class="num">{num(progress)} %</strong>
    </div>
  {/if}

  {#if alerts.length}
    <ul class="alerts">
      {#each alerts as a (a.k)}
        <li class="chip {a.due ? 'cool' : 'alert'}">{a.label}</li>
      {/each}
    </ul>
  {/if}

  {#if ok && drinks.length}
    <div class="block">
      <h3>{t('salon.cafe.boissons')}</h3>
      <p class="hint muted">{t('salon.cafe.choisir')}</p>
      <div class="drinks">
        {#each drinks as slug (slug)}
          <button class="drink" class:waiting={!!why} onclick={() => brew(slug)}>
            <DrinkIcon drink={slug} size={40} />
            <span>{drinkName(slug)}</span>
          </button>
        {/each}
      </div>
      <CoffeeRecipes {recipes} {drinks} />
    </div>
  {/if}
  {#if picked}
    <!-- A double brews its single's recipe twice: its water is per cup. -->
    <CoffeeBrew slug={picked} recipe={custom ? recipes?.[picked] : null} double={picked.startsWith('two_')} onbrew={brewPicked} onclose={() => (picked = null)} />
  {/if}

  <div class="block">
    <h3>{t('salon.cafe.cockpit')}</h3>
    {#if needs.length}
      <ul class="needs">
        {#each needs as n (n.k)}
          <li>
            <span>{n.label}</span>
            <div class="bar" class:high={n.n >= 80}><i style:width="{Math.min(100, n.n)}%"></i></div>
            <b class="num">{num(n.n)} %</b>
          </li>
        {/each}
      </ul>
    {/if}
    {#if filter}<p class="muted small">{t('salon.cafe.filtre_en_place')}</p>{/if}
    <dl class="counts">
      {#if typeof v('brews_total') === 'number'}
        <div class="total"><dt>{point('brews_total')?.label}</dt><dd class="num">{num(v('brews_total'))}</dd></div>
      {/if}
      {#each counts as c (c.k)}
        <div><dt>{c.label}</dt><dd class="num">{num(c.n)}</dd></div>
      {/each}
    </dl>
    {#if brews.length}
      <details>
        <summary>{t('salon.cafe.par_boisson')}</summary>
        <dl class="counts">
          {#each brews as b (b.k)}
            <div><dt>{b.name}</dt><dd class="num">{num(b.n)}</dd></div>
          {/each}
        </dl>
      </details>
    {/if}
  </div>

  {#if ok && programs.length}
    <div class="block">
      <h3>{t('salon.cafe.entretien')}</h3>
      <p class="hint muted">{t('salon.cafe.entretien_aide')}</p>
      <div class="controls">
        {#each programs as p (p)}
          <button class="act" class:confirm={asked === `program:${p}`} class:waiting={!!why} onclick={() => program(p)}>
            <Icon name="cleaning" size={18} />{asked === `program:${p}` ? t('salon.cafe.lancer_confirmer') : programName(p)}
          </button>
        {/each}
      </div>
    </div>
  {/if}

  {#if ok && settings.length}
    <details class="block">
      <summary>{t('salon.cafe.reglages')}</summary>
      <div class="settings">
        {#each settings as p (p.key)}
          <label>
            <span>{p.label}</span>
            {#if p.kind.type === 'enum'}
              <select value={v(p.key)} onmousedown={hold} onkeydown={hold} onchange={(e) => set(p, e.currentTarget)}>
                {#each p.kind.values as option (option)}
                  <option value={option}>{settingWord(option)}</option>
                {/each}
              </select>
            {:else}
              <span class="number">
                <input type="number" min={p.kind.min} max={p.kind.max} step={p.kind.step ?? 1} value={v(p.key)} onfocus={(e) => why && (e.currentTarget.blur(), explain())} onchange={(e) => set(p, e.currentTarget)} />
                {#if p.unit}<small>{p.unit}</small>{/if}
              </span>
            {/if}
          </label>
        {/each}
      </div>
    </details>
  {/if}

  <dialog bind:this={dialog} onclick={(e) => e.target === dialog && dialog.close()} aria-labelledby="coffee-why-{id}">
    <h3 id="coffee-why-{id}">{t('salon.cafe.indisponible')}</h3>
    <p>{why ? t(`salon.cafe.fenetre.${why}`, { etat: stateWord(status).toLowerCase() }) : t('salon.cafe.fenetre.prete')}</p>
    <button class="act" onclick={() => dialog.close()}>{t('salon.cafe.compris')}</button>
  </dialog>
</section>

<style>
  .coffee {
    display: grid;
    /* A wide child (the recipes' table) scrolls inside, the card keeps its width. */
    grid-template-columns: minmax(0, 1fr);
    gap: 16px;
  }

  .head {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
  }

  .logo {
    width: 44px;
    height: 44px;
    border-radius: 14px;
    display: grid;
    place-items: center;
    background: var(--surface-2);
    color: var(--ink-2);
  }

  [data-state='brewing'] .logo {
    background: var(--warm-soft);
    color: var(--warm-ink);
  }

  .title {
    display: grid;
    flex: 1;
    min-width: 0;
  }

  .title h2 {
    font-size: 18px;
    font-weight: 700;
  }

  .title .muted {
    font-size: 13px;
  }

  h3 {
    margin: 0;
    font-size: 14px;
    font-weight: 700;
    color: var(--ink-2);
  }

  .block {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 10px;
  }

  .hint,
  .small {
    margin: 0;
    font-size: 13px;
  }

  .doing {
    display: grid;
    gap: 8px;
  }

  .doing strong {
    font-size: 26px;
    font-weight: 300;
    letter-spacing: -0.02em;
  }

  .bar {
    height: 10px;
    border-radius: 999px;
    background: var(--surface-2);
    overflow: hidden;
  }

  .bar i {
    display: block;
    height: 100%;
    border-radius: 999px;
    background: var(--warm);
    transition: width 0.8s var(--ease);
  }

  .needs .bar i {
    background: var(--good);
  }

  .needs .bar.high i {
    background: var(--warm);
  }

  .alerts {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .drinks {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 140px), 1fr));
    gap: 8px;
  }

  .drink {
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 52px;
    padding: 6px 12px 6px 8px;
    border: 0;
    border-radius: var(--r-sm);
    background: var(--surface-2);
    color: var(--ink);
    font: inherit;
    font-weight: 600;
    text-align: left;
    cursor: pointer;
  }

  .drink.confirm,
  .act.confirm {
    background: var(--warm);
    color: #fff;
  }

  button:disabled {
    opacity: 0.45;
    cursor: default;
  }

  .needs {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 8px;
  }

  .needs li {
    display: grid;
    grid-template-columns: minmax(0, 9em) minmax(0, 1fr) 3.5em;
    align-items: center;
    gap: 10px;
    font-size: 14px;
  }

  .needs b {
    text-align: right;
  }

  .counts {
    margin: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 170px), 1fr));
    gap: 6px 16px;
  }

  .counts div {
    display: flex;
    justify-content: space-between;
    gap: 8px;
    font-size: 14px;
  }

  .counts dt {
    color: var(--ink-2);
  }

  .counts dd {
    margin: 0;
    font-weight: 650;
  }

  .counts .total {
    grid-column: 1 / -1;
  }

  summary {
    cursor: pointer;
    font-size: 14px;
    font-weight: 700;
    color: var(--ink-2);
  }

  details .counts {
    margin-top: 8px;
  }

  .controls {
    display: flex;
    gap: 10px;
    flex-wrap: wrap;
  }

  .act {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    border: 0;
    border-radius: 999px;
    padding: 10px 18px;
    font: inherit;
    font-weight: 650;
    background: var(--surface-2);
    color: var(--ink);
    cursor: pointer;
  }

  .why {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    justify-self: start;
    border: 0;
    border-radius: var(--r-sm);
    padding: 8px 12px;
    background: var(--surface-2);
    color: var(--ink-2);
    font: inherit;
    font-size: 14px;
    text-align: left;
    cursor: pointer;
  }

  .drink.waiting,
  .act.waiting {
    opacity: 0.45;
  }

  dialog {
    border: 0;
    padding: 20px;
    width: min(420px, 92vw);
    border-radius: var(--r-xl);
    background: var(--surface);
    color: var(--ink);
    box-shadow: 0 30px 80px rgb(0 0 0 / 40%);
  }

  dialog::backdrop {
    background: rgb(4 6 12 / 60%);
    backdrop-filter: blur(4px);
  }

  dialog h3 {
    font-size: 18px;
    color: var(--ink);
  }

  dialog p {
    margin: 12px 0 18px;
    font-size: 15px;
    line-height: 1.45;
  }

  .settings {
    display: grid;
    gap: 10px;
    margin-top: 10px;
  }

  .settings label {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    font-size: 14px;
  }

  .settings select,
  .settings input {
    font: inherit;
    padding: 6px 10px;
    border-radius: var(--r-sm);
    border: 1px solid var(--line);
    background: var(--surface);
    color: var(--ink);
  }

  .settings input {
    width: 5em;
  }

  .number {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
</style>
