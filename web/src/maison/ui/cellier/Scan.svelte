<script>
  import { onDestroy } from 'svelte';
  import Icon from '../Icon.svelte';
  import CodeGate from './CodeGate.svelte';
  import { hub } from '../../../lib/hub.svelte.js';
  import { needsCode } from '../../lib/cellier/errors.js';
  import { cellier, scanReceipt, readerWhy } from '../../lib/cellier.svelte.js';
  import { t } from '../../../lib/i18n.svelte.js';

  /** Taking a receipt: one photo, or up to four for a long one (top first),
   *  then read together. The reading takes up to about 30 seconds; what comes
   *  back is a draft to check, never purchases. */

  const MAX = 4;

  // The Moli Maison app shows the dashboard in a WebView: iOS closes an app
  // that opens the camera without declaring it, so the camera is offered
  // only in a plain browser, or once the app says it can (`MoliNative.camera`).
  const native = typeof window !== 'undefined' ? window.MoliNative : undefined;
  const camera = !native || native.camera === true;
  // On a computer `capture` does nothing: one button to pick a file is enough.
  const touch = typeof matchMedia === 'function' && matchMedia('(pointer: coarse)').matches;
  const twoWays = camera && touch;

  /** `[{file, url}]`, in the receipt's order. */
  let photos = $state([]);
  let busy = $state(false);
  let error = $state('');
  let seconds = $state(0);
  /** The input the first photo came from: the next ones come the same way. */
  let source = $state(null);
  let cameraInput = $state();
  let galleryInput = $state();

  /** A code is set and nobody typed it: Moli would refuse the photos. */
  const codeNeeded = $derived(needsCode(hub.session));
  // The code was typed: the refusal that asked for it no longer applies.
  $effect(() => {
    if (!codeNeeded && error === t('cellier.code.requis')) error = '';
  });
  const refused = $derived(cellier.reader && !cellier.reader.allowed ? readerWhy(cellier.reader) : '');

  function add(e, from) {
    const input = e.currentTarget;
    const room = MAX - photos.length;
    const files = [...(input.files ?? [])];
    input.value = '';
    if (!files.length) return;
    source = source ?? from;
    error = files.length > room ? t('cellier.scan.trop', { max: MAX }) : '';
    photos = [...photos, ...files.slice(0, room).map((file) => ({ file, url: URL.createObjectURL(file) }))];
  }

  function remove(i) {
    URL.revokeObjectURL(photos[i].url);
    photos = photos.filter((_, n) => n !== i);
    if (!photos.length) source = null;
    error = '';
  }

  const more = () => (source === 'camera' ? cameraInput : galleryInput)?.click();

  let timer;
  /** The page was left during the read: the draft still lands in « À valider ». */
  let left = false;
  async function read() {
    if (busy || !photos.length || codeNeeded) return;
    busy = true;
    error = '';
    seconds = 0;
    timer = setInterval(() => (seconds += 1), 1000);
    try {
      const receipt = await scanReceipt(photos.map((p) => p.file));
      // Elsewhere by now: no jump back here (`scanReceipt` already re-read
      // the drafts, so the new one shows on the Achats page).
      if (!left) location.hash = `#/cellier/achats/ticket/${receipt.id}`;
    } catch (err) {
      if (!left) error = err.message;
    } finally {
      clearInterval(timer);
      busy = false;
    }
  }

  onDestroy(() => {
    left = true;
    clearInterval(timer);
    for (const p of photos) URL.revokeObjectURL(p.url);
  });
</script>

<a class="back" href="#/cellier/achats"><Icon name="arrow-left" size={20} />{t('cellier.scan.retour')}</a>

<section class="card scan" aria-labelledby="cellier-scan-title" aria-busy={busy}>
  <h2 id="cellier-scan-title">{t('cellier.scan.titre')}</h2>
  <p class="muted">{t('cellier.scan.aide')}</p>
  {#if refused}<p class="warn"><Icon name="warning" size={18} />{refused}</p>{/if}

  {#if camera}
    <input bind:this={cameraInput} class="file" type="file" accept="image/*" capture="environment" tabindex="-1" aria-hidden="true" onchange={(e) => add(e, 'camera')} />
  {/if}
  <input bind:this={galleryInput} class="file" type="file" accept="image/*" multiple tabindex="-1" aria-hidden="true" onchange={(e) => add(e, 'gallery')} />

  {#if photos.length}
    <ol class="photos">
      {#each photos as p, i (p.url)}
        <li>
          <img src={p.url} alt={t('cellier.scan.photo', { n: i + 1 })} />
          <span class="n num">{i + 1}</span>
          <button class="drop" disabled={busy} onclick={() => remove(i)} aria-label={t('cellier.scan.retirer', { n: i + 1 })}>
            <Icon name="close" size={18} />
          </button>
        </li>
      {/each}
    </ol>
    {#if photos.length < MAX}
      <button class="ghost" disabled={busy} onclick={more}>
        <Icon name="plus" size={20} />{t('cellier.scan.suite')}
      </button>
    {/if}
  {:else}
    <div class="take">
      {#if twoWays}
        <button class="primary" onclick={() => cameraInput.click()}><Icon name="camera" size={22} />{t('cellier.scan.photo_prendre')}</button>
      {/if}
      <button class={twoWays ? 'ghost' : 'primary'} onclick={() => galleryInput.click()}>
        <Icon name="gallery" size={22} />{twoWays ? t('cellier.scan.galerie') : t('cellier.scan.choisir')}
      </button>
    </div>
  {/if}

  {#if error}<p class="error" role="alert"><Icon name="alert" size={18} />{error}</p>{/if}

  {#if photos.length}
    <CodeGate />
    <div class="go">
      <button class="primary big" disabled={busy || !!refused || codeNeeded} onclick={read}>
        <Icon name="receipt" size={22} />
        {busy ? t('cellier.scan.lecture') : t('cellier.scan.lire')}
      </button>
      {#if busy}
        <p class="muted small" role="status">{t('cellier.scan.attente', { s: seconds })}</p>
      {/if}
    </div>
  {/if}
</section>

<style>
  .back {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    justify-self: start;
    min-height: 44px;
    color: var(--ink-2);
    font-weight: 600;
    text-decoration: none;
  }

  .scan {
    display: grid;
    gap: 14px;
    margin-top: 6px;
  }

  h2 {
    font-size: 18px;
    font-weight: 650;
  }

  .file {
    display: none;
  }

  .take,
  .go {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px;
  }

  .photos {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(120px, 1fr));
    gap: 10px;
  }

  .photos li {
    position: relative;
  }

  .photos img {
    display: block;
    width: 100%;
    aspect-ratio: 3 / 4;
    object-fit: cover;
    /* The head of a receipt (chain, store, date) tells which one it is. */
    object-position: top;
    border-radius: var(--r-sm);
    background: var(--surface-2);
  }

  .n {
    position: absolute;
    left: 6px;
    top: 6px;
    min-width: 26px;
    height: 26px;
    padding: 0 6px;
    display: grid;
    place-items: center;
    border-radius: 999px;
    background: var(--surface);
    box-shadow: var(--shadow);
    font-size: 13px;
    font-weight: 700;
  }

  .drop {
    position: absolute;
    right: 4px;
    top: 4px;
    width: 40px;
    height: 40px;
    display: grid;
    place-items: center;
    border: 0;
    border-radius: 999px;
    background: var(--surface);
    box-shadow: var(--shadow);
    color: var(--ink-2);
  }

  .primary,
  .ghost {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    min-height: 48px;
    padding: 0 20px;
    border-radius: 999px;
    font-size: 15px;
    font-weight: 650;
  }

  .primary {
    border: 0;
    background: var(--ink);
    color: var(--surface);
  }

  .ghost {
    justify-self: start;
    border: 1px solid var(--line);
    background: var(--surface);
    color: var(--ink-2);
  }

  .primary:disabled,
  .ghost:disabled {
    opacity: 0.45;
  }

  .big {
    min-height: 54px;
    padding: 0 26px;
    font-size: 16px;
  }

  .small {
    font-size: 13px;
  }

  .error {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    padding: 12px 16px;
    border-radius: var(--r-sm);
    background: var(--alert-soft);
    color: var(--alert);
    font-weight: 550;
  }

  .warn {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    padding: 12px 16px;
    border-radius: var(--r-sm);
    background: var(--warm-soft);
    color: var(--warm-ink);
    font-weight: 550;
  }

  .warn :global(svg),
  .error :global(svg) {
    margin-top: 2px;
  }

  @media (max-width: 760px) {
    .take,
    .go {
      flex-direction: column;
      align-items: stretch;
    }

    .ghost {
      justify-self: stretch;
    }

    .photos {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }
</style>
