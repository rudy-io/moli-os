<script>
  import { t, locale } from '../../lib/i18n.svelte.js';
  import { retryDate } from '../lib/somfy-cloud.js';
  let { row } = $props();
  const connection = $derived(row?.state);
  // Stored state describes the last result. Only the authoritative binding and
  // current source availability may confirm that its Ready is still current.
  const phase = $derived.by(() => {
    const saved = connection?.phase ?? 'disconnected';
    if (saved !== 'ready') return saved;
    if (row?.session_phase === 'login_required') return 'reconnect_required';
    if (row?.session_phase === 'refreshing') return 'refreshing';
    return row?.session_phase === 'ready' && row.online === true ? 'ready' : 'unconfirmed';
  });
  const retry = $derived(retryDate(connection?.last_attempt_ms, connection?.retry_after_secs, locale()));
</script>

<p role="status">{t(`systeme.somfy.phase.${phase}`)}</p>
{#if connection?.failure}<p class="notice">{t(`systeme.somfy.failure.${connection.failure.kind}`)}</p>{/if}
{#if connection?.retry_after_secs != null}<p class="notice">{retry ? t('systeme.somfy.retry', { date: retry }) : t('systeme.somfy.retry_unknown')}</p>{/if}

<style>
  p { font-size: 13px; margin: 0; overflow-wrap: anywhere; }
  .notice { color: var(--warm-ink); }
</style>
