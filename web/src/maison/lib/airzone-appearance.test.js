import {test} from 'node:test';
import assert from 'node:assert/strict';
import {zoneAppearance} from './airzone-appearance.js';
const zone={online:true,availability:'online',on:true,mode:'heat',status:'idle'};
const current={state:'current'};

test('mode tint and actual demand stay distinct, with no inference from temperature gaps',()=>{
 for(const [mode,tone,icon]of [['heat','warm','fire'],['cool','cool','snowflake'],['fan','good','fan'],['dry','cool','humidity']]){
  const summary={...zone,mode,temperature:{value:10},target:{value:30}};
  const idle=zoneAppearance(summary,current);assert.equal(idle.tone,tone);assert.equal(idle.icon,icon);assert.equal(idle.demand,false);
  assert.equal(zoneAppearance({...summary,status:'demand'},current).demand,true);
  assert.equal(summary.status,'idle');
 }
});
test('off, unavailable, stale and unknown power states never carry an active mode tint',()=>{
 for(const [summary,sync]of [[{...zone,on:false},current],[{...zone,mode:'off'},current],[{...zone,on:null},current],[{...zone,online:false,availability:'offline'},current],[zone,{state:'stale'}],[zone,{state:'disconnected'}],[zone,{state:'unknown'}],[zone,null]]){
  const appearance=zoneAppearance({...summary,status:'demand'},sync);assert.equal(appearance.tone,'neutral');assert.equal(appearance.demand,false);
 }
 assert.equal(zoneAppearance({...zone,on:false},current).icon,'power');
 assert.equal(zoneAppearance({...zone,online:false,availability:'offline'},current).icon,'offline');
 assert.equal(zoneAppearance(zone,{state:'stale'}).icon,'clock');
});
test('auto and unrecognised modes do not invent heating or cooling from a target difference',()=>{
 for(const mode of ['auto','heat_cool','future_mode',null])assert.equal(zoneAppearance({...zone,mode},current).tone,'neutral');
 assert.equal(zoneAppearance({...zone,mode:'future_mode'},current).mode,null);
});
