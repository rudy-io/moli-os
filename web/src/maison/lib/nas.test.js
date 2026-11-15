import { test } from 'node:test';
import assert from 'node:assert/strict';
import { nasGroups, parts, shortDate } from './nas.js';

const dev = (instance, native, manufacturer = 'Synology') => ({ id: `${instance}:${native}`, instance, manufacturer, points: [] });

test('the devices of one instance make one NAS, by what each part is', () => {
  const groups = nasGroups([dev('nas', 'nas'), dev('nas', 'volume_1'), dev('nas', 'disk_sda'), dev('nas', 'disk_sdb'), dev('nas', 'ups'), dev('nas', 'backup_8')]);
  assert.equal(groups.length, 1);
  const [g] = groups;
  assert.equal(g.nas.id, 'nas:nas');
  assert.deepEqual(g.volumes.map((d) => d.id), ['nas:volume_1']);
  assert.deepEqual(g.disks.map((d) => d.id), ['nas:disk_sda', 'nas:disk_sdb']);
  assert.equal(g.ups.id, 'nas:ups');
  assert.deepEqual(g.backups.map((d) => d.id), ['nas:backup_8']);
  assert.equal(parts(g).length, 6);
});

test('two instances make two NAS; other makers and a group without its NAS are not NAS', () => {
  const groups = nasGroups([dev('a', 'nas'), dev('a', 'volume_1'), dev('b', 'nas'), dev('c', 'volume_1'), dev('d', 'nas', 'Other')]);
  assert.deepEqual(groups.map((g) => g.instance), ['a', 'b']);
});

test('a DSM time is shortened, anything else is left alone', () => {
  assert.equal(shortDate('2026/11/14 03:46:02'), '14/11 03:46');
  assert.equal(shortDate('2026-11-14T03:46:02'), '14/11 03:46');
  assert.equal(shortDate('hier'), 'hier');
  assert.equal(shortDate(null), null);
});
