<script>
  import { mdiWeatherNight, mdiWeatherSunny, mdiWeatherCloudy } from '@mdi/js';
  import SegmentedControl from './SegmentedControl.svelte';

  let mode = $state('auto');
  let size = $state('medium');
  let theme = $state('light');

  const modeSegments = [
    { value: 'auto', label: 'Auto' },
    { value: 'manual', label: 'Manuel' },
    { value: 'off', label: 'Off', disabled: false },
  ];

  const sizeSegments = [
    { value: 'small', label: 'S' },
    { value: 'medium', label: 'M' },
    { value: 'large', label: 'L' },
    { value: 'xlarge', label: 'XL' },
  ];

  const themeSegments = [
    {
      value: 'light',
      label: 'Jour',
      icon: `<svg viewBox="0 0 24 24" width="20" height="20"><path fill="currentColor" d="${mdiWeatherSunny}"/></svg>`,
    },
    {
      value: 'auto',
      label: 'Auto',
      icon: `<svg viewBox="0 0 24 24" width="20" height="20"><path fill="currentColor" d="${mdiWeatherCloudy}"/></svg>`,
    },
    {
      value: 'dark',
      label: 'Nuit',
      icon: `<svg viewBox="0 0 24 24" width="20" height="20"><path fill="currentColor" d="${mdiWeatherNight}"/></svg>`,
    },
  ];
</script>

<div class="demo">
  <h2>SegmentedControl Examples</h2>

  <section>
    <h3>Basic</h3>
    <SegmentedControl segments={modeSegments} bind:value={mode} label="Mode de fonctionnement" />
    <p>Selected: <code>{mode}</code></p>
  </section>

  <section>
    <h3>Multiple options</h3>
    <SegmentedControl segments={sizeSegments} bind:value={size} label="Taille" />
    <p>Selected: <code>{size}</code></p>
  </section>

  <section>
    <h3>With icons</h3>
    <SegmentedControl segments={themeSegments} bind:value={theme} label="Thème" />
    <p>Selected: <code>{theme}</code></p>
  </section>

  <section>
    <h3>Compact mode</h3>
    <SegmentedControl segments={themeSegments} bind:value={theme} label="Thème compact" compact />
  </section>
</div>

<style>
  .demo {
    max-width: 600px;
    margin: 0 auto;
    padding: 40px 20px;
    font-family: system-ui, -apple-system, sans-serif;
  }

  h2 {
    margin: 0 0 32px;
    font-size: 24px;
    font-weight: 700;
  }

  h3 {
    margin: 0 0 12px;
    font-size: 16px;
    font-weight: 600;
    color: var(--ink-2, #666);
  }

  section {
    margin-bottom: 40px;
  }

  p {
    margin: 12px 0 0;
    font-size: 14px;
    color: var(--ink-2, #666);
  }

  code {
    padding: 2px 6px;
    background: var(--surface-3, #f0f0f0);
    border-radius: 4px;
    font-family: 'SF Mono', Monaco, monospace;
    font-size: 13px;
  }
</style>
