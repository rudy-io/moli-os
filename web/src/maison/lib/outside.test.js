import test from 'node:test';
import assert from 'node:assert/strict';
import { idsOf, pick, originOf, measures, compass } from './outside.js';

const num = (key, semantic, extra = {}) => ({ key, semantic, kind: { type: 'numeric' }, access: { read: true, write: false }, ...extra });

test('the ids of a config point', () => {
  assert.deepEqual(idsOf('z2m:0x1,infoclimat:a'), ['z2m:0x1', 'infoclimat:a']);
  assert.deepEqual(idsOf(''), []);
  assert.deepEqual(idsOf(null), []);
});

test('the point named after the meaning wins, else the only one', () => {
  const forecast = [num('feels_like', 'temperature'), num('temperature', 'temperature'), num('wind_gusts', 'wind_gust')];
  assert.equal(pick(forecast, 'temperature', 'temperature').key, 'temperature');
  assert.equal(pick(forecast, 'wind_gust', 'wind_gust').key, 'wind_gusts');
  assert.equal(pick([num('dew_point', 'temperature'), num('apparent_temperature', 'temperature')], 'temperature', 'temperature'), null);
  assert.equal(pick(forecast, 'humidity', 'humidity'), null);
});

test('a value is measured when a station gave it', () => {
  assert.equal(originOf('z2m:0x1', ['z2m:0x1']), 'measured');
  assert.equal(originOf('meteo:api', ['z2m:0x1']), 'forecast');
  assert.equal(originOf(null, ['z2m:0x1']), null);
});

test('a station shows its measures, not its settings nor its radio', () => {
  const d = {
    points: [
      num('temperature', 'temperature'),
      num('humidity', 'humidity'),
      num('temperature_calibration', 'config', { access: { read: true, write: true } }),
      num('linkquality', 'signal_strength'),
      num('comfort_temperature_min', 'temperature', { access: { read: true, write: true } }),
      { key: 'battery_low', semantic: 'battery_low', kind: { type: 'binary' }, access: { read: true } },
      { key: 'name', semantic: 'other', kind: { type: 'text' }, access: { read: true } },
    ],
  };
  assert.deepEqual(measures(d).map((p) => p.key), ['temperature', 'humidity', 'battery_low']);
  assert.deepEqual(measures(null), []);
});

test('a wind direction is a compass point', () => {
  assert.equal(compass(0), 0);
  assert.equal(compass(61), 3);
  assert.equal(compass(350), 0);
  assert.equal(compass(-90), 12);
  assert.equal(compass(null), null);
});
