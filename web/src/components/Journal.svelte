<script>
  import { onMount } from 'svelte';
  import { hub } from '../lib/hub.svelte.js';
  import { ago, deviceName } from '../lib/format.js';
  import { t } from '../lib/i18n.svelte.js';

  const ORIGIN = ['ui', 'api', 'mcp', 'cli', 'system', 'assistant', 'automation'];
  const CHANGE = ['created', 'updated', 'approved', 'switched_off', 'restored', 'deleted'];
  const VERDICT = ['approved', 'denied', 'expired'];
  const word = (table, area, id) => (table.includes(id) ? t(`systeme.journal.${area}.${id}`) : id);

  let now = $state(Date.now());
  onMount(() => {
    const timer = setInterval(() => (now = Date.now()), 15000);
    return () => clearInterval(timer);
  });

  function describe(entry) {
    const a = entry.action;
    if (a.kind === 'command') {
      const [id, key] = [a.point.slice(0, a.point.indexOf('/')), a.point.slice(a.point.indexOf('/') + 1)];
      const d = hub.devices[id];
      return { target: d ? deviceName(d) : id, detail: `${key} → ${JSON.stringify(a.value)}` };
    }
    if (a.kind === 'automation') {
      return { target: t('systeme.journal.automatisme', { name: a.name }), detail: word(CHANGE, 'changement', a.change) };
    }
    if (a.kind === 'approval') {
      const id = a.point.slice(0, a.point.indexOf('/'));
      const d = hub.devices[id];
      const verdict = word(VERDICT, 'verdict', a.decision);
      return { target: d ? deviceName(d) : id, detail: t('systeme.journal.demande', { request: a.request, verdict }) };
    }
    const d = hub.devices[a.device];
    const parts = [a.label.name, a.label.room].filter(Boolean).join(' · ');
    return { target: d ? deviceName(d) : a.device, detail: t('systeme.journal.etiquette', { value: parts || t('systeme.journal.effacee') }) };
  }
</script>

<aside>
  <h2>{t('systeme.journal.titre')}</h2>
  {#if hub.journal.length === 0}
    <p class="empty">{t('systeme.journal.vide')}</p>
  {/if}
  <ol>
    {#each hub.journal as entry (entry.id)}
      {@const d = describe(entry)}
      <li class:err={entry.outcome.result === 'err'} class:held={entry.outcome.result === 'pending'}>
        <div class="line">
          <strong>{d.target}</strong>
          <time class="num" datetime={new Date(entry.ts).toISOString()}>{ago(entry.ts, now)}</time>
        </div>
        <div class="mono detail">{d.detail}</div>
        <div class="who">
          {word(ORIGIN, 'origine', entry.origin)}{#if entry.actor} · {entry.actor}{/if}
          {#if entry.outcome.result === 'err'}<span class="error"> · {entry.outcome.error}</span>{/if}
          {#if entry.outcome.result === 'pending'}<span class="pending"> · {entry.outcome.error}</span>{/if}
        </div>
      </li>
    {/each}
  </ol>
</aside>

<style>
  aside {
    align-self: start;
    position: sticky;
    top: 16px;
  }
  h2 {
    font-size: 13px;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    margin: 0 0 12px;
  }
  .empty {
    color: var(--ink-3);
    font-size: 13.5px;
  }
  ol {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 8px;
    max-height: calc(100vh - 140px);
    overflow-y: auto;
  }
  li {
    padding: 10px 12px;
    background: var(--card);
    border: 1.5px solid var(--line);
    border-radius: var(--radius);
    font-size: 13px;
  }
  li.err {
    border-color: var(--alarm);
  }
  li.held {
    border-color: var(--copper);
  }
  .pending {
    color: var(--copper);
  }
  .line {
    display: flex;
    justify-content: space-between;
    gap: 8px;
  }
  time {
    color: var(--ink-3);
    white-space: nowrap;
  }
  .detail {
    margin: 2px 0;
    color: var(--ink-2);
    word-break: break-all;
  }
  .who {
    color: var(--ink-3);
    font-size: 12px;
  }
  .error {
    color: var(--alarm);
  }
</style>
