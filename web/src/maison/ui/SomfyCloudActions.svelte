<script>
  import { t } from '../../lib/i18n.svelte.js';
  let { actions = [], total = actions.length, truncated = false } = $props();
  let expanded = $state(false);
  const commandText = (command) => command.kind === 'unknown'
    ? t('systeme.somfy.programme.native_command', { name: command.name })
    : t(`systeme.somfy.programme.command.${command.kind}`, { value: command.value });
</script>

{#snippet actionRow(action)}
  <li>
    <span class="target">{action.target.label ?? t('systeme.somfy.programme.unidentified_target')}</span>
    {#if !action.target.known && action.target.url}<code>{action.target.url}</code>{/if}
    <ul class="commands">
      {#each action.commands as command}
        <li>{commandText(command)}{#if command.kind === 'unknown' && command.parametersPreview}<code>{command.parametersPreview}</code>{/if}</li>
      {/each}
    </ul>
    {#if !action.commands.length}<small>{t('systeme.somfy.programme.warning.invalid_commands')}</small>{/if}
    {#if action.truncated}<small>{t('systeme.somfy.programme.warning.truncated')}</small>{/if}
  </li>
{/snippet}

<div class="actions">
  <ul>{#each actions.slice(0, 3) as action}{@render actionRow(action)}{/each}</ul>
  {#if actions.length > 3}
    <details ontoggle={(event) => expanded = event.currentTarget.open}>
      <summary>{t('systeme.somfy.programme.more_actions', { count: actions.length - 3 })}</summary>
      {#if expanded}<ul>{#each actions.slice(3) as action}{@render actionRow(action)}{/each}</ul>{/if}
    </details>
  {/if}
  {#if !actions.length}<p>{t('systeme.somfy.programme.no_actions')}</p>{/if}
  {#if truncated || total > actions.length}<small>{t('systeme.somfy.programme.warning.truncated')}</small>{/if}
</div>

<style>
  .actions { min-width: 0; }
  ul { list-style: none; margin: 0; padding: 0; display: grid; gap: 9px; }
  .target { font-weight: 600; }
  li { min-width: 0; overflow-wrap: anywhere; }
  .commands { gap: 3px; margin-top: 2px; color: var(--ink-2); }
  code { display: block; font-size: 11px; overflow-wrap: anywhere; color: var(--ink-2); }
  small, p { color: var(--ink-2); font-size: 12px; }
  details { margin-top: 9px; }
  summary { cursor: pointer; font-size: 13px; color: var(--ink-2); }
  details > ul { margin-top: 10px; }
</style>
