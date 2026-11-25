import { test } from 'node:test';
import assert from 'node:assert/strict';
import { addDevice, privateHost, probeDevice, previewIsValid } from './device-add.js';

const response = (data, status = 200) => ({ ok: status >= 200 && status < 300, status, json: async () => data });
const found = {
  status: 'recognized', profile: 'meross-mss310', name: 'Prise connectée Meross avec mesure (MSS310)',
  manufacturer: 'Meross', model: 'mss315', identity: '00000000000000000000000000000001', mac: '020000000001', key: 'empty',
};

test('only a private address written plainly is asked', async () => {
  assert.equal(privateHost(' 192.168.1.20 '), '192.168.1.20');
  assert.equal(privateHost('10.0.0.7'), '10.0.0.7');
  assert.equal(privateHost('172.16.0.1'), '172.16.0.1');
  for (const host of ['8.8.8.8', '127.0.0.1', '192.168.001.20', 'plug.local', '192.168.1.256', '']) {
    assert.equal(privateHost(host), null, host);
  }
  let called = false;
  const fetcher = async () => { called = true; return response(found); };
  assert.deepEqual(await probeDevice({ host: '1.1.1.1' }, fetcher), { ok: false, error: 'invalid_host' });
  assert.equal(called, false, 'nothing leaves the browser for a public address');
});

test('a probe is a human POST, with the typed key only when there is one', async () => {
  const calls = [];
  const fetcher = async (...args) => { calls.push(args); return response(found); };
  assert.deepEqual(await probeDevice({ host: '192.168.1.20' }, fetcher), { ok: true, data: found });
  assert.deepEqual(calls[0], ['/api/integrations/devices/probe', {
    method: 'POST', headers: { 'content-type': 'application/json', 'x-moli-origin': 'ui' },
    body: JSON.stringify({ host: '192.168.1.20' }),
  }]);
  await probeDevice({ host: '192.168.1.20', key: 'fictional-key' }, fetcher);
  assert.equal(calls[1][1].body, JSON.stringify({ host: '192.168.1.20', key: 'fictional-key' }));
});

test('a refusal is a code to translate, a 403 asks for the house code', async () => {
  assert.deepEqual(await probeDevice({ host: '192.168.1.20' }, async () => response({}, 403)), { ok: false, error: 'auth', status: 403 });
  assert.deepEqual(
    await addDevice({ ...found, host: '192.168.1.20', name: 'Lampe' }, async () => response({ error: 'duplicate_device', existing_instance: 'meross-conso-1' }, 409)),
    { ok: false, error: 'duplicate_device', status: 409, existingInstance: 'meross-conso-1' },
  );
  assert.deepEqual(await probeDevice({ host: '192.168.1.20' }, async () => response({ error: '<script>' }, 500)), { ok: false, error: 'server', status: 500 });
  assert.deepEqual(await probeDevice({ host: '192.168.1.20' }, async () => response({ status: 'recognized' })), { ok: false, error: 'response', status: 200 });
});

test('an addition carries the identity the person saw, a name and a room', async () => {
  const calls = [];
  const fetcher = async (...args) => { calls.push(args); return response({ instance: 'meross-mss310-1', device: 'meross-mss310-1:0001', labelled: true }); };
  assert.deepEqual(await addDevice({ host: '192.168.1.20', identity: found.identity, name: '  ', room: '' }, fetcher), { ok: false, error: 'invalid_name' });
  const result = await addDevice({ host: '192.168.1.20', identity: found.identity, name: ' Lampe ', room: 'Bureau' }, fetcher);
  assert.equal(result.ok, true);
  assert.equal(calls.length, 1);
  assert.equal(calls[0][0], '/api/integrations/devices');
  assert.deepEqual(JSON.parse(calls[0][1].body), { host: '192.168.1.20', identity: found.identity, name: 'Lampe', room: 'Bureau' });
});

test('what the server says about the device is checked before it is shown', () => {
  assert.equal(previewIsValid(found), true);
  assert.equal(previewIsValid({ status: 'needs_key', manufacturer: 'Meross' }), true);
  assert.equal(previewIsValid({ status: 'silent' }), true);
  assert.equal(previewIsValid({ ...found, key: 'secret' }), false);
  assert.equal(previewIsValid({ ...found, mac: '02:00:00:00:00:01' }), false);
  assert.equal(previewIsValid({ ...found, name: 'a\u0000b' }), false);
  assert.equal(previewIsValid({ status: 'other' }), false);
});
