<script>
  import { onMount } from 'svelte';
  import Icon from '../ui/Icon.svelte';
  import { note } from '../lib/home.svelte.js';
  import { api, asHuman, CANCELLED } from '../lib/auto.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';

  /** An automation Moli drafted in the conversation: what it does, and one
   *  gesture to approve it. */
  let { id } = $props();

  let v = $state(null);
  let failed = $state(false);

  async function load() {
    try {
      v = await api.get(id);
    } catch {
      failed = true;
    }
  }
  onMount(load);

  const errors = $derived((v?.problems ?? []).filter((p) => p.level === 'error'));

  async function approve() {
    try {
      await asHuman(t('automatismes.activer_label', { nom: v.automation.name }), () => api.approve(id, v.fingerprint));
      note(t('automatismes.parti'));
      load();
    } catch (err) {
      if (err.status === 409) {
        note(t('automatismes.carte.change_entre_temps'), 'error');
        load();
      } else if (err.message !== CANCELLED) note(err.message, 'error');
    }
  }
</script>

<section class="card auto">
  {#if failed}
    <p class="muted">{t('automatismes.carte.introuvable')}</p>
  {:else if !v}
    <p class="muted">…</p>
  {:else}
    <div class="card-head">
      <h2><Icon name="robot" size={18} />{v.automation.name}</h2>
      <span class="pill" class:live={v.live}>{v.live ? t('automatismes.etat.live') : t('automatismes.etat.brouillon')}</span>
    </div>
    <p class="summary">{v.summary}</p>
    {#if v.protected?.length}<p class="lock"><Icon name="lock" size={14} />{t('automatismes.carte.agit_dans', { pieces: v.protected.join(', ') })}</p>{/if}
    {#if errors.length}<p class="err">{errors[0].message}</p>{/if}
    <div class="row">
      {#if !v.live}
        <button class="primary" onclick={approve} disabled={errors.length > 0}><Icon name="shield" size={16} />{t('automatismes.carte.valider')}</button>
      {/if}
      <a class="ghost" href="#/automatismes/{id}"><Icon name="branch" size={16} />{t('automatismes.carte.voir')}</a>
    </div>
  {/if}
</section>

<style>
  .auto {
    display: grid;
    gap: 10px;
  }

  .pill {
    padding: 4px 10px;
    border-radius: 999px;
    font-size: 12px;
    font-weight: 700;
    background: var(--warm-soft);
    color: var(--warm-ink);
  }

  .pill.live {
    background: var(--good-soft);
    color: var(--good);
  }

  .summary {
    font-size: 15px;
    line-height: 1.45;
  }

  .lock {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
    color: var(--cool);
    font-weight: 600;
  }

  .err {
    font-size: 13px;
    color: var(--alert);
  }

  .row {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }

  .primary,
  .ghost {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    border: 0;
    border-radius: 999px;
    padding: 9px 16px;
    font-weight: 700;
    text-decoration: none;
  }

  .primary {
    background: var(--ink);
    color: var(--bg);
  }

  .primary:disabled {
    opacity: 0.4;
  }

  .ghost {
    background: var(--surface-2);
    color: var(--ink);
  }
</style>
