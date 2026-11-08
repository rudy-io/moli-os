// The rooms of the house, floor by floor, as the plan stacks them.
import { t } from '../../lib/i18n.svelte.js';

/** Plan rooms outside the walls (the garden, the driveway…). */
const OUTSIDE = new Set(['garden', 'terrace', 'deck', 'pool', 'driveway']);
/** The sections added after the floors: ids no floor can have (a floor
 *  named « Dehors » gets the id « dehors » from the plan). */
const OUT = '§dehors';
const ANYWHERE = '§partout';

/** The ids of a room's devices (a fixture's bulbs with it). */
const idsOf = (room) =>
  [room.group, ...room.lights, ...room.plugs, ...room.covers, ...room.climates, ...room.media, ...room.sensors]
    .filter(Boolean)
    .flatMap((d) => [d.id, ...(d.members ?? [])]);

/** Where each room of `rooms` is: the floor whose plan draws it (a plan
 *  room's `room`, through `named` for the aliases), « Dehors » when the plan
 *  draws it only outside, else the floor where most of its devices are
 *  pinned (the attic), else « Toute la maison » (the alarm). Without a plan,
 *  one section with no name.
 *  Returns `{ sections: [{ id, name }], of: Map<room name, section id> }`. */
export function floorsOf(rooms, plan, named) {
  const floors = plan?.floors ?? [];
  if (!floors.length) return { sections: [{ id: 'all', name: null }], of: new Map(rooms.map((r) => [r.name, 'all'])) };

  // Room name → { floor, inside }: a room drawn inside wins over its driveway.
  const drawn = new Map();
  for (const f of floors) {
    for (const r of f.rooms ?? []) {
      if (!r.room) continue;
      const name = named(r.room);
      const inside = !OUTSIDE.has(r.kind);
      const seen = drawn.get(name);
      if (!seen || (inside && !seen.inside)) drawn.set(name, { floor: f.id, inside });
    }
  }
  const pinned = new Map();
  for (const f of floors) for (const p of f.devices ?? []) pinned.set(p.id, f.id);

  const where = (room) => {
    const d = drawn.get(room.name);
    if (d) return d.inside ? d.floor : OUT;
    const votes = new Map();
    for (const id of idsOf(room)) {
      const f = pinned.get(id);
      if (f) votes.set(f, (votes.get(f) ?? 0) + 1);
    }
    return [...votes].sort((a, b) => b[1] - a[1])[0]?.[0] ?? ANYWHERE;
  };

  return {
    sections: [
      ...floors.map((f) => ({ id: f.id, name: f.name })),
      { id: OUT, name: t('commun.etage.dehors') },
      { id: ANYWHERE, name: t('commun.etage.toute_la_maison') },
    ],
    of: new Map(rooms.map((r) => [r.name, where(r)])),
  };
}
