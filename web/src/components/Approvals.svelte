<script>
  import { onMount } from 'svelte';
  import { hub, decide } from '../lib/hub.svelte.js';
  import { t } from '../lib/i18n.svelte.js';

  const WHO = ['mcp', 'api', 'cli', 'assistant'];
  const who = (origin) => (WHO.includes(origin) ? t('systeme.approbations.origine.' + origin) : origin);

  let now = $state(Date.now());
  onMount(() => {
    const timer = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(timer);
  });

  // Name and rooms come from the request itself, frozen when the order was
  // held: renaming the device afterwards cannot disguise what is approved.
  function describe(request) {
    const i = request.point.indexOf('/');
    const device = hub.devices[request.point.slice(0, i)];
    const key = request.point.slice(i + 1);
    const point = device?.points.find((p) => p.key === key);
    let action = `${point?.label ?? key} → ${JSON.stringify(request.value)}`;
    if (point?.kind.type === 'binary') action = request.value ? t('systeme.approbations.allumer') : t('systeme.approbations.eteindre');
    // The source system's name cannot be changed by an agent: always shown.
    const target =
      request.native_name && request.native_name !== request.device_name
        ? `${request.device_name} (${request.native_name})`
        : request.device_name;
    return { target, action, room: request.rooms.join(' / ') };
  }

  function remaining(request) {
    const s = Math.max(0, Math.round((request.expires - now) / 1000));
    return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
  }
</script>

{#if hub.approvals.length}
  <section class="approvals" aria-label={t('systeme.approbations.titre')}>
    {#each hub.approvals as request (request.id)}
      {@const d = describe(request)}
      <article>
        <div class="text">
          <strong>{who(request.origin)}</strong>{#if request.actor} <span class="declared">{t('systeme.approbations.declare', { actor: request.actor })}</span>{/if}
          {t('systeme.approbations.veut')} <strong>{d.action}</strong> · {d.target}{#if d.room} · {d.room}{/if}
          <span class="why">{t('systeme.approbations.retenu', { reason: request.reason })} <span class="num">{remaining(request)}</span></span>
        </div>
        <div class="actions">
          {#if hub.session.human}
            <button class="yes" onclick={() => decide(request, true)}>{t('systeme.approbations.autoriser')}</button>
            <button class="no" onclick={() => decide(request, false)}>{t('systeme.approbations.refuser')}</button>
          {:else}
            <span class="hint">{t('systeme.approbations.indice')}</span>
          {/if}
        </div>
      </article>
    {/each}
  </section>
{/if}

<style>
  .approvals {
    display: grid;
    gap: 0;
    border-bottom: var(--stroke) solid var(--ink);
  }
  article {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    flex-wrap: wrap;
    padding: 12px 28px;
    background: var(--card);
  }
  article + article {
    border-top: 1px solid var(--line);
  }
  .why {
    display: block;
    margin-top: 2px;
    font-size: 13px;
    color: var(--ink-3);
  }
  .actions {
    display: flex;
    gap: 8px;
  }
  button {
    border: var(--stroke) solid var(--ink);
    border-radius: var(--radius);
    padding: 6px 14px;
    font-weight: 700;
    cursor: pointer;
  }
  .yes {
    background: var(--sun);
    color: #161512;
  }
  .no {
    background: transparent;
  }
  .declared {
    color: var(--ink-2);
  }
  .hint {
    font-size: 13px;
    color: var(--ink-2);
  }
  button:focus-visible {
    outline: 2px solid var(--copper);
    outline-offset: 2px;
  }
  @media (max-width: 700px) {
    article {
      padding: 12px 16px;
    }
  }
</style>
