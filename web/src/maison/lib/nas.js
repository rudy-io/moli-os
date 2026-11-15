/** The Synology NAS of the house, as its driver publishes it: one device for
 *  the NAS itself (`nas`) and one per volume, disk, UPS and backup task.
 *  Grouped by driver instance (one instance = one NAS). The NAS itself is
 *  always there, even asleep (it carries the wake order): a group without it
 *  is not a NAS. */
export function nasGroups(devices) {
  const groups = new Map();
  for (const d of devices) {
    if (d.manufacturer !== 'Synology') continue;
    const native = d.id.slice(d.instance.length + 1);
    const g = groups.get(d.instance) ?? { instance: d.instance, nas: null, volumes: [], disks: [], ups: null, backups: [] };
    if (native === 'nas') g.nas = d;
    else if (native === 'ups') g.ups = d;
    else if (native.startsWith('volume_')) g.volumes.push(d);
    else if (native.startsWith('disk_')) g.disks.push(d);
    else if (native.startsWith('backup_')) g.backups.push(d);
    groups.set(d.instance, g);
  }
  return [...groups.values()].filter((g) => g.nas);
}

/** Every part of a group, the NAS first. */
export const parts = (g) => [g.nas, ...g.volumes, ...g.disks, ...(g.ups ? [g.ups] : []), ...g.backups];

/** DSM writes a time as « 2026/11/14 03:46:02 »: « 14/11 03:46 ». Anything
 *  else is shown as it came. */
export function shortDate(text) {
  const m = /^(\d{4})[/-](\d{2})[/-](\d{2})[ T](\d{2}):(\d{2})/.exec(String(text ?? ''));
  return m ? `${m[3]}/${m[2]} ${m[4]}:${m[5]}` : (text ?? null);
}
