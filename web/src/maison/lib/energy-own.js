// What the house used without buying it, bucket by bucket, pure: the
// whole-home clamp and the Linky see only what was bought, the circuits
// also ran on what the panels made and the home kept, and on what the
// battery gave back (not on what it stored). As `unmeasured` in the
// server's report (crate moli-energy).

/** `(bucket) → kWh` used at home on top of what was bought: production −
 *  export (never more out than made), less what the battery stored
 *  (charge − discharge, negative when it gave back more). `null` without
 *  panels and export, nor a battery: nothing to add. */
export function ownUse(meters = []) {
  const ids = (role) => meters.filter((m) => m.role === role).map((m) => m.id);
  const [solar, exported, charge, discharge] = ['solar', 'export', 'charge', 'discharge'].map(ids);
  const sum = (b, list) => list.reduce((t, id) => t + (b.meters?.[id]?.kwh ?? 0), 0);
  const panels = solar.length > 0 && exported.length > 0;
  const battery = charge.length > 0 || discharge.length > 0;
  if (!panels && !battery) return null;
  return (b) => {
    const made = sum(b, solar);
    const kept = panels ? Math.min(Math.max(made - sum(b, exported), 0), Math.max(made, 0)) : 0;
    const stored = battery ? sum(b, charge) - sum(b, discharge) : 0;
    return kept - stored;
  };
}
