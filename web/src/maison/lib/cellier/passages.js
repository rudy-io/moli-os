// The purchases grouped by passage: one trip to a shop, one drive basket.
// Pure functions (no `$state`), so node:test can import them.
import { lineCost } from './draft.js';

const MINUTE = 60_000;

/** What a purchase line cost: the server's figure, else quantity times price
 *  less the discount (rounded half away from zero, as the server does). */
export function cost(p) {
  return p.line_cents ?? lineCost(p.qty, p.price_cents, p.discount_cents ?? 0);
}

/** Group purchases by receipt, else by basket, else by (chain, store, minute).
 *  `receipts` carries the printed totals. Newest first.
 *  @returns {{key: string, kind: 'ticket'|'drive'|'other', receipt: number|null, provider: string,
 *             store: string, at: number, total_cents: number, printed_cents: number|null,
 *             lines: object[]}[]} */
export function passages(purchases, receipts = []) {
  const known = new Map(receipts.map((r) => [r.id, r]));
  const groups = new Map();
  for (const p of purchases) {
    let key;
    let kind;
    if (p.receipt != null) {
      key = `ticket:${p.receipt}`;
      kind = 'ticket';
    } else if (p.basket != null) {
      key = `basket:${p.basket}`;
      kind = 'drive';
    } else {
      key = `other:${p.provider}|${p.store}|${Math.floor(p.at / MINUTE)}`;
      kind = 'other';
    }
    let group = groups.get(key);
    if (!group) {
      group = {
        key,
        kind,
        receipt: kind === 'ticket' ? p.receipt : null,
        provider: p.provider,
        store: p.store,
        at: p.at,
        total_cents: 0,
        printed_cents: null,
        lines: [],
      };
      groups.set(key, group);
    }
    group.lines.push(p);
    group.total_cents += cost(p);
    group.at = Math.min(group.at, p.at);
  }
  for (const group of groups.values()) {
    if (group.kind !== 'ticket') continue;
    const printed = known.get(group.receipt)?.draft?.total_cents;
    if (printed != null && printed !== group.total_cents) group.printed_cents = printed;
  }
  return [...groups.values()].sort((a, b) => b.at - a.at);
}

/** Passages grouped by calendar month (local time), newest month first.
 *  @returns {{month: string, total_cents: number, passages: object[]}[]} */
export function byMonth(list) {
  const months = new Map();
  for (const passage of list) {
    const d = new Date(passage.at);
    const month = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}`;
    let entry = months.get(month);
    if (!entry) {
      entry = { month, total_cents: 0, passages: [] };
      months.set(month, entry);
    }
    entry.total_cents += passage.total_cents;
    entry.passages.push(passage);
  }
  return [...months.values()].sort((a, b) => (a.month < b.month ? 1 : -1));
}
