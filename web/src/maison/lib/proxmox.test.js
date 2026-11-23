import { test } from 'node:test';
import assert from 'node:assert/strict';
import { proxmoxNodes, proxmoxIssues } from './proxmox.js';

const dev = (id, manufacturer = 'Proxmox', native_name = id) => ({ id, manufacturer, native_name, points: [] });

const devices = [
  dev('pve:node', 'Proxmox', 'Proxmox pve'),
  dev('pve:lxc_105', 'Proxmox', 'cameras'),
  dev('pve:lxc_100', 'Proxmox', 'medias'),
  dev('pve:qemu_200', 'Proxmox', 'vm'),
  dev('pve:storage_local-lvm'),
  dev('pve:storage_local'),
  dev('pve:backup'),
  dev('pve:disk_10'),
  dev('pve:disk_2'),
  dev('nas:nas', 'Synology'),
  dev('lab:node', 'Proxmox', 'Proxmox lab'),
];

test('a node gathers its own parts, in order, and nothing of another', () => {
  const nodes = proxmoxNodes(devices);
  assert.deepEqual(nodes.map((n) => n.instance), ['lab', 'pve']);
  const pve = nodes[1];
  assert.deepEqual(pve.guests.map((d) => d.id), ['pve:lxc_100', 'pve:lxc_105', 'pve:qemu_200']);
  assert.deepEqual(pve.storages.map((d) => d.id), ['pve:storage_local', 'pve:storage_local-lvm']);
  assert.equal(pve.backup.id, 'pve:backup');
  assert.deepEqual(pve.disks.map((d) => d.id), ['pve:disk_2', 'pve:disk_10']);
  assert.equal(nodes[0].guests.length, 0);
  assert.equal(nodes[0].backup, null);
});

test('no Proxmox, no node', () => {
  assert.deepEqual(proxmoxNodes([dev('nas:nas', 'Synology')]), []);
});

test('the issues are the parts in trouble, with their words', () => {
  const pve = proxmoxNodes(devices)[1];
  const values = {
    'pve:lxc_105/problem': true,
    'pve:lxc_105/detail': 'Mémoire à 95 %',
    'pve:backup/problem': true,
    'pve:backup/detail': '',
    'pve:lxc_100/problem': false,
  };
  const value = (id, key) => values[`${id}/${key}`] ?? null;
  assert.deepEqual(proxmoxIssues(pve, value), [
    { id: 'pve:lxc_105', detail: 'Mémoire à 95 %' },
    { id: 'pve:backup', detail: null },
  ]);
});
