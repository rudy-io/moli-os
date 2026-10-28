// The household's pantry, « Le Cellier » (server: moli-api pantry, crate
// moli-pantry): the shopping list, the products and their sheets per chain,
// the drives' settings and the open basket. Read in one go (`GET
// /api/cellier`), again on every `pantry_changed` event (see hub.svelte.js):
// an agent fills prices and baskets in the background.
import { t } from '../../lib/i18n.svelte.js';
import { money } from '../../lib/energy.js';
import { shrink, toBase64 } from './cellier/photo.js';
import { knownRefusal } from './cellier/errors.js';

export const cellier = $state({
  loaded: false,
  /** No `[pantry]` in moli.toml: the routes answer 404. */
  absent: false,
  items: [],
  products: [],
  offers: [],
  /** `[{info: {id, name, kind, site}, settings: {id, enabled, store, min_order_cents, fee_cents}}]` */
  providers: [],
  threshold: 0.3,
  basket: null,
  /** The purchases of the period read last (`loadPurchases`), with their receipts
   *  (the drafts still to check included). */
  purchases: [],
  receipts: [],
  /** The purchases were read at least once. */
  purchasesLoaded: false,
  /** Who reads receipts: `{provider, model, allowed, reason}`, null until read. */
  reader: null,
  /** Why the last read failed, one field per read so that one succeeding
   *  does not hide another failing: the pantry, the purchases, the reader. */
  error: '',
  purchasesError: '',
  readerError: '',
});

const headers = { 'content-type': 'application/json', 'x-moli-origin': 'ui' };

/** Throws an error with the HTTP `status` (0 when Moli could not be
 *  reached) and the server's own words in `detail`. */
async function call(method, path, body) {
  let res;
  try {
    res = await fetch(path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) });
  } catch {
    // No answer at all: the Wi-Fi dropped, Moli restarted...
    const err = new Error(t('cellier.erreurs.connexion'));
    err.status = 0;
    throw err;
  }
  if (res.status === 204) return null;
  const json = await res.json().catch(() => ({}));
  if (!res.ok) {
    const err = new Error(json.error ?? t('cellier.erreur', { status: res.status }));
    err.status = res.status;
    err.detail = json.error ?? '';
    throw err;
  }
  return json;
}

/** A failed write said to a person: `{text, detail}`. The pantry's own
 *  refusals (in English) become the house's words, the known ones exactly,
 *  the others generically with the server's words as `detail`; what the
 *  server already says in the house's language (a guest, the demo house) is
 *  kept as it is. */
export function explain(err) {
  if (![400, 404, 500].includes(err?.status) || !err.detail) return { text: err?.message ?? String(err), detail: '' };
  const known = knownRefusal(err.detail);
  return known ? { text: t(known.key, known.params), detail: '' } : { text: t('cellier.erreurs.refus'), detail: err.detail };
}

export async function loadCellier() {
  try {
    const all = await call('GET', '/api/cellier');
    Object.assign(cellier, {
      absent: false,
      items: all.items,
      products: all.products,
      offers: all.offers,
      providers: all.providers,
      threshold: all.threshold,
      basket: all.basket,
      error: '',
    });
  } catch (e) {
    cellier.absent = e.status === 404;
    cellier.error = cellier.absent ? '' : e.message;
  } finally {
    cellier.loaded = true;
  }
}

/** Every write re-reads the whole pantry (and the purchases, once a page read
 *  them): the answers are small, and the same read comes anyway with the
 *  change event. Writes throw; the pages catch and say why. */
async function write(method, path, body) {
  const answer = await call(method, path, body);
  await reloadCellier();
  return answer;
}

export const addItem = (text, product = null, qty = 1) => write('POST', '/api/cellier/items', { text, product, qty });
export const setQty = (id, qty) => write('PUT', `/api/cellier/items/${id}`, { qty });
export const removeItem = (id) => write('DELETE', `/api/cellier/items/${id}`);
export const saveProduct = (product) => write('PUT', `/api/cellier/products/${encodeURIComponent(product.id)}`, product);
export const confirmOffer = (id) => write('POST', `/api/cellier/offers/${id}/confirm`);
/** `{threshold?, providers?: [settings]}` */
export const saveSettings = (settings) => write('PUT', '/api/cellier/settings', settings);
export const chooseOption = (basket, option, accepted) => write('POST', `/api/cellier/baskets/${basket}/choose`, { option, accepted });
export const setBasketStatus = (basket, status) => write('POST', `/api/cellier/baskets/${basket}/status`, { status });

/** Cents as an amount in the house's language (« 12,34 € »). */
export const euros = (cents) => money(cents / 100);

/** The line is in a drive's cart (a basket filled or ordered), so a new
 *  request for it makes a new line. Only the open basket's status is known:
 *  a line tied to it is in a cart once it is filled or ordered (while merely
 *  chosen, it is not yet); a line tied to another basket is, since an older
 *  basket still open beside a newer one can only be filled or ordered. */
export const inCart = (item) =>
  item.basket != null && !(cellier.basket?.id === item.basket && ['proposed', 'chosen'].includes(cellier.basket.status));

/** A chain's display name from its id. */
export const providerName = (id) => cellier.providers.find((p) => p.info.id === id)?.info.name ?? id;

// ---- Receipts and purchases ---------------------------------------------------

/** The period the purchases were read for last; null until a page reads them. */
let period = null;

/** The purchases between two instants (ms; the server defaults to the last
 *  12 months) with the receipts they come from and the drafts to check. */
export async function loadPurchases(from, to) {
  period = { from, to };
  const query = new URLSearchParams();
  if (from != null) query.set('from', String(from));
  if (to != null) query.set('to', String(to));
  // `URLSearchParams.size` is too recent for some phones' WebViews.
  const suffix = query.toString() ? `?${query}` : '';
  try {
    const all = await call('GET', `/api/cellier/purchases${suffix}`);
    Object.assign(cellier, { purchases: all.purchases, receipts: all.receipts, purchasesError: '' });
  } catch (e) {
    cellier.purchasesError = e.message;
  } finally {
    cellier.purchasesLoaded = true;
  }
}

/** Re-reads what the pages show: the pantry, and the purchases of the same
 *  period once a page read them (a draft scanned by someone else, a line
 *  corrected...). What `pantry_changed` calls. */
export async function reloadCellier() {
  await Promise.all([loadCellier(), period ? loadPurchases(period.from, period.to) : null]);
}

/** One receipt with its draft (and the model's raw reading). Throws. */
export const loadReceipt = (id) => call('GET', `/api/cellier/receipts/${id}`);

/** Who reads receipts, and whether they may. */
export async function loadReader() {
  try {
    cellier.reader = await call('GET', '/api/cellier/reader');
    cellier.readerError = '';
  } catch (e) {
    cellier.reader = null;
    cellier.readerError = e.status === 404 ? '' : e.message;
  }
}

/** Why receipts cannot be read, for a person: the server's reason (said in
 *  the house's language), or « no assistant » translated. */
export const readerWhy = (reader) =>
  !reader?.reason || reader.reason === 'no assistant' ? t('cellier.ticket.pas_assistant') : reader.reason;

/** Why the server refused to read (503): no assistant, or the provider is not allowed. */
async function refusal() {
  await loadReader();
  const reader = cellier.reader;
  if (!reader || reader.allowed !== false || reader.reason === 'no assistant') return t('cellier.ticket.pas_assistant');
  // The server's reason says which provider and why; the generic words otherwise.
  return reader.reason || t('cellier.ticket.refuse');
}

/** Sends the photos of one receipt (1 to 4 images, shrunk here) to be read.
 *  Answers the draft receipt created, or throws an error with a translated
 *  message (a photo the browser cannot open, unreadable, no assistant,
 *  refused, busy...) and the HTTP `status` (0 when nothing was sent). */
export async function scanReceipt(photos) {
  let sent;
  try {
    sent = await Promise.all(photos.map(async (photo) => toBase64(await shrink(photo))));
  } catch {
    // A format the browser cannot decode (HEIC on a desktop browser...).
    const err = new Error(t('cellier.ticket.photo_illisible'));
    err.status = 0;
    throw err;
  }
  let receipt;
  try {
    receipt = await call('POST', '/api/cellier/receipts', { photos: sent });
  } catch (e) {
    const err = new Error(await scanError(e));
    err.status = e.status;
    throw err;
  }
  await reloadCellier();
  return receipt;
}

/** What to tell about a refused scan. */
async function scanError(e) {
  switch (e.status) {
    case 422:
      return t('cellier.ticket.illisible');
    case 503:
      return refusal();
    case 502:
      return t('cellier.ticket.lecture_inutilisable');
    case 429:
      return t('cellier.ticket.occupe');
    case 413:
      return t('cellier.ticket.trop_lourd');
    case 0:
      // The read takes long: a phone leaving the Wi-Fi meanwhile is likely.
      return t('cellier.ticket.connexion_lecture');
    case 400:
      // The assistant's own refusals say « invalid request: ... »; the
      // others come from the photos' checks.
      return /^invalid request/i.test(e.detail ?? '') ? t('cellier.ticket.demande_refusee') : t('cellier.ticket.photos_refusees');
    default:
      return e.message;
  }
}

/** Saves the corrected draft of a receipt (a `ReceiptDraft`); answers the receipt. */
export const saveDraft = (id, draft) => write('PUT', `/api/cellier/receipts/${id}`, draft);
/** Validates a draft: its kept lines become purchases. A receipt that looks
 *  recorded already answers 400 until `force` is set. */
export const validateReceipt = (id, force = false) => write('POST', `/api/cellier/receipts/${id}/validate`, { force });
/** Drops a draft that will not be recorded. */
export const abandonReceipt = (id) => write('DELETE', `/api/cellier/receipts/${id}`);
/** `{qty, price_cents, line_cents, product}` */
export const updatePurchase = (id, fields) => write('PUT', `/api/cellier/purchases/${id}`, fields);
export const deletePurchase = (id) => write('DELETE', `/api/cellier/purchases/${id}`);
/** A photo kept of a receipt (`n` from 1 to 4). */
export const photoUrl = (id, n) => `/api/cellier/receipts/${id}/photos/${n}`;
