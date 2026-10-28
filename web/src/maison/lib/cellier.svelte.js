// The household's pantry, « Le Cellier » (server: moli-api pantry, crate
// moli-pantry): the shopping list, the products and their sheets per chain,
// the drives' settings and the open basket. Read in one go (`GET
// /api/cellier`), again on every `pantry_changed` event (see hub.svelte.js):
// an agent fills prices and baskets in the background.
import { t } from '../../lib/i18n.svelte.js';
import { money } from '../../lib/energy.js';
import { shrink, toBase64 } from './cellier/photo.js';

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
  /** Who reads receipts: `{provider, model, allowed, reason}`, null until read. */
  reader: null,
  error: '',
});

const headers = { 'content-type': 'application/json', 'x-moli-origin': 'ui' };

async function call(method, path, body) {
  const res = await fetch(path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) });
  if (res.status === 204) return null;
  const json = await res.json().catch(() => ({}));
  if (!res.ok) {
    const err = new Error(json.error ?? t('cellier.erreur', { status: res.status }));
    err.status = res.status;
    throw err;
  }
  return json;
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

/** Every write re-reads the whole pantry: the answer is small, and the same
 *  read comes anyway with the change event. */
async function write(method, path, body) {
  const answer = await call(method, path, body);
  await loadCellier();
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

/** The purchases between two instants (ms; the server defaults to the last
 *  12 months) with the receipts they come from and the drafts to check. */
export async function loadPurchases(from, to) {
  const query = new URLSearchParams();
  if (from != null) query.set('from', String(from));
  if (to != null) query.set('to', String(to));
  const suffix = query.size ? `?${query}` : '';
  try {
    const all = await call('GET', `/api/cellier/purchases${suffix}`);
    Object.assign(cellier, { purchases: all.purchases, receipts: all.receipts, error: '' });
  } catch (e) {
    cellier.error = e.message;
  }
}

/** Who reads receipts, and whether they may. */
export async function loadReader() {
  try {
    cellier.reader = await call('GET', '/api/cellier/reader');
  } catch (e) {
    cellier.reader = null;
    cellier.error = e.status === 404 ? '' : e.message;
  }
}

/** Why the server refused to read (503): no assistant, or the provider is not allowed. */
async function refusal() {
  await loadReader();
  return cellier.reader && cellier.reader.allowed === false && cellier.reader.reason !== 'no assistant'
    ? t('cellier.ticket.refuse')
    : t('cellier.ticket.pas_assistant');
}

/** Sends the photos of one receipt (1 to 4 images, shrunk here) to be read.
 *  Answers the draft receipt created, or throws an error with a translated
 *  message (unreadable, no assistant, refused...) and the HTTP `status`. */
export async function scanReceipt(photos) {
  const sent = await Promise.all(photos.map(async (photo) => toBase64(await shrink(photo))));
  try {
    return await call('POST', '/api/cellier/receipts', { photos: sent });
  } catch (e) {
    const message =
      e.status === 422
        ? t('cellier.ticket.illisible')
        : e.status === 503
          ? await refusal()
          : e.status === 502
            ? t('cellier.ticket.lecture_inutilisable')
            : e.status === 400
              ? t('cellier.ticket.photos_refusees')
              : e.message;
    const err = new Error(message);
    err.status = e.status;
    throw err;
  }
}

/** Saves the corrected draft of a receipt (a `ReceiptDraft`). */
export const saveDraft = (id, draft) => call('PUT', `/api/cellier/receipts/${id}`, draft);
/** Validates a draft: its kept lines become purchases. A receipt that looks
 *  recorded already answers 400 until `force` is set. */
export const validateReceipt = (id, force = false) => write('POST', `/api/cellier/receipts/${id}/validate`, { force });
/** Drops a draft that will not be recorded. */
export const abandonReceipt = (id) => call('DELETE', `/api/cellier/receipts/${id}`);
/** `{qty, price_cents, line_cents, product}` */
export const updatePurchase = (id, fields) => write('PUT', `/api/cellier/purchases/${id}`, fields);
export const deletePurchase = (id) => write('DELETE', `/api/cellier/purchases/${id}`);
/** A photo kept of a receipt (`n` from 1 to 4). */
export const photoUrl = (id, n) => `/api/cellier/receipts/${id}/photos/${n}`;
