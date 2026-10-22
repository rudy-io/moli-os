<script>
  import { hub } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import PeopleCard from '../ui/PeopleCard.svelte';
  import SessionsCard from '../ui/SessionsCard.svelte';

  /** Famille: who lives here and who is invited, what each may do and
   *  reach; for the person signed in, their own connections. */
  const signedIn = $derived(!!hub.session?.person && hub.session?.via !== 'access');
</script>

<div class="famille">
  <header>
    <h1 class="page-title">{t('personnes.page.titre')}</h1>
    <p class="page-sub">{t('personnes.page.sous_titre')}</p>
  </header>

  <PeopleCard />

  {#if signedIn}
    <SessionsCard />
  {/if}
</div>

<style>
  .famille {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 20px;
  }

  .famille > :global(*) {
    min-width: 0;
  }
</style>
