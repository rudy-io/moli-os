<script>
  import { hub } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import { isPrinter } from '../lib/printers.js';
  import { isPaperPrinter } from '../lib/paper.js';
  import PrinterCard from '../ui/PrinterCard.svelte';
  import PaperPrinterCard from '../ui/PaperPrinterCard.svelte';

  const printers = $derived(
    Object.values(hub.devices)
      .filter(isPrinter)
      .sort((a, b) => (b.online === true) - (a.online === true) || a.id.localeCompare(b.id)),
  );
  const paper = $derived(Object.values(hub.devices).filter(isPaperPrinter));
</script>

<div class="impression">
  <header>
    <h1 class="page-title">{t('salon.impression.titre')}</h1>
    <p class="page-sub">{t('salon.impression.sous_titre')}</p>
  </header>

  {#if printers.length || paper.length}
    <div class="grid">
      {#each printers as p (p.id)}
        <PrinterCard id={p.id} />
      {/each}
      {#each paper as p (p.id)}
        <PaperPrinterCard id={p.id} />
      {/each}
    </div>
  {:else}
    <p class="muted">{t('salon.impression.aucune')}</p>
  {/if}
</div>

<style>
  .impression {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 20px;
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 440px), 1fr));
    gap: 20px;
    align-items: start;
  }
</style>
