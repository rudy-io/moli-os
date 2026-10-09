// Printing on a paper printer (an IPP printer Moli knows: `printer: true`):
// the pages are made here, in the browser (a PDF rendered page by page, a
// photo re-encoded), then sent one by one to Moli, which queues them.

import { t } from '../../lib/i18n.svelte.js';

/** A page's longest side, at most (A4 at 300 dpi): bigger is only heavier. */
const MAX_SIDE = 3508;
/** A PDF page is rendered at this resolution (text stays sharp). */
const PDF_DPI = 200;
const QUALITY = 0.9;
/** Moli's queue full, or too many pages at once: wait, then again. */
const RETRY_WAIT = { 409: 15_000, 429: 4_000 };
const RETRIES = 40;

export const isPaperPrinter = (d) => d?.printer === true;

export const isPdf = (file) => file.type === 'application/pdf' || /\.pdf$/i.test(file.name);

/** The canvas as a JPEG, then freed (Safari caps the canvases' memory). */
function jpeg(canvas) {
  return new Promise((resolve, reject) =>
    canvas.toBlob(
      (b) => {
        canvas.width = 0;
        canvas.height = 0;
        if (b) resolve(b);
        else reject(new Error(t('salon.papier.page_impossible')));
      },
      'image/jpeg',
      QUALITY,
    ),
  );
}

async function photo(file) {
  // EXIF orientation applied (a phone photo stands the right way up).
  const bitmap = await createImageBitmap(file);
  const scale = Math.min(1, MAX_SIDE / Math.max(bitmap.width, bitmap.height));
  const canvas = document.createElement('canvas');
  canvas.width = Math.round(bitmap.width * scale);
  canvas.height = Math.round(bitmap.height * scale);
  const g = canvas.getContext('2d');
  g.fillStyle = '#fff';
  g.fillRect(0, 0, canvas.width, canvas.height);
  g.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
  bitmap.close?.();
  return jpeg(canvas);
}

/** The pages of a file, made one at a time (a long PDF is never all in
 *  memory): `{ count, page(n) → Promise<Blob>, close() }`, n from 1. */
export async function pagesOf(file) {
  if (isPdf(file)) {
    // Loaded only when a PDF is printed (the library is big); the legacy
    // build also runs on older tablets (an iPad left on an older iPadOS).
    const pdfjs = await import('pdfjs-dist/legacy/build/pdf.mjs');
    pdfjs.GlobalWorkerOptions.workerSrc = (await import('pdfjs-dist/legacy/build/pdf.worker.min.mjs?url')).default;
    const task = pdfjs.getDocument({ data: new Uint8Array(await file.arrayBuffer()) });
    const doc = await task.promise;
    return {
      count: doc.numPages,
      async page(n) {
        const page = await doc.getPage(n);
        const base = page.getViewport({ scale: 1 });
        // 200 dpi, but never a side above A4 at 300 dpi (an A0 plan).
        const scale = Math.min(PDF_DPI / 72, MAX_SIDE / Math.max(base.width, base.height));
        const viewport = page.getViewport({ scale });
        const canvas = document.createElement('canvas');
        canvas.width = Math.ceil(viewport.width);
        canvas.height = Math.ceil(viewport.height);
        await page.render({ canvas, viewport, intent: 'print', background: '#ffffff' }).promise;
        page.cleanup();
        return jpeg(canvas);
      },
      // pdf.js 6: the loading task frees the document (and its worker).
      close: () => task.destroy(),
    };
  }
  if (file.type.startsWith('image/')) return { count: 1, page: () => photo(file), close() {} };
  throw new Error(t('salon.papier.choisir'));
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** One page to Moli; a full queue or a burst is waited out, not failed. */
async function send(id, blob, query, waiting) {
  for (let attempt = 0; ; attempt++) {
    const res = await fetch(`/api/devices/${encodeURIComponent(id)}/print?${query}`, {
      method: 'POST',
      headers: { 'content-type': 'image/jpeg', 'x-moli-origin': 'ui' },
      body: blob,
    });
    if (res.ok) return;
    const said = (await res.json().catch(() => ({}))).error ?? t('salon.papier.erreur', { status: res.status });
    const busy = res.status === 429 || (res.status === 409 && /pleine|full/.test(said));
    if (!busy || attempt >= RETRIES) throw new Error(said);
    waiting();
    await sleep(RETRY_WAIT[res.status] ?? 10_000);
  }
}

/** Sends a file, page after page. `progress({ done, total, step })`, step
 *  « prepare », « envoie », « attend » (Moli's queue is full) or « fini ».
 *  A failure says how many pages went (they print: sending again would
 *  print them twice). */
export async function printFile(id, file, { copies = 1, color = true, pages: only = null } = {}, progress = () => {}) {
  const pages = await pagesOf(file);
  const name = file.name.replace(/\.[^.]+$/, '') || 'Document';
  const wanted = (only ?? Array.from({ length: pages.count }, (_, i) => i + 1)).filter((n) => n >= 1 && n <= pages.count);
  let done = 0;
  try {
    for (const n of wanted) {
      progress({ done, total: wanted.length, step: 'prepare' });
      const blob = await pages.page(n);
      progress({ done, total: wanted.length, step: 'envoie' });
      const query = new URLSearchParams({
        name: pages.count > 1 ? `${name} (p. ${n}/${pages.count})` : name,
        copies: String(copies),
        color: String(color),
      });
      await send(id, blob, query, () => progress({ done, total: wanted.length, step: 'attend' }));
      done += 1;
    }
    progress({ done, total: wanted.length, step: 'fini' });
  } catch (err) {
    if (done > 0) {
      // `partial`: the card shows this one as it is (the pages that went print).
      const partial = new Error(t('salon.papier.partiel', { count: done, total: wanted.length, message: err.message }));
      partial.partial = true;
      throw partial;
    }
    throw err;
  } finally {
    pages.close();
  }
}

/** « 1-3, 5 » → [1, 2, 3, 5]; empty: every page (null). */
export function parsePages(text, count) {
  const raw = String(text ?? '').trim();
  if (!raw) return null;
  const out = new Set();
  for (const part of raw.split(/[,;\s]+/).filter(Boolean)) {
    const m = /^(\d+)(?:-(\d+))?$/.exec(part);
    if (!m) return [];
    const a = Number(m[1]);
    const b = Number(m[2] ?? m[1]);
    for (let n = Math.min(a, b); n <= Math.max(a, b) && n <= count; n++) if (n >= 1) out.add(n);
  }
  return [...out].sort((x, y) => x - y);
}
