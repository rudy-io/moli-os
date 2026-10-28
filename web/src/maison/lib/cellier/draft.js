// Editing a receipt draft before it is validated: amounts typed by a person,
// the gap with the printed total, what would stop the validation; and the
// small formatting shared by the Cellier's pages. Pure functions (no
// `$state`), so node:test can import them.

/** What a line costs: quantity times unit price, rounded half away from zero
 *  as the server does, less the discount. */
export function lineCost(qty, priceCents, discountCents = 0) {
  const gross = qty * priceCents;
  return Math.sign(gross) * Math.round(Math.abs(gross)) - discountCents;
}

/** The printed total minus every line's cost (kept or not, as the server
 *  counts it); null without a printed total. */
export function gapOf(draft) {
  if (draft?.total_cents == null) return null;
  return draft.total_cents - draft.lines.reduce((sum, l) => sum + l.line_cents, 0);
}

/** A number typed by a person (« 2,58 », « 2.58 € », « 0,532 »): zero or
 *  more, else null. */
export function parseAmount(text) {
  const clean = String(text ?? '')
    .replace(/[\s  €]/g, '')
    .replace(',', '.');
  if (!clean || !/^\d*\.?\d*$/.test(clean) || clean === '.') return null;
  const n = Number(clean);
  return Number.isFinite(n) && n >= 0 ? n : null;
}

/** Euros typed as cents, or null. */
export function toCents(text) {
  const n = parseAmount(text);
  return n == null ? null : Math.round(n * 100);
}

/** Cents as euros to edit (« 2,58 » in French), without the currency sign. */
export function centsText(cents, locale = 'fr-FR') {
  if (cents == null) return '';
  return (cents / 100).toLocaleString(locale, { minimumFractionDigits: 2, maximumFractionDigits: 2, useGrouping: false });
}

/** A quantity to edit (« 0,532 »). */
export function qtyText(qty, locale = 'fr-FR') {
  return qty == null ? '' : qty.toLocaleString(locale, { maximumFractionDigits: 3, useGrouping: false });
}

const LETTER_OR_DIGIT = /[\p{L}\p{N}]/u;

/** Why the draft cannot be validated yet, line by line (kept lines only):
 *  `[{index, why}]` with `why` one of `product` (no product chosen),
 *  `name` (a new product needs a name with letters or digits), `qty`
 *  (above zero) or `price` (zero or more, discount too). */
export function draftProblems(draft) {
  const out = [];
  draft.lines.forEach((line, index) => {
    if (!line.keep) return;
    const product = line.product?.trim();
    const fresh = line.new_product;
    if (!product && fresh == null) out.push({ index, why: 'product' });
    else if (!product && !LETTER_OR_DIGIT.test(fresh)) out.push({ index, why: 'name' });
    else if (!Number.isFinite(line.qty) || line.qty <= 0) out.push({ index, why: 'qty' });
    else if (line.price_cents < 0 || line.discount_cents < 0) out.push({ index, why: 'price' });
  });
  return out;
}

/** The open shopping-list lines this receipt may settle: those of a product
 *  a kept line is bought as. */
export function listMatches(draft, items) {
  const bought = new Set(draft.lines.filter((l) => l.keep && l.product).map((l) => l.product));
  return items.filter((i) => i.product && bought.has(i.product));
}

/** The ticked shopping-list lines that still match a kept line: a line
 *  unticked or moved to another product no longer settles its item. */
export function pruneItemsDone(draft, items) {
  const open = new Set(listMatches(draft, items).map((i) => i.id));
  return draft.items_done.filter((id) => open.has(id));
}

/** The parts that exist, « a · b · c ». */
export const dots = (parts) => parts.filter(Boolean).join(' · ');

/** What was bought, with `money(cents)` for the amounts: « 1,29 € » for one,
 *  « 2 × 1,29 € », « 0,532 kg × 12,99 €/kg ». */
export function qtyPrice(line, money, locale = 'fr-FR') {
  const price = `${money(line.price_cents)}${line.unit ? `/${line.unit}` : ''}`;
  if (line.qty === 1 && !line.unit) return price;
  return `${qtyText(line.qty, locale)}${line.unit ? ` ${line.unit}` : ''} × ${price}`;
}

/** An instant (ms) as a `datetime-local` value, in this browser's time zone. */
export function toLocalInput(ms) {
  if (ms == null) return '';
  const d = new Date(ms);
  const pad = (n) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** A `datetime-local` value back to an instant (ms), or null. */
export function fromLocalInput(value) {
  if (!value) return null;
  const ms = new Date(value).getTime();
  return Number.isFinite(ms) ? ms : null;
}
