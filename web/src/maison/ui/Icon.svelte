<script>
  import { icon } from '../lib/icons.js';

  /** `name` from the icon set, or a raw `path`. */
  let { name = null, path = null, size = 24, label = null } = $props();
  const d = $derived(path ?? icon(name));
</script>

<svg
  viewBox="0 0 24 24"
  width={size}
  height={size}
  role={label ? 'img' : undefined}
  aria-label={label ?? undefined}
  aria-hidden={label ? undefined : 'true'}
  focusable="false"><path {d} fill="currentColor" /></svg>

<style>
  svg {
    flex: none;
    display: block;
  }
</style>
