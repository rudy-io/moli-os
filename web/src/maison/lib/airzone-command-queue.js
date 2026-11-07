/** Airzone intentions are not observations. One request per point may be in
 * flight; newer intentions replace unsent ones. An ACK only starts the wait
 * for a matching, causally newer entity reading. No physical retry is automatic. */
export function createAirzoneCommandQueue({send,read,changed,now=Date.now,schedule=setTimeout,cancel=clearTimeout,coalesceMs=250,confirmationMs=45000}) {
  const entries=new Map();
  const active=new Set();
  const pending=(entry)=>['queued','sending','awaiting'].includes(entry?.phase);
  const publish=(entry)=>changed(entry.point,{value:entry.value,phase:entry.phase,error:entry.error ?? null});
  function stopTimer(entry) {if(entry.timer!=null){cancel(entry.timer);entry.timer=null;}}
  function fail(entry,error) {stopTimer(entry);entry.phase='error';entry.error=error;publish(entry);}
  function observe(point) {
    const entry=entries.get(point);
    if(!entry)return;
    const current=read(point);
    if(entry.phase==='confirmed'&&(!current?.writable||!Object.is(current.value,entry.value))){entries.delete(point);changed(point,null);return;}
    if(!pending(entry))return;
    if(!current?.writable || !current.online) {fail(entry,!current?.writable?'read_only':'offline');return;}
    if(entry.phase==='awaiting' && Number.isSafeInteger(current.generation) && current.generation>entry.barrier && Object.is(current.value,entry.value)) {
      stopTimer(entry);entry.phase='confirmed';publish(entry);
    }
  }
  function arm(entry) {
    stopTimer(entry);
    entry.timer=schedule(()=>{entry.timer=null;void flush(entry.point);},Math.max(0,entry.at-now()));
  }
  async function flush(point) {
    const entry=entries.get(point);
    if(active.has(point)||entry?.phase!=='queued')return;
    const current=read(point);
    if(!current?.writable||!current.online){fail(entry,!current?.writable?'read_only':'offline');return;}
    active.add(point);entry.phase='sending';publish(entry);
    let result;
    try {result=await send(point,entry.value);} catch {result={ok:false,error:'network'};}
    active.delete(point);
    const latest=entries.get(point);
    if(latest!==entry || entry.phase!=='sending') {
      if(latest?.phase==='queued')arm(latest);
      return;
    }
    if(!result.ok){fail(entry,result.error ?? 'failed');return;}
    if(!Number.isSafeInteger(result.barrier)||result.barrier<0){fail(entry,'unconfirmed');return;}
    entry.phase='awaiting';entry.barrier=result.barrier;publish(entry);
    entry.timer=schedule(()=>{if(entries.get(point)===entry&&entry.phase==='awaiting')fail(entry,'unconfirmed');},confirmationMs);
    observe(point);
  }
  return {
    request(point,value,{coalesce=false}={}) {
      const previous=entries.get(point);
      if(previous)stopTimer(previous);
      if(!previous && entries.size>=128) {
        const removable=[...entries].find(([,entry])=>!pending(entry));
        if(removable){entries.delete(removable[0]);changed(removable[0],null);}else return false;
      }
      const entry={point,value,phase:'queued',at:now()+(coalesce?coalesceMs:0),timer:null};
      entries.set(point,entry);publish(entry);arm(entry);return true;
    },
    observe(point) {if(point)observe(point);else for(const key of entries.keys())observe(key);},
    destroy() {for(const entry of entries.values()){stopTimer(entry);if(pending(entry))fail(entry,'cancelled');}entries.clear();},
  };
}
