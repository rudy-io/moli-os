// What the « Dehors » page shows of the rooms outside (#217): no list to keep,
// the room of a device is enough. Pure: the page passes what it knows.

/** Whether a room is outside: `/api/home` says it (`rooms[].outside`). */
export const isOutside = (layoutRooms, name) => (layoutRooms ?? []).some((r) => r?.name === name && r.outside === true);

/** A device whose points say it measures a pool (pH, chlorine). */
export const measuresPool = (d) => (d?.points ?? []).some((p) => p.key === 'ph' || p.key === 'orp');

/** The outside rooms as the page shows them, and what goes elsewhere on it.
 *
 *  - `rooms`: the room list (`roomList()` shape), cut to the outside ones.
 *  - `layoutRooms`: `home.config.rooms`, where `/api/home` wrote `outside`.
 *  - `pool`, `pump`: the pool and its pump if `home.json` names them.
 *  - `garden`: `outdoor.garden_lights` (legacy: still read, merged).
 *  - `cams`: the camera tiles the page already draws (`[{ id }]`).
 *  - `on`: whether a light is on (to count the lit ones again).
 *  - `shown`: devices another card of the page already shows (a station).
 *
 *  Returns `{ rooms, pool, garden, cams }`: the outside rooms without what
 *  the page shows in its own cards (the pool, its pump, the cameras); the
 *  pool (named, else the first outside device that measures one); the
 *  garden lights no outside room already shows; the outside rooms' cameras
 *  the page does not draw yet. */
export function outdoorView({ rooms, layoutRooms, pool = null, pump = null, garden = [], cams = [], on = () => false, shown = [] }) {
  const outside = (rooms ?? []).filter((r) => isOutside(layoutRooms, r.name));
  const all = (r) => [r.group, ...r.lights, ...r.plugs, ...r.covers, ...r.impulses, ...r.climates, ...r.media, ...(r.helpers ?? []), ...r.sensors].filter(Boolean);
  const found = pool ?? outside.flatMap(all).find(measuresPool)?.id ?? null;

  const drawn = new Set(cams.map((c) => c.id));
  const extraCams = [];
  const elsewhere = new Set([found, pump, ...shown].filter(Boolean));
  for (const r of outside) {
    for (const d of r.sensors) {
      if (d.camera) {
        elsewhere.add(d.id);
        if (!drawn.has(d.id)) {
          drawn.add(d.id);
          extraCams.push({ id: d.id });
        }
      }
    }
  }

  const keep = (list) => list.filter((d) => !elsewhere.has(d.id));
  const visible = outside
    .map((r) => {
      const lights = keep(r.lights);
      return {
        ...r,
        group: r.group && !elsewhere.has(r.group.id) ? r.group : null,
        lights,
        plugs: keep(r.plugs),
        covers: keep(r.covers),
        impulses: keep(r.impulses),
        climates: keep(r.climates),
        media: keep(r.media),
        helpers: keep(r.helpers ?? []),
        sensors: keep(r.sensors),
        lit: lights.filter((d) => on(d.id)).length,
      };
    })
    .filter((r) => all(r).length);

  const inRooms = new Set(visible.flatMap(all).flatMap((d) => [d.id, ...(d.members ?? [])]));
  return {
    rooms: visible,
    pool: found,
    garden: (garden ?? []).filter((l) => l?.id && !inRooms.has(l.id)),
    cams: extraCams,
  };
}
