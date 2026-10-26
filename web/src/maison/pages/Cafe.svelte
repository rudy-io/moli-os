<script>
  import { hub } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import { isCoffee } from '../lib/coffee.js';
  import CoffeeCard from '../ui/CoffeeCard.svelte';
  import CoffeeUsage from '../ui/CoffeeUsage.svelte';

  const machines = $derived(Object.values(hub.devices).filter(isCoffee).sort((a, b) => a.id.localeCompare(b.id)));
</script>

<div class="cafe">
  <header>
    <h1 class="page-title">{t('salon.cafe.titre')}</h1>
    <p class="page-sub">{t('salon.cafe.sous_titre')}</p>
  </header>

  {#if machines.length}
    {#each machines as m (m.id)}
      <CoffeeCard id={m.id} />
      <CoffeeUsage id={m.id} />
    {/each}
  {:else}
    <p class="muted">{t('salon.cafe.aucune')}</p>
  {/if}
</div>

<style>
  .cafe {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 16px;
  }
</style>
