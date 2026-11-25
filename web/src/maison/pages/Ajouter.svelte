<script>
  import { onDestroy, tick } from 'svelte';
  import { t } from '../../lib/i18n.svelte.js';
  import { hub } from '../../lib/hub.svelte.js';
  import { home, protectedRoom } from '../lib/home.svelte.js';
  import { probeDevice, addDevice, privateHost } from '../lib/device-add.js';
  import { gatewayApproval } from '../lib/gateway-approval.js';
  import Icon from '../ui/Icon.svelte';

  // address → (key) → found → added. Each answer is for the address and key it
  // was asked with: typing again starts over, so nothing stale is ever added.
  let host = $state('');
  let key = $state('');
  let name = $state('');
  let room = $state('');
  let found = $state(null);
  let added = $state(null);
  let failure = $state(null);
  let pending = $state(null);
  let awaitingPin = $state(false);
  let heading = $state(null);
  let revision = 0;
  let alive = true;
  let reading = null;

  const busy = $derived(pending !== null || awaitingPin);
  const rooms = $derived((home.config?.rooms ?? []).map((r) => r.name));
  const step = $derived(added ? 3 : found?.status === 'recognized' && !found.existing_instance ? 2 : 1);
  const asksKey = $derived(found?.status === 'needs_key');
  const title = $derived(added ? 'ajout.fait.titre' : found?.status === 'recognized' ? 'ajout.trouve.titre' : asksKey ? 'ajout.cle.titre' : 'ajout.adresse.titre');
  const help = $derived(added ? (added.labelled ? 'ajout.fait.aide' : 'ajout.fait.aide_sans_nom') : found?.status === 'recognized' ? 'ajout.trouve.aide' : asksKey ? 'ajout.cle.aide' : 'ajout.adresse.aide');

  const approval = gatewayApproval({
    home,
    label: (intent) => t(intent.kind === 'probe' ? 'ajout.adresse.bouton' : 'ajout.trouve.bouton'),
    waiting: (value) => { awaitingPin = value; },
    failed: (error) => { failure = { error }; },
    current: (intent) => alive && intent.revision === revision,
    perform,
  });

  async function focus() {
    await tick();
    if (alive) heading?.focus();
  }

  function edit(field, value) {
    if (busy) return;
    if (field === 'host') { host = value; found = null; key = ''; }
    if (field === 'key') key = value;
    revision++;
    failure = null;
  }

  async function perform(intent, authenticated = false) {
    if (!alive || intent.revision !== revision) return;
    if (!hub.connected) { failure = { error: 'connection' }; return; }
    pending = intent.kind;
    failure = null;
    reading = intent.kind === 'probe' ? new AbortController() : null;
    const result = intent.kind === 'probe'
      ? await probeDevice({ host: intent.host, key: intent.key }, globalThis.fetch, { signal: reading.signal })
      : await addDevice({ host: intent.host, identity: intent.identity, name: intent.name, room: intent.room, key: intent.key });
    if (!alive || intent.revision !== revision) return;
    pending = null;
    reading = null;
    if (!result.ok) {
      if (result.error === 'auth' && !authenticated) approval.ask(intent);
      else failure = result;
      return;
    }
    if (intent.kind === 'probe') {
      // The server's `key` says how the device is reached (none, empty, known, given); the
      // typed key stays apart, never shown.
      found = { ...result.data, host: intent.host, typedKey: intent.key };
      if (found.status === 'needs_key' && intent.key) failure = { error: 'wrong_key' };
      focus();
    } else {
      added = result.data;
      found = null;
      focus();
    }
  }

  async function look(event) {
    event?.preventDefault();
    if (busy) return;
    const address = privateHost(host);
    if (!address) { failure = { error: 'invalid_host' }; return; }
    await perform({ kind: 'probe', host: address, key: key || undefined, revision });
  }

  async function add(event) {
    event?.preventDefault();
    if (busy || found?.status !== 'recognized' || found.existing_instance) return;
    if (!name.trim()) { failure = { error: 'invalid_name' }; return; }
    await perform({ kind: 'install', host: found.host, identity: found.identity, name: name.trim(), room, key: found.typedKey, revision });
  }

  function again() {
    if (busy) return;
    revision++;
    host = ''; key = ''; name = ''; room = '';
    found = null; added = null; failure = null;
    focus();
  }

  function back() {
    if (busy) return;
    revision++;
    found = null; key = ''; failure = null;
    focus();
  }

  const kindName = (f) => f?.name ?? f?.model ?? '';
  const failureText = (f) => (f?.error ? t(`ajout.erreur.${f.error}`) : '');

  onDestroy(() => {
    alive = false;
    reading?.abort();
    approval.destroy();
  });
</script>

<div class="ajouter">
  <header class="page-head">
    <a class="back-link" href="#/systeme"><Icon name="chevron-left" size={18} />{t('ajout.retour')}</a>
    <h1 class="page-title">{t('ajout.titre')}</h1>
    <p class="page-sub">{t('ajout.sous_titre')}</p>
  </header>

  <section class="card" aria-labelledby="ajout-heading">
    <ol class="steps" aria-label={t('ajout.etapes')}>
      {#each ['adresse', 'nommer', 'fait'] as s, index}
        <li aria-current={step === index + 1 ? 'step' : undefined} class:done={step > index + 1}><span class="step-number">{step > index + 1 ? '✓' : index + 1}</span><span>{t(`ajout.etape.${s}`)}</span></li>
      {/each}
    </ol>
    <header>
      <span class="hero-icon"><Icon name={added ? 'ok' : found?.status === 'recognized' ? 'socket' : asksKey ? 'lock' : 'lan'} size={30} /></span>
      <h2 id="ajout-heading" tabindex="-1" bind:this={heading}>{t(title)}</h2>
      <p>{t(help)}</p>
    </header>

    {#if !hub.connected}<p class="notice" role="status">{t('ajout.erreur.connection')}</p>{/if}

    {#if added}
      <div class="confirmed" role="status">
        <a class="primary-link" href="#/pieces">{t('ajout.fait.voir')}<Icon name="chevron-right" size={18} /></a>
        <button type="button" class="quiet" onclick={again}>{t('ajout.fait.autre')}</button>
      </div>
    {:else if found?.status === 'recognized'}
      <div class="preview">
        <div class="identity"><Icon name="socket" size={26} /><div><h3>{kindName(found)}</h3><p>{t('ajout.trouve.modele', { model: found.model.toUpperCase() })}</p></div></div>
        <details><summary>{t('ajout.trouve.details')}</summary>
          <dl>
            <div><dt>{t('ajout.adresse.champ')}</dt><dd>{found.host}</dd></div>
            {#if found.mac}<div><dt>{t('ajout.trouve.mac')}</dt><dd>{found.mac.match(/../g).join(':')}</dd></div>{/if}
            <div><dt>{t('ajout.trouve.cle')}</dt><dd>{t(`ajout.trouve.cle_${found.key}`)}</dd></div>
          </dl>
        </details>
        {#if found.existing_instance}
          <p class="notice" role="status">{t('ajout.trouve.deja', { instance: found.existing_instance })}</p>
        {:else}
          <form onsubmit={add} novalidate aria-busy={busy}>
            <label>{t('ajout.trouve.nom')}<input bind:value={name} maxlength="60" autocomplete="off" required disabled={busy} placeholder={t('ajout.trouve.nom_exemple')} aria-invalid={failure?.error === 'invalid_name'} /></label>
            <label>{t('ajout.trouve.piece')}
              <select bind:value={room} disabled={busy}>
                <option value="">{t('ajout.trouve.sans_piece')}</option>
                {#each rooms as r (r)}<option value={r}>{r}</option>{/each}
              </select>
            </label>
            {#if room && protectedRoom(room)}<p class="field-help"><Icon name="shield" size={15} />{t('ajout.trouve.protegee')}</p>{/if}
            <button type="submit" class="primary" disabled={busy || !hub.connected}>{t(pending === 'install' ? 'ajout.trouve.en_cours' : 'ajout.trouve.bouton')}</button>
          </form>
          <p class="reassurance"><Icon name="lock" size={15} />{t('ajout.trouve.rien_commande')}</p>
        {/if}
      </div>
    {:else}
      <form onsubmit={look} novalidate aria-busy={busy}>
        <label>{t('ajout.adresse.champ')}<input value={host} oninput={(e) => edit('host', e.currentTarget.value)} autocomplete="off" inputmode="decimal" maxlength="15" required disabled={busy} placeholder="192.168.1.x" aria-invalid={failure?.error === 'invalid_host'} aria-describedby="ajout-host-help" /></label>
        <p class="field-help" id="ajout-host-help">{t('ajout.adresse.ou_trouver')}</p>
        {#if asksKey}
          <label>{t('ajout.cle.champ')}<input type="password" value={key} oninput={(e) => edit('key', e.currentTarget.value)} autocomplete="off" maxlength="128" disabled={busy} aria-describedby="ajout-key-help" /></label>
          <p class="field-help" id="ajout-key-help">{t('ajout.cle.ou_trouver')}</p>
        {/if}
        <button type="submit" class="primary" disabled={busy || !hub.connected}>{t(pending === 'probe' ? 'ajout.adresse.en_cours' : asksKey ? 'ajout.cle.bouton' : 'ajout.adresse.bouton')}</button>
      </form>
      {#if found?.status === 'silent'}<p class="notice" role="status">{t('ajout.resultat.silent')}</p>{/if}
      {#if found?.status === 'unknown'}<p class="notice" role="status">{t('ajout.resultat.unknown')}</p>{/if}
      <p class="reassurance"><Icon name="lock" size={15} />{t('ajout.adresse.lecture_seule')}</p>
    {/if}

    {#if awaitingPin}<p class="notice" role="status">{t('ajout.code_attendu')}</p>{/if}
    {#if failure}<div class="error" role="alert"><p>{failureText(failure)}</p>{#if failure.existingInstance}<p>{t('ajout.trouve.deja', { instance: failure.existingInstance })}</p>{/if}</div>{/if}
    {#if found && !added && !busy}<button class="back quiet" type="button" onclick={back}><Icon name="chevron-left" size={18} />{t('ajout.precedent')}</button>{/if}
  </section>

  <details class="help"><summary>{t('ajout.aide.titre')}</summary>
    <div class="help-content">
      <p>{t('ajout.aide.connus')}</p>
      <p>{t('ajout.aide.autres')}</p>
    </div>
  </details>
</div>

<style>
  .ajouter { display:grid; gap:18px; max-width:720px; margin:0 auto; }
  .back-link { display:inline-flex; align-items:center; gap:6px; min-height:44px; color:var(--ink-2); text-decoration:none; font-size:.875rem; }
  .card { background:var(--surface); border:1px solid var(--line); border-radius:var(--r-lg); padding:32px; box-shadow:var(--shadow); }
  .card > header { margin:26px 0; text-align:center; }
  .card > header p { max-width:46ch; margin:0 auto; }
  .hero-icon { display:inline-grid; place-items:center; width:64px; height:64px; border-radius:20px; background:var(--surface-3); color:var(--cool); margin-bottom:20px; }
  h2 { font-size:clamp(1.4rem,3vw,1.75rem); line-height:1.2; letter-spacing:-.03em; margin:0 0 12px; }
  h2:focus { outline:none; }
  h3 { font-size:1.1rem; margin:0; overflow-wrap:anywhere; }
  p { font-size:1rem; line-height:1.55; color:var(--ink-2); }
  .steps { display:flex; gap:12px; justify-content:space-between; list-style:none; margin:0; padding:0 0 22px; border-bottom:1px solid var(--line); }
  .steps li { display:flex; align-items:center; gap:8px; font-size:.8125rem; color:var(--ink-2); }
  .step-number { display:inline-grid; place-items:center; width:26px; height:26px; border-radius:50%; border:1px solid var(--line); font-size:.75rem; flex:none; }
  .steps li[aria-current] { color:var(--ink); font-weight:650; }
  [aria-current] .step-number { background:var(--cool); border-color:var(--cool); color:var(--paper); }
  .done .step-number { background:var(--surface-3); color:var(--cool); }
  form { display:grid; gap:6px; }
  label { display:grid; gap:8px; font-size:.875rem; font-weight:600; margin-top:10px; }
  input, select { min-width:0; width:100%; background:var(--surface-2); color:var(--ink); border:1px solid var(--line); border-radius:var(--r-sm); padding:12px; min-height:48px; font:inherit; font-size:1rem; }
  .field-help { display:flex; gap:6px; align-items:center; font-size:.8125rem; margin:4px 0 8px; line-height:1.5; }
  button, .primary-link { display:inline-flex; gap:8px; align-items:center; justify-content:center; min-height:48px; padding:12px 18px; border:1px solid transparent; border-radius:var(--r-sm); font:inherit; font-size:.9375rem; font-weight:650; cursor:pointer; background:var(--cool); color:var(--paper); }
  .primary, .primary-link { width:100%; margin-top:12px; }
  .primary-link { text-decoration:none; }
  button.quiet { display:flex; background:transparent; color:var(--ink-2); margin:8px auto 0; padding:10px 12px; font-weight:500; }
  button:disabled, input:disabled, select:disabled { opacity:.55; cursor:not-allowed; }
  :is(button, a, input, select, summary):focus-visible { outline:3px solid var(--cool); outline-offset:3px; }
  .preview { border:1px solid var(--line); border-radius:var(--r-md); padding:22px; background:var(--surface-2); }
  .identity { display:flex; align-items:center; gap:14px; color:var(--cool); }
  .identity h3 { color:var(--ink); }
  .identity p { margin:6px 0 0; }
  summary { cursor:pointer; min-height:44px; display:list-item; padding:12px 0; font-size:.875rem; font-weight:600; color:var(--ink-2); }
  details { margin:12px 0 6px; }
  dl { display:grid; grid-template-columns:repeat(3,minmax(0,1fr)); gap:14px; margin:6px 0 14px; }
  dt { font-size:.75rem; color:var(--ink-2); margin-bottom:5px; }
  dd { margin:0; font-size:.875rem; overflow-wrap:anywhere; }
  .notice, .error { padding:12px 14px; background:var(--surface-2); border-radius:var(--r-sm); border:1px solid var(--line); margin-top:16px; font-size:.875rem; }
  .error { color:var(--alert); }
  .error p { margin:0; color:inherit; font-size:.875rem; }
  .error p + p { margin-top:8px; }
  .reassurance { display:flex; align-items:center; justify-content:center; gap:7px; font-size:.8125rem; margin:18px 0 0; text-align:center; }
  .confirmed { text-align:center; }
  .help { padding:0 16px; margin:0; }
  .help-content p { font-size:.875rem; }
  @media (max-width:600px) {
    .card { padding:22px 18px; }
    .steps { gap:7px; }
    .steps li { gap:5px; font-size:.75rem; }
    dl { grid-template-columns:1fr; }
  }
</style>
