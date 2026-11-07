import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';
let vite, filterZoneSystems, zoneActivityCounts;
before(async () => {
  vite = await createServer({root:fileURLToPath(new URL('../../../',import.meta.url)),server:{middlewareMode:true,watch:null,hmr:false},logLevel:'error'});
  ({filterZoneSystems,zoneActivityCounts} = await vite.ssrLoadModule('/src/maison/lib/airzone-view.js'));
});
after(async () => { await vite?.close(); });

const zone = (id, name, online, values = {}, room = '') => ({
  id, native_name: name, native_room: room, online,
  points: Object.entries(values).map(([key, value]) => ({ key, access: {read:true}, kind:{type:typeof value === 'boolean'?'binary':'numeric'} })),
  state: Object.fromEntries(Object.entries(values).map(([key,value])=>[key,{value,ts:1}])),
});
const demand = zone('a', 'Séjour', true, {on:false, heat_demand:true}, 'Étage');
const idle = zone('b', 'Bureau', true, {on:true, temperature:18, target_temperature:22, heat_demand:false});
const offline = zone('c', 'Chambre', false, {heat_demand:true});
const unknown = zone('d', 'Entrée', null);
const missingDemand = zone('e', 'Salon', true, {on:true, temperature:15, target_temperature:22});
const systems = [{id:'one', zones:[demand,idle,offline]}, {id:'two',zones:[unknown,missingDemand]}];
const ids = (result) => result.flatMap(s=>s.zones.map(d=>d.id));

test('activity filters use reported demand, never activation or temperature differences', () => {
  assert.deepEqual(ids(filterZoneSystems(systems,{activity:'demand'})),['a']);
  assert.deepEqual(ids(filterZoneSystems(systems,{activity:'unavailable'})),['c','d']);
  assert.deepEqual(zoneActivityCounts(systems.flatMap(s=>s.zones)),{all:5,demand:1,unavailable:2});
});
test('search combines accents, local labels, rooms, system and activity without mutating inventory', () => {
  demand.label = {name:'Pièce de vie'};
  assert.deepEqual(ids(filterZoneSystems(systems,{search:'  etage ',system:'one',activity:'demand',language:'fr'})),['a']);
  assert.deepEqual(ids(filterZoneSystems(systems,{search:'piece de vie'})),['a']);
  assert.deepEqual(ids(filterZoneSystems(systems,{search:'sejour'})),[]);
  assert.deepEqual(ids(filterZoneSystems(systems,{system:'two',activity:'demand'})),[]);
  assert.equal(systems[0].zones.length,3);
});
test('empty and refreshed inventories produce current counts and no empty groups', () => {
  assert.deepEqual(filterZoneSystems([],{}),[]);
  assert.deepEqual(zoneActivityCounts([]),{all:0,demand:0,unavailable:0});
  assert.deepEqual(filterZoneSystems(systems,{system:'removed'}),[]);
  assert.equal(zoneActivityCounts([{...offline,online:true}]).demand,1);
  const selected = filterZoneSystems(systems,{system:'two'});
  assert.deepEqual(zoneActivityCounts(selected.flatMap(s=>s.zones)),{all:2,demand:0,unavailable:1});
});

test('a lost event stream cannot advertise retained demand as current, and reconnect restores it', () => {
  assert.equal(zoneActivityCounts([demand],false).demand,0);
  assert.deepEqual(ids(filterZoneSystems(systems,{activity:'demand',connected:false})),[]);
  assert.equal(zoneActivityCounts([demand],false).unavailable,0);
  assert.equal(zoneActivityCounts([demand],true).demand,1);
  assert.deepEqual(ids(filterZoneSystems(systems,{activity:'demand',connected:true})),['a']);
});
