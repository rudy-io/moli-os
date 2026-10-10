<script>
  import { hub, roomOf, hidden, nameOf, act } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from '../ui/Icon.svelte';

  // Every device that can say a sentence: a writable text point `say`, or
  // `announce` for those that only announce (a Sonos plays a clip over the
  // music). New integrations with such a point show up here by themselves.
  const SPEAK = ['say', 'announce'];
  /** The shortest limit among the speakers (an Echo says 250 characters). */
  const MAX = 250;
  const KEEP = 6;

  const speakers = $derived(
    Object.values(hub.devices ?? {})
      .filter((d) => !hidden(d.id))
      .map((d) => {
        const key = SPEAK.find((k) => d.points?.some((p) => p.key === k && p.access?.write && p.kind?.type === 'text'));
        if (!key) return null;
        // Two « Salon » (an Echo, a Sonos) are told apart by what they are.
        const what = [d.manufacturer, d.model].filter(Boolean).join(' ');
        return { id: d.id, key, name: nameOf(d.id), what, room: roomOf(d), online: d.online !== false };
      })
      .filter(Boolean)
      .sort((a, b) => (a.room ?? '￿').localeCompare(b.room ?? '￿') || a.name.localeCompare(b.name)),
  );
  const rooms = $derived(
    Object.entries(
      speakers.reduce((by, s) => {
        (by[s.room ?? ''] ??= []).push(s);
        return by;
      }, {}),
    ),
  );

  function load(key, fallback) {
    try {
      return JSON.parse(localStorage.getItem(key)) ?? fallback;
    } catch {
      return fallback;
    }
  }
  function keep(key, val) {
    try {
      localStorage.setItem(key, JSON.stringify(val));
    } catch {
      // Private window: the choice lasts the visit.
    }
  }

  let chosen = $state(load('parler-cibles', []));
  let recent = $state(load('parler-phrases', []));
  let text = $state('');
  let busy = $state(false);
  /** id → 'ok' | 'held' | 'error', for the last sentence. */
  let results = $state({});

  const picked = $derived(speakers.filter((s) => chosen.includes(s.id)));
  const ready = $derived(text.trim().length > 0 && text.length <= MAX && picked.some((s) => s.online) && !busy);

  function toggle(id) {
    chosen = chosen.includes(id) ? chosen.filter((c) => c !== id) : [...chosen, id];
    keep('parler-cibles', chosen);
  }

  async function say(e) {
    e?.preventDefault();
    if (!ready) return;
    busy = true;
    results = {};
    const sentence = text.trim();
    const targets = picked.filter((s) => s.online);
    const outcomes = await Promise.all(
      targets.map((s) => act(`${s.id}/${s.key}`, sentence, t('parler.dire_sur', { nom: s.name }))),
    );
    results = Object.fromEntries(targets.map((s, i) => [s.id, outcomes[i]]));
    recent = [sentence, ...recent.filter((r) => r !== sentence)].slice(0, KEEP);
    keep('parler-phrases', recent);
    busy = false;
  }

  function onkey(e) {
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) say(e);
  }
</script>

<div class="parler">
  <header>
    <h1 class="page-title">{t('parler.titre')}</h1>
    <p class="page-sub">{t('parler.sous_titre')}</p>
  </header>

  <form class="card compose" onsubmit={say}>
    <label for="parler-texte" class="sr">{t('parler.texte')}</label>
    <textarea
      id="parler-texte"
      bind:value={text}
      onkeydown={onkey}
      rows="3"
      maxlength={MAX}
      placeholder={t('parler.placeholder')}
    ></textarea>
    <div class="row">
      <span class="muted count" class:over={text.length > MAX - 20}>{text.length} / {MAX}</span>
      <button class="btn primary" disabled={!ready}>
        <Icon name="send" size={18} />
        {busy ? t('parler.envoi') : picked.length > 1 ? t('parler.dire_partout', { n: picked.length }) : t('parler.dire')}
      </button>
    </div>
    {#if recent.length}
      <div class="recent">
        {#each recent as r (r)}
          <button type="button" class="chip" onclick={() => (text = r)} title={t('parler.reprendre')}>{r}</button>
        {/each}
      </div>
    {/if}
  </form>

  <section class="targets">
    <h2>{t('parler.ou')}</h2>
    {#each rooms as [room, list] (room)}
      <div class="room">
        <h3>{room || t('parler.sans_piece')}</h3>
        <div class="list">
          {#each list as s (s.id)}
            <button
              type="button"
              class="target"
              class:on={chosen.includes(s.id)}
              disabled={!s.online}
              aria-pressed={chosen.includes(s.id)}
              onclick={() => toggle(s.id)}
            >
              <Icon name={s.key === 'announce' ? 'speaker' : 'talk'} size={18} />
              <span class="name">{s.name}<small class="muted">{s.what}</small></span>
              {#if !s.online}
                <span class="muted small">{t('parler.hors_ligne')}</span>
              {:else if results[s.id] === 'ok'}
                <span class="small ok"><Icon name="ok" size={14} />{t('parler.envoye')}</span>
              {:else if results[s.id] === 'held'}
                <span class="small"><Icon name="lock" size={14} />{t('parler.en_attente')}</span>
              {:else if results[s.id] === 'error'}
                <span class="small ko"><Icon name="ko" size={14} />{t('parler.echec')}</span>
              {/if}
            </button>
          {/each}
        </div>
      </div>
    {:else}
      <p class="muted">{t('parler.aucun')}</p>
    {/each}
  </section>
</div>

<style>
  .parler {
    display: grid;
    gap: 22px;
    max-width: 920px;
  }

  .compose {
    display: grid;
    gap: 12px;
    padding: 16px 18px;
    border-radius: var(--r-md);
    background: var(--surface-2);
  }

  textarea {
    font: inherit;
    font-size: 1.1rem;
    resize: vertical;
    min-height: 84px;
    padding: 12px 14px;
    border-radius: 12px;
    border: 1px solid var(--line, rgba(0, 0, 0, 0.15));
    background: var(--surface);
    color: inherit;
  }

  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
  }

  .count.over {
    color: var(--danger, #b3261e);
  }

  .btn {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    border: 0;
    border-radius: 999px;
    padding: 10px 18px;
    font: inherit;
    font-weight: 650;
    cursor: pointer;
  }

  .btn.primary {
    background: var(--warm);
    color: #fff;
  }

  .btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .recent {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .recent .chip {
    border: 0;
    border-radius: 999px;
    padding: 6px 12px;
    font: inherit;
    font-size: 0.9rem;
    background: var(--surface);
    color: inherit;
    cursor: pointer;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .targets {
    display: grid;
    gap: 16px;
  }

  .targets h2 {
    margin: 0;
    font-size: 1.1rem;
  }

  .room h3 {
    margin: 0 0 8px;
    font-size: 0.85rem;
    font-weight: 650;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    opacity: 0.7;
  }

  .list {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(100%, 220px), 1fr));
    gap: 10px;
  }

  .target {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 12px 14px;
    border-radius: 14px;
    border: 2px solid transparent;
    background: var(--surface-2);
    color: inherit;
    font: inherit;
    text-align: left;
    cursor: pointer;
    min-width: 0;
  }

  .target.on {
    border-color: var(--warm);
    background: var(--warm-soft);
  }

  .target:disabled {
    opacity: 0.55;
    cursor: default;
  }

  .target .name {
    flex: 1;
    min-width: 0;
    display: grid;
  }

  .target .name,
  .target .name small {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .target .name small {
    font-size: 0.78rem;
  }

  .small {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font-size: 0.8rem;
  }

  .ok {
    color: var(--ok, #2e7d32);
  }

  .ko {
    color: var(--danger, #b3261e);
  }

  .btn:focus-visible,
  .target:focus-visible,
  .recent .chip:focus-visible,
  textarea:focus-visible {
    outline: 2px solid currentColor;
    outline-offset: 2px;
  }

  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
  }
</style>
