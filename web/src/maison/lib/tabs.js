// Whether a page's tab has something to show, from the devices the house has.
// A house with no camera and no 3D printer sees no empty Caméras or Impression tab;
// each comes back on its own with the first device it shows.

const CONTACT = ['contact', 'doorcontact_state'];
const MOVES = ['occupancy', 'pir', 'motion'];
/** Who is home (people, phones, presence drivers) is no sensor of the house. */
const PEOPLE = ['people', 'phones', 'presence'];

const has = (d, keys) => !!d?.points?.some((p) => keys.includes(p.key));

/** A 3D printer, as `deviceKind` tells one: a print's progress and a nozzle. */
export const printersPresent = (devices) =>
  Object.values(devices ?? {}).some((d) => has(d, ['progress']) && has(d, ['nozzle_temperature']));

/** The Caméras page also holds the Sécurité card (doors, what moves, the alarm):
 *  its tab shows while either has something, never hiding a door left open. */
export function camerasUseful(devices, drivers, config) {
  if ((config?.doors ?? []).length) return true;
  const people = new Set((drivers ?? []).filter((x) => PEOPLE.includes(x?.kind)).map((x) => x.instance));
  const aboutPeople = (d) => people.has(d.instance) || d.id?.startsWith('presence:') || d.id?.startsWith('telephones:');
  const moves = (d) => has(d, MOVES) || !!d.points?.some((p) => p.semantic === 'occupancy');
  return Object.values(devices ?? {}).some(
    (d) =>
      !!d?.camera ||
      has(d, CONTACT) ||
      (moves(d) && !aboutPeople(d)) ||
      (d?.label?.room ?? d?.native_room) === 'Sécurité',
  );
}
