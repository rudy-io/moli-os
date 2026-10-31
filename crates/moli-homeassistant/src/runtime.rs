//! Supervised polling and the only outbound write boundary.
use std::time::{Duration, Instant};

use moli_core::{Device, Value};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};

use crate::{
    Entity, HomeAssistant,
    model::{self, Service, Snapshot},
    net::Client,
};

const POLL_BUDGET: Duration = Duration::from_millis(1500);

#[derive(Debug)]
struct Live {
    snapshot: Snapshot,
    received: Instant,
}
impl Live {
    fn service(
        &self,
        e: &Entity,
        key: &str,
        value: &Value,
        now: Instant,
        freshness: Duration,
    ) -> anyhow::Result<Service> {
        anyhow::ensure!(
            now.saturating_duration_since(self.received) < freshness,
            "stale bridge snapshot"
        );
        model::command(e, &self.snapshot, key, value)
    }
}

impl Driver for HomeAssistant {
    fn kind(&self) -> &'static str {
        "homeassistant"
    }
    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(self, ctx))
    }
}

async fn park(ctx: &DriverCtx, key: &str) -> anyhow::Result<()> {
    for id in ctx.devices() {
        ctx.set_availability(&id, false);
    }
    ctx.wait_for(moli_i18n::tr(key));
    ctx.cancelled().await;
    Ok(())
}

async fn run(driver: &HomeAssistant, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    // Restored readings are stale until independently refreshed in this run.
    for id in ctx.known_devices() {
        ctx.set_availability(&id, false);
    }
    let config = &driver.config;
    let Some(token) = ctx
        .secret("token")
        .filter(|s| !s.is_empty() && s.len() <= 8192)
    else {
        return park(ctx, "homeassistant.credentials").await;
    };
    if ctx.secret("token_endpoint").as_deref() != Some(config.token_endpoint().as_str()) {
        return park(ctx, "homeassistant.credentials").await;
    }
    let client = Client::new(config, &token)?;
    let mut live: Vec<Option<Live>> = (0..config.entities.len()).map(|_| None).collect();
    let freshness = Duration::from_secs(config.poll_seconds * 2);
    let period = Duration::from_secs(config.poll_seconds)
        / u32::try_from(config.entities.len()).expect("validated entity count");
    let mut ticks = tokio::time::interval(period);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let has_climate = config.entities.iter().any(|e| e.domain == "climate");
    let mut global_unit = None;
    let mut cursor = 0;
    let mut transport_failures = 0;
    ctx.ready();
    loop {
        tokio::select! {
            command=ctx.next_command()=> {
                let Some(command)=command else {return Ok(())};
                let index=config.entities.iter().position(|e| ctx.device_id(&e.native_id)==command.device.id);
                let Some(index)=index else {command.reply(Err(moli_i18n::tr!("homeassistant.refused")));continue};
                if execute(ctx,&client,&config.entities[index],live[index].as_ref(),freshness,command).await {
                    return park(ctx,"homeassistant.auth").await;
                }
            }
            _=ticks.tick()=> {
                // One bounded configuration read per sweep, never per entity.
                // Other domains do not need or fetch climate unit metadata.
                if cursor == 0 && has_climate {
                    let response=tokio::select! {
                        ()=ctx.cancelled()=>return Ok(()),
                        response=client.call(http::Method::GET,"/api/config",None,Instant::now()+POLL_BUDGET)=>response,
                    };
                    let unit=match response {
                        Ok((401|403,_))=>return park(ctx,"homeassistant.auth").await,
                        Ok((200,body))=>serde_json::from_slice(&body).ok().and_then(|raw|model::TemperatureUnit::from_config(&raw)),
                        _=>None,
                    };
                    if unit != global_unit {
                        // A changed or lost unit invalidates cached command
                        // conversions until each climate has a fresh snapshot.
                        for (entry, entity) in live.iter_mut().zip(&config.entities) {
                            if entity.domain == "climate" {
                                *entry=None;
                                ctx.set_availability(&ctx.device_id(&entity.native_id),false);
                            }
                        }
                    }
                    global_unit=unit;
                }
                let index=cursor;cursor=(cursor+1)%config.entities.len();
                let e=&config.entities[index];let id=ctx.device_id(&e.native_id);
                let path=format!("/api/states/{}",e.entity_id);
                let result=tokio::select! {
                    ()=ctx.cancelled()=>return Ok(()),
                    response=client.call(http::Method::GET,&path,None,Instant::now()+POLL_BUDGET)=>response,
                };
                match result {
                    Ok((401|403,_))=>return park(ctx,"homeassistant.auth").await,
                    Ok((200,body))=> {
                        transport_failures=0;
                        let snapshot=serde_json::from_slice(&body).ok().and_then(|raw|model::decode(e,&raw,global_unit).ok());
                        if let Some(snapshot)=snapshot {
                            ctx.upsert_device(Device{id:id.clone(),instance:ctx.instance().clone(),native_name:e.native_id.as_str().into(),
                                manufacturer:Some("Home Assistant".into()),model:Some(format!("Home Assistant {}",e.domain).into()),description:None,
                                native_room:Some(e.room.as_str().into()),members:vec![],points:snapshot.points.clone()});
                            for (key,value) in &snapshot.values {ctx.set_state(&id,key,value.clone());}
                            ctx.set_availability(&id,true);
                            live[index]=Some(Live{snapshot,received:Instant::now()});
                        } else {ctx.set_availability(&id,false);live[index]=None;}
                    }
                    Ok(_)=>{transport_failures=0;ctx.set_availability(&id,false);live[index]=None;}
                    Err(_)=> {
                        ctx.set_availability(&id,false);live[index]=None;transport_failures+=1;
                        if transport_failures>=config.entities.len()*2 {anyhow::bail!(moli_i18n::tr!("homeassistant.network"));}
                    }
                }
                // A slow sweep must not claim restored or expired states are live.
                for (entry,entity) in live.iter().zip(&config.entities) {
                    if entry.as_ref().is_some_and(|s|s.received.elapsed()>=freshness) {ctx.set_availability(&ctx.device_id(&entity.native_id),false);}
                }
            }
        }
    }
}

/// Returns true for authentication rejection, causing the whole driver to park.
async fn execute(
    ctx: &DriverCtx,
    client: &Client,
    e: &Entity,
    live: Option<&Live>,
    freshness: Duration,
    command: CommandRequest,
) -> bool {
    if command.abandoned() {
        command.reply(Err(moli_i18n::tr!("homeassistant.deadline")));
        return false;
    }
    let service = live.and_then(|s| {
        s.service(e, &command.key, &command.value, Instant::now(), freshness)
            .ok()
    });
    let Some(service) = service else {
        command.reply(Err(moli_i18n::tr!("homeassistant.refused")));
        return false;
    };
    // Recheck abandonment immediately before constructing the transport future.
    if command.abandoned() {
        command.reply(Err(moli_i18n::tr!("homeassistant.deadline")));
        return false;
    }
    let response = tokio::select! {
        ()=ctx.cancelled()=>Err(anyhow::anyhow!(moli_i18n::tr!("homeassistant.deadline"))),
        response=client.call(http::Method::POST,&service.path,Some(&service.body),command.deadline)=>response,
    };
    let auth = matches!(response, Ok((401 | 403, _)));
    let result = match response {
        Ok((200..=299, _)) => Ok(()),
        Ok(_) => Err(moli_i18n::tr!("homeassistant.dispatch")),
        Err(e) => Err(e.to_string()),
    };
    command.reply(result);
    auth
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fresh_snapshot_is_required_and_expires_at_the_boundary() {
        let entity:Entity=serde_json::from_value(json!({"native_id":"stable","entity_id":"switch.example","domain":"switch","room":"Workshop","safety":"ordinary","writable":["on"]})).unwrap();
        let raw = json!({"entity_id":"switch.example","state":"off","attributes":{}});
        let now = Instant::now();
        let live = Live {
            snapshot: model::decode(&entity, &raw, None).unwrap(),
            received: now,
        };
        assert!(
            live.service(
                &entity,
                "on",
                &Value::Bool(true),
                now + Duration::from_secs(9),
                Duration::from_secs(10)
            )
            .is_ok()
        );
        assert!(
            live.service(
                &entity,
                "on",
                &Value::Bool(true),
                now + Duration::from_secs(10),
                Duration::from_secs(10)
            )
            .is_err()
        );
    }
}
