<script>
  import { onMount } from 'svelte';
  import { hub, home } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import { people, loadPeople, savePerson, removePerson, invite, setPassword, assignPhone, signOut, whereabouts, colorOf, ROLES } from '../lib/people.svelte.js';
  import Icon from './Icon.svelte';
  import Login from './Login.svelte';

  /** Système › Personnes: who lives here, what each may do, their phones.
   *  An owner adds, invites, changes and removes; everyone sees who is
   *  home and changes their own password. */
  onMount(loadPeople);

  let editing = $state(null); // null, 'new' or a person id
  let draft = $state(empty());
  let invited = $state(null); // { name, code, expires }
  let message = $state('');
  let busy = $state(false);
  let newPassword = $state('');
  let passwordDone = $state(false);
  let signingIn = $state(false);

  function empty() {
    return { name: '', role: 'member', emails: '', rooms: [], expires: '' };
  }

  const rooms = $derived((home.config?.rooms ?? []).map((r) => r.name));
  // Signed in with a password (Access signs out by itself).
  const signedIn = $derived(!!hub.session?.person && hub.session?.via !== 'access');

  function edit(p) {
    editing = p ? p.id : 'new';
    invited = null;
    message = '';
    draft = p
      ? { name: p.name, role: p.role, emails: (p.emails ?? []).join(', '), rooms: [...(p.rooms ?? [])], expires: p.expires ? new Date(p.expires).toISOString().slice(0, 10) : '' }
      : empty();
  }

  async function save(e) {
    e.preventDefault();
    busy = true;
    message = '';
    try {
      const body = {
        name: draft.name,
        role: draft.role,
        emails: draft.emails.split(/[\s,;]+/).filter(Boolean),
        rooms: draft.role === 'guest' ? draft.rooms : [],
        expires: draft.role === 'guest' && draft.expires ? new Date(`${draft.expires}T23:59:59`).getTime() : null,
      };
      await savePerson(editing === 'new' ? null : editing, body);
      editing = null;
    } catch (err) {
      message = err.message;
    } finally {
      busy = false;
    }
  }

  async function sendInvite(p) {
    message = '';
    try {
      const { code, expires } = await invite(p.id);
      invited = { name: p.name, code, expires };
    } catch (err) {
      message = err.message;
    }
  }

  async function remove(p) {
    if (!confirm(t('personnes.carte.retirer_confirmer', { name: p.name }))) return;
    try {
      await removePerson(p.id);
    } catch (err) {
      message = err.message;
    }
  }

  async function changePassword(e) {
    e.preventDefault();
    passwordDone = false;
    message = '';
    try {
      await setPassword(people.me.id, newPassword);
      newPassword = '';
      passwordDone = true;
    } catch (err) {
      message = err.message;
    }
  }

  const phonesOf = (id) => people.phones.filter((ph) => ph.person === id);
  const unassigned = $derived(people.phones.filter((ph) => !ph.person));
  const date = (ms) => new Date(ms).toLocaleDateString(undefined, { day: 'numeric', month: 'long' });

  function placeOf(p) {
    const w = whereabouts(p.id);
    if (w.home === true) return t('personnes.carte.a_la_maison');
    return w.zone ?? t('personnes.carte.inconnu');
  }
</script>

<section class="card people">
  <header>
    <span class="icon"><Icon name="people" size={20} /></span>
    <div class="what">
      <b>{t('personnes.carte.titre')}</b>
      <span class="muted">
        {#if people.me}{t('personnes.carte.connecte', { name: people.me.name })}{:else}{t('personnes.carte.intro')}{/if}
      </span>
    </div>
    {#if people.owner}
      <button class="primary" onclick={() => edit(null)}><Icon name="plus" size={18} />{t('personnes.carte.ajouter')}</button>
    {/if}
    {#if signedIn}
      <button class="ghost" onclick={signOut}><Icon name="logout" size={18} />{t('personnes.carte.deconnexion')}</button>
    {:else if !people.me && people.list.length}
      <button class="ghost" onclick={() => (signingIn = true)}><Icon name="account" size={18} />{t('personnes.carte.connexion')}</button>
    {/if}
  </header>

  {#if people.error}<p class="error">{people.error}</p>{/if}
  {#if message}<p class="error" role="alert">{message}</p>{/if}

  <ul class="list">
    {#each people.list as p (p.id)}
      <li>
        <span class="dot" style:background={colorOf(p)}>{p.name.slice(0, 1).toUpperCase()}</span>
        <div class="who">
          <b>{p.name}</b>
          <span class="muted">
            {t(`personnes.role.${p.role}`)} · {placeOf(p)}
            {#if p.role === 'guest' && p.expires} · {t('personnes.carte.jusqu_au', { date: date(p.expires) })}{/if}
          </span>
          {#if phonesOf(p.id).length}
            <span class="phones">{#each phonesOf(p.id) as ph (ph.id)}<span class="chip"><Icon name="phone" size={14} />{ph.name}</span>{/each}</span>
          {/if}
        </div>
        {#if people.owner}
          <div class="actions">
            <button class="ghost small" onclick={() => sendInvite(p)}>{p.has_password ? t('personnes.carte.reinviter') : t('personnes.carte.inviter')}</button>
            <button class="ghost small" onclick={() => edit(p)}>{t('personnes.carte.modifier')}</button>
            <button class="ghost small danger" onclick={() => remove(p)} aria-label={t('personnes.carte.retirer')}><Icon name="trash" size={16} /></button>
          </div>
        {/if}
      </li>
    {:else}
      {#if people.loaded}<li class="muted">{t('personnes.carte.personne')}</li>{/if}
    {/each}
  </ul>

  {#if invited}
    <div class="invite" role="status">
      <b>{t('personnes.carte.invitation_pour', { name: invited.name })}</b>
      <code>{invited.code}</code>
      <span>{t('personnes.carte.invitation_aide', { date: date(invited.expires) })}</span>
      {#if hub.session?.via === 'access'}<span class="muted">{t('personnes.carte.invitation_access')}</span>{/if}
      <button class="ghost small" onclick={() => (invited = null)}>{t('personnes.carte.fermer')}</button>
    </div>
  {/if}

  {#if editing}
    <form class="edit" onsubmit={save}>
      <label>
        <span>{t('personnes.carte.prenom')}</span>
        <input bind:value={draft.name} maxlength="40" required />
      </label>
      <label>
        <span>{t('personnes.carte.role')}</span>
        <select bind:value={draft.role}>
          {#each ROLES as r (r)}<option value={r}>{t(`personnes.role.${r}`)}</option>{/each}
        </select>
        <small>{t(`personnes.role.${draft.role}_aide`)}</small>
      </label>
      <label>
        <span>{t('personnes.carte.emails')}</span>
        <input bind:value={draft.emails} type="text" autocomplete="off" spellcheck="false" placeholder="prenom@exemple.fr" />
        <small>{t('personnes.carte.emails_aide')}</small>
      </label>
      {#if draft.role === 'guest'}
        <fieldset>
          <legend>{t('personnes.carte.pieces')}</legend>
          <div class="rooms">
            {#each rooms as r (r)}
              <label class="check"><input type="checkbox" value={r} bind:group={draft.rooms} />{r}</label>
            {/each}
          </div>
        </fieldset>
        <label>
          <span>{t('personnes.carte.fin')}</span>
          <input bind:value={draft.expires} type="date" />
        </label>
      {/if}
      <div class="row">
        <button class="ghost" type="button" onclick={() => (editing = null)}>{t('personnes.carte.annuler')}</button>
        <button class="primary" type="submit" disabled={busy || !draft.name.trim()}>{t('personnes.carte.enregistrer')}</button>
      </div>
    </form>
  {/if}

  {#if people.owner && unassigned.length}
    <div class="unassigned">
      <b>{t('personnes.carte.telephones_libres')}</b>
      {#each unassigned as ph (ph.id)}
        <label class="phone-row">
          <span><Icon name="phone" size={16} />{ph.name} <span class="muted">{ph.model}</span></span>
          <select onchange={(e) => assignPhone(ph.id, e.currentTarget.value)}>
            <option value="">{t('personnes.carte.a_personne')}</option>
            {#each people.list as p (p.id)}<option value={p.id}>{p.name}</option>{/each}
          </select>
        </label>
      {/each}
    </div>
  {/if}

  {#if signedIn && people.me}
    <form class="password" onsubmit={changePassword}>
      <label>
        <span>{t('personnes.carte.mon_mot_de_passe')}</span>
        <input bind:value={newPassword} type="password" autocomplete="new-password" placeholder={t('personnes.connexion.min', { min: 10 })} />
      </label>
      <button class="ghost" type="submit" disabled={newPassword.length < 10}>{t('personnes.carte.changer')}</button>
      {#if passwordDone}<span class="good">{t('personnes.carte.mot_de_passe_change')}</span>{/if}
    </form>
  {/if}
</section>

{#if signingIn}
  <Login onclose={() => (signingIn = false)} />
{/if}

<style>
  .people {
    display: grid;
    gap: 14px;
  }

  header {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 12px 16px;
  }

  .icon {
    display: grid;
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
    min-width: 200px;
    gap: 2px;
    font-size: 14px;
  }

  .list {
    display: grid;
    gap: 6px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .list li {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px 12px;
    padding: 10px 12px;
    border-radius: 16px;
    background: var(--surface-2);
  }

  .dot {
    display: grid;
    place-items: center;
    width: 36px;
    height: 36px;
    border-radius: 50%;
    color: #fff;
    font-weight: 750;
    flex: none;
  }

  .who {
    display: grid;
    flex: 1;
    min-width: 180px;
    gap: 2px;
    font-size: 14px;
  }

  .phones {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 4px;
  }

  .chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 2px 8px;
    border-radius: 999px;
    background: var(--surface-3);
    font-size: 12px;
  }

  .actions {
    display: flex;
    gap: 6px;
  }

  .invite {
    display: grid;
    gap: 6px;
    padding: 14px;
    border-radius: 16px;
    background: var(--cool-soft);
    font-size: 14px;
  }

  .invite code {
    font-size: 26px;
    font-weight: 750;
    letter-spacing: 0.12em;
  }

  .edit,
  .unassigned,
  .password {
    display: grid;
    gap: 12px;
    padding: 14px;
    border-radius: 16px;
    background: var(--surface-2);
  }

  label,
  fieldset {
    display: grid;
    gap: 6px;
    font-size: 14px;
    font-weight: 650;
  }

  fieldset {
    margin: 0;
    padding: 0;
    border: none;
  }

  .rooms {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 14px;
  }

  .check {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-weight: 500;
  }

  input:not([type='checkbox']),
  select {
    min-height: 42px;
    padding: 0 12px;
    border: 1px solid var(--surface-3);
    border-radius: 12px;
    background: var(--surface);
    color: var(--ink);
    font: inherit;
  }

  small {
    color: var(--ink-3);
    font-weight: 500;
  }

  .row {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }

  .phone-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    font-weight: 500;
  }

  .phone-row span {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  .primary,
  .ghost {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-height: 40px;
    padding: 0 16px;
    border-radius: 999px;
    font: inherit;
    font-weight: 650;
    cursor: pointer;
  }

  .primary {
    border: none;
    background: var(--cool);
    color: #fff;
  }

  .ghost {
    border: 1px solid var(--surface-3);
    background: var(--surface);
    color: var(--ink);
  }

  .small {
    min-height: 34px;
    padding: 0 12px;
    font-size: 13px;
  }

  .danger {
    color: var(--alert);
  }

  button:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .error {
    color: var(--alert);
    font-size: 14px;
    font-weight: 600;
  }

  .good {
    color: var(--good);
    font-weight: 650;
  }
</style>
