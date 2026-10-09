<script>
  import { home, value, reachable, nameOf, act, since, clock } from '../lib/home.svelte.js';
  import { t } from '../../lib/i18n.svelte.js';
  import Icon from '../ui/Icon.svelte';
  import Remote from '../ui/Remote.svelte';
  import LightTile from '../ui/LightTile.svelte';
  import SpeakerCard from '../ui/SpeakerCard.svelte';
  import AmbiancePicker from '../ui/AmbiancePicker.svelte';

  const s = $derived(home.config?.salon ?? {});
  const tv = $derived(s.tv);
  const sp = $derived(s.speaker);
  const tvOn = $derived(value(tv, 'power') === true);
  const app = $derived(value(tv, 'app'));

  const APPS = {
    'com.netflix.ninja': 'Netflix',
    'com.google.android.youtube.tv': 'YouTube',
    'org.jellyfin.androidtv': 'Jellyfin',
    'com.disney.disneyplus': 'Disney+',
    'com.amazon.amazonvideo.livingroom': 'Prime Video',
    'com.limelight': 'Moonlight',
    'org.droidtv.playtv': 'salon.app.chaines',
    'com.google.android.tvlauncher': 'salon.app.accueil',
    'com.google.android.apps.tv.launcherx': 'salon.app.accueil',
  };
  // The brands keep their name; the other apps are words (keys, translated when shown).
  const appLabel = (pkg) => (APPS[pkg]?.startsWith('salon.') ? t(APPS[pkg]) : APPS[pkg]);
  // The ones to open from here (the TV's own remote does the rest).
  const LAUNCH = ['com.netflix.ninja', 'com.google.android.youtube.tv', 'com.disney.disneyplus', 'com.amazon.amazonvideo.livingroom', 'com.limelight', 'org.droidtv.playtv'];
  const appName = $derived(app ? (appLabel(app) ?? t('salon.une_application')) : null);

  // What plays, as Cast tells it (title, picture, progress).
  const title = $derived(value(tv, 'media_title'));
  const subtitle = $derived(value(tv, 'media_subtitle'));
  const image = $derived(value(tv, 'media_image'));
  const castApp = $derived(value(tv, 'media_app'));
  const state = $derived(value(tv, 'media_state'));
  const duration = $derived(value(tv, 'media_duration'));
  // The position was told at some moment: while it plays, it moves on.
  const position = $derived.by(() => {
    const p = value(tv, 'media_position');
    if (typeof p !== 'number') return null;
    const told = since(tv, 'media_position') ?? home.now;
    const moved = state === 'playing' ? Math.max(0, (home.now - told) / 1000) : 0;
    return duration ? Math.min(duration, p + moved) : p + moved;
  });
  const mmss = (sec) => {
    const s = Math.round(sec);
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    return h ? `${h} h ${String(m).padStart(2, '0')}` : `${m} min`;
  };
  const playing = $derived(state === 'playing' || state === 'buffering');
  let busy = $state(null);
  async function open(pkg) {
    busy = pkg;
    await act(`${tv}/app`, pkg, t('salon.ouvrir', { app: appLabel(pkg) ?? t('salon.l_appli') }));
    busy = null;
  }
  function playPause() {
    act(`${tv}/media_control`, playing ? 'pause' : 'play', playing ? t('salon.pause') : t('salon.lecture'));
  }
</script>

<div class="salon">
  <header class="head">
    <h1 class="page-title">{t('salon.titre')}</h1>
    <p class="page-sub">{t('salon.sous_titre')}</p>
  </header>

  <div class="layout">
    <div class="remote-col">
      {#if tv}<Remote {tv} />{/if}
    </div>

    <div class="now-col">
      <section class="card now" class:lit={tvOn}>
        {#if tvOn && title}
          <div class="playing">
            {#if image}<img src={image} alt="" referrerpolicy="no-referrer" />{/if}
            <div class="what">
              <span class="eyebrow">{castApp ?? appName ?? t('salon.ecran')}{state === 'paused' ? ` · ${t('salon.en_pause')}` : ''}</span>
              <h2>{title}</h2>
              {#if subtitle}<p class="muted">{subtitle}</p>{/if}
              {#if duration && position != null}
                <div class="progress" aria-hidden="true"><span style:width="{Math.min(100, (position / duration) * 100)}%"></span></div>
                <p class="muted small num">{t('salon.progression', { position: mmss(position), duree: mmss(duration) })}{playing ? ` · ${t('salon.fin_vers', { heure: clock(home.now + (duration - position) * 1000) })}` : ''}</p>
              {/if}
            </div>
            <button class="pp" onclick={playPause} aria-label={playing ? t('salon.pause') : t('salon.lecture')}><Icon name={playing ? 'pause' : 'play'} size={26} /></button>
          </div>
        {:else}
          <span class="eyebrow">{tvOn ? t('salon.ecran') : t('salon.television')}</span>
          <h2>{tvOn ? (castApp ?? appName ?? t('salon.tele_allumee')) : reachable(tv) ? t('salon.soiree') : t('salon.veille')}</h2>
          <p class="muted">
            {#if tvOn}{t('salon.aide_allumee')}{:else}{t('salon.aide_eteinte')}{/if}
          </p>
        {/if}
        <div class="apps" role="group" aria-label={t('salon.ouvrir_une_appli')}>
          {#each LAUNCH as pkg (pkg)}
            <button class:on={app === pkg} disabled={!tvOn || busy != null} onclick={() => open(pkg)}>{appLabel(pkg)}</button>
          {/each}
        </div>
      </section>

      {#if sp}<SpeakerCard id={sp} name={nameOf(sp, t('salon.barre_de_son'))} />{/if}

      <section class="card ambiance">
        <div class="card-head"><h2><Icon name="light-group" size={18} />{t('salon.ambiance')}</h2></div>
        <div class="amb">
          {#if s.room_light}<LightTile id={s.room_light} name={t('salon.tout_le_salon')} icon="sofa" dimmer members={s.lamps ?? []} />{/if}
          {#if tv}<LightTile id={tv} point="ambilight" name="Ambilight" icon="ambilight" />{/if}
        </div>
        <AmbiancePicker lights={s.lamps ?? []} label={t('salon.titre')} />
        <div class="lamps">
          {#each s.lamps ?? [] as lamp (lamp)}
            <LightTile id={lamp} icon="light" compact dimmer />
          {/each}
        </div>
      </section>
    </div>
  </div>
</div>

<style>
  .salon {
    display: grid;
    gap: 22px;
  }

  .layout {
    display: grid;
    grid-template-columns: minmax(320px, 400px) minmax(0, 1fr);
    gap: 24px;
    align-items: start;
  }

  .remote-col {
    position: sticky;
    top: 24px;
  }

  /* A grid track grows to its widest content (the ambiance strip) unless
     told it may shrink: on a phone, that pushed the whole page sideways. */
  .remote-col,
  .now-col,
  .now-col > :global(*) {
    min-width: 0;
  }

  .now-col {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 20px;
  }

  .now {
    padding: 26px;
    overflow: hidden;
  }

  .now::after {
    content: '';
    position: absolute;
    inset: auto -40px -60px auto;
    width: 220px;
    height: 220px;
    border-radius: 50%;
    background: radial-gradient(circle, #8f8cf544, transparent 70%);
    opacity: 0;
    transition: opacity 0.6s;
  }

  .now.lit::after {
    opacity: 1;
  }

  .eyebrow {
    font-size: 11px;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: var(--ink-3);
    font-weight: 750;
  }

  .now h2 {
    font-size: 28px;
    font-weight: 700;
    letter-spacing: -0.02em;
    margin: 6px 0 4px;
  }

  .playing {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    gap: 18px;
    align-items: center;
  }

  .playing img {
    width: 120px;
    aspect-ratio: 16 / 9;
    object-fit: cover;
    border-radius: var(--r-sm);
    background: var(--surface-2);
  }

  .what {
    min-width: 0;
  }

  .what h2 {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .progress {
    height: 6px;
    margin-top: 10px;
    border-radius: 999px;
    background: var(--surface-2);
    overflow: hidden;
  }

  .progress span {
    display: block;
    height: 100%;
    background: var(--cool);
  }

  .small {
    font-size: 13px;
    margin-top: 6px;
  }

  .pp {
    width: 52px;
    height: 52px;
    border: 0;
    border-radius: 50%;
    background: var(--surface-2);
    color: inherit;
    display: grid;
    place-items: center;
    cursor: pointer;
  }

  .apps {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-top: 18px;
  }

  .apps button {
    border: 0;
    border-radius: 999px;
    padding: 8px 14px;
    font: inherit;
    font-size: 14px;
    font-weight: 650;
    background: var(--surface-2);
    color: var(--ink-2);
    cursor: pointer;
  }

  .apps button.on {
    background: var(--cool-soft);
    color: var(--cool);
  }

  .apps button:disabled {
    opacity: 0.5;
    cursor: default;
  }

  @media (max-width: 560px) {
    .playing {
      grid-template-columns: minmax(0, 1fr) auto;
    }

    .playing img {
      grid-column: 1 / -1;
      width: 100%;
    }
  }

  .amb {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
  }

  .lamps {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 10px;
    margin-top: 12px;
  }

  @media (max-width: 1100px) {
    .layout {
      grid-template-columns: minmax(0, 1fr);
    }

    .remote-col {
      position: static;
    }
  }

  @media (max-width: 560px) {
    .amb,
    .lamps {
      grid-template-columns: 1fr;
    }
  }
</style>
