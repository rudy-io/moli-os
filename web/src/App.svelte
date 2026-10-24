<script>
  import { onMount } from 'svelte';
  import Maison from './maison/Maison.svelte';

  // One dashboard, its rail on every page, the Atelier (every device, to
  // tinker with) among them. Routes live in the hash.
  const read = () => location.hash.replace(/^#\/?/, '');
  let route = $state(read());

  onMount(() => {
    const update = () => (route = read());
    addEventListener('hashchange', update);
    return () => removeEventListener('hashchange', update);
  });
</script>

<Maison {route} />
