<script>
  import { NAV, filledPages } from '../lib/pages.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from './Icon.svelte';

  /** Every page of the dashboard, shown or not: what it is for, and for a
   *  page out of the menu, what would bring it back (one may exist already). */
  const filled = $derived(filledPages());
  const pages = $derived(
    NAV.map((p) => {
      const key = p.path || 'accueil';
      return { ...p, key, name: p.name ?? t(p.label), shown: filled[p.path] ?? true, depends: p.path in filled };
    }),
  );
  const hidden = $derived(pages.filter((p) => !p.shown).length);
</script>

<section class="card pages">
  <div class="card-head">
    <h2><Icon name="dots" size={18} />{t('systeme.pages.titre')}</h2>
    <span class="muted">{t('systeme.pages.compte', { shown: pages.length - hidden, count: pages.length })}</span>
  </div>
  <p class="muted intro">{t('systeme.pages.aide')}</p>
  <ul>
    {#each pages as p (p.key)}
      <li class:off={!p.shown}>
        <a href="#/{p.path}" class="ic"><Icon name={p.icon ?? 'sparkles'} size={18} /></a>
        <span class="text">
          <b>{p.name} <span class="chip {p.shown ? 'good' : ''}">{p.shown ? t('systeme.pages.dans_menu') : t('systeme.pages.masquee')}</span></b>
          <small>{t(`systeme.pages.role.${p.key}`)}</small>
          {#if p.depends}<small class="when">{t(`systeme.pages.quand.${p.key}`)}</small>{/if}
        </span>
      </li>
    {/each}
  </ul>
</section>

<style>
  .card-head {
    margin-bottom: 6px;
  }

  .intro {
    font-size: 13.5px;
    margin-bottom: 12px;
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(300px, 1fr));
    gap: 8px 20px;
  }

  li {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    gap: 10px;
    align-items: start;
    padding: 6px 0;
  }

  .ic {
    width: 32px;
    height: 32px;
    border-radius: 10px;
    display: grid;
    place-items: center;
    background: var(--surface-2);
    color: var(--ink-2);
  }

  li.off .ic {
    color: var(--ink-3);
  }

  .text {
    display: grid;
    gap: 2px;
  }

  b {
    font-size: 14px;
    font-weight: 650;
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }

  .chip {
    padding: 2px 8px;
    font-size: 11.5px;
  }

  small {
    font-size: 12.5px;
    color: var(--ink-3);
    line-height: 1.35;
  }

  .when {
    font-style: italic;
  }
</style>
