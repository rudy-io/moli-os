// A Proxmox node as the `proxmox` driver publishes it, gathered for the
// Infra page: its node, guests (containers, virtual machines), storages,
// last backup and disks. Devices of one instance share the `<instance>:`
// prefix; the node is `<instance>:node`.

const native = (id) => id.slice(id.indexOf(':') + 1);
const vmid = (id) => Number(native(id).split('_')[1]);

/** Every Proxmox node among `devices` (a list), with its parts. */
export function proxmoxNodes(devices) {
  const mine = devices.filter((d) => d.manufacturer === 'Proxmox' && d.id.includes(':'));
  return mine
    .filter((d) => native(d.id) === 'node')
    .map((node) => {
      const instance = node.id.slice(0, node.id.indexOf(':'));
      const parts = mine.filter((d) => d.id.startsWith(`${instance}:`) && d.id !== node.id);
      const of = (prefix) => parts.filter((d) => native(d.id).startsWith(prefix));
      return {
        instance,
        node,
        guests: [...of('lxc_'), ...of('qemu_')].sort((a, b) => vmid(a.id) - vmid(b.id)),
        storages: of('storage_').sort((a, b) => native(a.id).localeCompare(native(b.id))),
        backup: parts.find((d) => native(d.id) === 'backup') ?? null,
        disks: of('disk_').sort((a, b) => native(a.id).localeCompare(native(b.id), undefined, { numeric: true })),
      };
    })
    .sort((a, b) => a.instance.localeCompare(b.instance));
}

/** What needs a look on a node: each part whose `problem` is true, with the
 *  driver's words (`detail`, null when it has none). `value(id, key)` reads
 *  a point; the page names the part and puts the words in its language. */
export function proxmoxIssues(group, value) {
  const parts = [group.node, ...group.guests, ...group.storages, group.backup, ...group.disks].filter(Boolean);
  return parts
    .filter((d) => value(d.id, 'problem') === true)
    .map((d) => ({ id: d.id, detail: value(d.id, 'detail') || null }));
}
