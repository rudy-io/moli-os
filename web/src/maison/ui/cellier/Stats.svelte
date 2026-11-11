<script>
  import Icon from '../Icon.svelte';
  import { t } from '../../../lib/i18n.svelte.js';

  /** Reserved: the page exists in the Cellier's bar, the statistics come later. */
</script>

<section class="card soon">
  <Icon name="chart" size={28} />
  <p class="muted">{t('cellier.stats.bientot')}</p>
</section>

<style>
  .soon {
    display: flex;
    align-items: center;
    gap: 14px;
    color: var(--ink-3);
  }
</style>
