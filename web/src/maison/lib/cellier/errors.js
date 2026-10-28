// The pantry's refusals (moli-pantry's `PantryError`, in English) said to a
// person. Pure (no `$state`), so node:test can import it.

/** The translation key (and its parameters) for a refusal the pantry is
 *  known to give, or null for one to say generically.
 *  @returns {{key: string, params?: object} | null} */
export function knownRefusal(raw) {
  const text = String(raw ?? '');
  // The server's « only a person sends this » (403), in either language.
  if (isHumanOnly(text)) return { key: 'cellier.code.requis' };
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

/** The server's refusal of a send that needs a person (`serveur.session.humain_seulement`,
 *  « seule une personne (code) envoie ceci » / « only a person (with the code) can send this »). */
export const isHumanOnly = (raw) => /seule une personne \(code\)|only a person \(with the code\)/i.test(String(raw ?? ''));

/** True when the code must be typed before a receipt can be sent: a code is
 *  set on the house and this dashboard is not (yet) recognised as a person. */
export const needsCode = (session) => !!session?.pin_configured && !session?.human;
