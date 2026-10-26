// A coffee machine's recipes (`recipes` point) and what a cup changes in
// them, without the house's words (tested on its own).

/** The recipes the machine keeps (`recipes` point): drink → field →
 *  { value, min, max, step, unit? }; null until read. */
export function parseRecipes(text) {
  try {
    const r = JSON.parse(text ?? 'null');
    return r && typeof r === 'object' ? r : null;
  } catch {
    return null;
  }
}

/** What changes from the recipe, for `brew_custom`: the fields set
 *  otherwise than the machine keeps them, or null when none. */
export function changesFrom(recipe, chosen) {
  const out = {};
  for (const [key, f] of Object.entries(recipe ?? {})) if (chosen[key] != null && chosen[key] !== f.value) out[key] = chosen[key];
  return Object.keys(out).length ? out : null;
}
