<script>
  let { drivers = [] } = $props();
</script>

{#each drivers.filter((driver) => driver.warning) as driver (driver.instance)}
  <p class="warning" role="status"><strong>{driver.instance}</strong> {driver.warning}</p>
{/each}

<style>
  .warning {
    margin: 0;
    padding: 12px 28px;
    background: var(--sun, #f1ce71);
    color: #161512;
    border-bottom: var(--stroke, 1px) solid var(--ink, #161512);
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  strong {
    margin-right: 8px;
    font-size: 12px;
  }
</style>
