// The pantry's refusals (moli-pantry's `PantryError`, in English) said to a
// person. Pure (no `$state`), so node:test can import it.

/** The translation key (and its parameters) for a refusal the pantry is
 *  known to give, or null for one to say generically.
 *  @returns {{key: string, params?: object} | null} */
export function knownRefusal(raw) {
  const text = String(raw ?? '');
  // Validated or abandoned by someone else while this page was open.
  if (/^not found: receipt \d+/.test(text) || /^receipt \d+ is validated/.test(text)) return { key: 'cellier.ticket.parti' };
  if (/looks already recorded/.test(text)) return { key: 'cellier.ticket.doublon_sans_date' };
  if (/^not found: purchase \d+/.test(text)) return { key: 'cellier.erreurs.achat' };
  const product = text.match(/^not found: product (.+)$/);
  if (product) return { key: 'cellier.erreurs.produit', params: { id: product[1] } };
  const line = text.match(/^line (\d+)/);
  if (line) return { key: 'cellier.erreurs.ligne', params: { n: Number(line[1]) } };
  return null;
}
