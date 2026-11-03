<script>
  import { onMount } from 'svelte';
  import { hub } from './lib/hub.svelte.js';
  import Maison from './maison/Maison.svelte';

  // Two faces, one shell: « Maison » for the family (default), « Atelier »
  // (a page of it) for tinkering with every device. Routes live in the hash.
  const read = () => location.hash.replace(/^#\/?/, '');
  let route = $state(read());

  onMount(() => {
    const update = () => (route = read());
    addEventListener('hashchange', update);
    return () => removeEventListener('hashchange', update);
  });

  // The Atelier is for owners: anyone else lands back on the house.
  const atelier = $derived(route === 'atelier' || route.startsWith('atelier/'));
  const allowed = $derived(hub.session.atelier !== false);
  $effect(() => {
    if (atelier && !allowed) location.replace('#/');
  });
</script>

<Maison route={atelier && !allowed ? '' : route} />
