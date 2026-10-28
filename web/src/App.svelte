<script>
  import { onMount } from 'svelte';
  import Maison from './maison/Maison.svelte';
  import Atelier from './Atelier.svelte';
  import { mayLeave } from './lib/leave.js';

  // Two faces, one hub: « Maison » for the family (default), « Atelier »
  // for tinkering with every device. Routes live in the hash.
  const read = () => location.hash.replace(/^#\/?/, '');
  let route = $state(read());

  onMount(() => {
    const update = (e) => {
      const next = read();
      // A page with unsaved work may keep the person on it: the address
      // goes back to it, without a new history entry nor a new event.
      if (next !== route && !mayLeave()) {
        history.replaceState(history.state, '', e.oldURL);
        return;
      }
      route = next;
    };
    addEventListener('hashchange', update);
    return () => removeEventListener('hashchange', update);
  });
</script>

{#if route === 'atelier' || route.startsWith('atelier/')}
  <Atelier />
{:else}
  <Maison {route} />
{/if}
