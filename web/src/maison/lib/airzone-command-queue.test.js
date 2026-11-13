import {test} from 'node:test';
import assert from 'node:assert/strict';
import {createAirzoneCommandQueue} from './airzone-command-queue.js';
const settle=async()=>{for(let i=0;i<8;i++)await Promise.resolve();};
function fixture(){
 let time=0,id=0,current={writable:true,online:true,generation:1,value:20};
 const timers=new Map(),sent=[],states=[];
 const queue=createAirzoneCommandQueue({now:()=>time,schedule:(fn,delay)=>{timers.set(++id,{fn,at:time+delay});return id;},cancel:id=>timers.delete(id),read:()=>current,changed:(point,state)=>states.push({point,...state}),send:(point,value)=>new Promise(resolve=>sent.push({point,value,resolve}))});
 const advance=async(ms)=>{time+=ms;for(const[key,timer]of [...timers])if(timer.at<=time){timers.delete(key);timer.fn();}await settle();};
 return {queue,sent,states,timers,advance,set:value=>{current={...current,...value};queue.observe();},last:()=>states.at(-1)};
}
test('rapid setpoints coalesce to the latest intention and ACK never confirms preflight state',async()=>{
 const f=fixture();f.queue.request('zone/target',21,{coalesce:true});f.queue.request('zone/target',22,{coalesce:true});
 await f.advance(249);assert.equal(f.sent.length,0);await f.advance(1);assert.deepEqual(f.sent.map(x=>x.value),[22]);
 f.set({value:22,generation:2});f.sent[0].resolve({ok:true,barrier:2});await settle();assert.equal(f.last().phase,'awaiting');
 f.set({generation:3});assert.equal(f.last().phase,'confirmed');assert.equal(f.timers.size,0);
});
test('one request per point is in flight, superseded ACK and failure cannot replace the latest intent',async()=>{
 for(const outcome of [{ok:true,barrier:2},{ok:false,error:'failed'}]){
 const f=fixture();f.queue.request('zone/target',21);await f.advance(0);f.queue.request('zone/target',22,{coalesce:true});f.queue.request('zone/target',23,{coalesce:true});await f.advance(300);
 assert.equal(f.sent.length,1);assert.equal(f.last().value,23);f.sent[0].resolve(outcome);await settle();await f.advance(0);assert.deepEqual(f.sent.map(x=>x.value),[21,23]);assert.equal(f.last().value,23);assert.equal(f.last().phase,'sending');
 f.sent[1].resolve({ok:true,barrier:5});await settle();f.set({value:21,generation:6});assert.equal(f.last().phase,'awaiting');f.set({value:23,generation:6});assert.equal(f.last().phase,'confirmed');
 }
});
test('capability loss and offline status stop unsent work without automatic replay',async()=>{
 for(const state of [{writable:false},{online:false}]){const f=fixture();f.queue.request('zone/on',true,{coalesce:true});f.set(state);await f.advance(500);assert.equal(f.sent.length,0);assert.equal(f.last().phase,'error');f.set({online:true,writable:true});await f.advance(500);assert.equal(f.sent.length,0);}
});
test('late response after revocation cannot report success and timeout never retries',async()=>{
 const f=fixture();f.queue.request('zone/on',true);await f.advance(0);f.set({writable:false});f.sent[0].resolve({ok:true,barrier:2});await settle();assert.equal(f.last().error,'read_only');
 const g=fixture();g.queue.request('zone/on',true);await g.advance(0);g.sent[0].resolve({ok:true,barrier:2});await settle();await g.advance(45000);assert.equal(g.last().error,'unconfirmed');g.set({value:true,generation:3});assert.equal(g.last().phase,'error');assert.equal(g.sent.length,1);
});
test('uncertain HTTP outcome, absent barrier and cancellation never become confirmed',async()=>{
 for(const result of [{ok:false,error:'network'},{ok:true}]){const f=fixture();f.queue.request('zone/on',true);await f.advance(0);f.sent[0].resolve(result);await settle();assert.equal(f.last().phase,'error');assert.equal(f.sent.length,1);}
 const f=fixture();f.queue.request('zone/on',true,{coalesce:true});f.queue.destroy();await f.advance(500);assert.equal(f.sent.length,0);assert.equal(f.last().error,'cancelled');
});
test('a later different observation clears a historic confirmation instead of presenting it as current',async()=>{
 const f=fixture();f.queue.request('zone/on',true);await f.advance(0);f.sent[0].resolve({ok:true,barrier:2});await settle();f.set({value:true,generation:3});assert.equal(f.last().phase,'confirmed');f.set({value:false,generation:4});assert.equal(f.last().phase,undefined);
});
