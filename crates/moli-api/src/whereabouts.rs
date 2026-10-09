//! Where the household's people are: one device per person
//! (`personnes:<id>`), from their phones, and one for the house
//! (`personnes:maison`). Automations use them like any device: « quand
//! Élodie arrive au travail » is `personnes:elodie/zone` becoming « Travail »,
//! « quand tout le monde est parti » is `personnes:maison/everyone_away`.
//!
//! - home: a phone of theirs says it is home (home Wi-Fi, or inside the home
//!   zone); away when every phone of theirs says it is away;
//! - position, accuracy, battery: from their phone that spoke last;
//! - zone: the home zone when home, else the smallest zone containing the
//!   position, else « Ailleurs ».

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use moli_core::{Access, Device, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::{BoxFuture, Driver, DriverCtx, Hub};

use crate::people::{People, Zone};

/// The instance: its devices are `personnes:<person id>` and `personnes:maison`.
pub const INSTANCE: &str = "personnes";
const HOUSE: &str = "maison";
const EVERY: Duration = Duration::from_secs(15);

pub(crate) struct Whereabouts {
    pub(crate) people: Arc<People>,
    pub(crate) hub: Hub,
    pub(crate) phones: Option<moli_phones::Gateway>,
}

fn spec(key: &str, label: &str, kind: Kind, unit: Option<Unit>, semantic: Semantic) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: Access {
            read: true,
            write: false,
        },
        unit,
        semantic,
    }
}

fn number() -> Kind {
    Kind::Numeric {
        min: None,
        max: None,
        step: None,
    }
}

/// Metres between two positions (haversine).
pub(crate) fn distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    let r = 6_371_000.0_f64;
    let (la1, la2) = (a.0.to_radians(), b.0.to_radians());
    let dla = la2 - la1;
    let dlo = (b.1 - a.1).to_radians();
    let h = (dla / 2.0).sin().powi(2) + la1.cos() * la2.cos() * (dlo / 2.0).sin().powi(2);
    2.0 * r * h.sqrt().asin()
}

/// The zone a position is in: the smallest that contains it.
pub(crate) fn zone_of(zones: &[Zone], at: (f64, f64)) -> Option<&Zone> {
    zones
        .iter()
        .filter(|z| distance((z.latitude, z.longitude), at) <= z.radius)
        .min_by(|a, b| a.radius.total_cmp(&b.radius))
}

/// What one phone tells, read from the hub.
#[derive(Debug, Default, Clone)]
struct Phone {
    home: Option<bool>,
    at: Option<(f64, f64)>,
    accuracy: Option<f64>,
    battery: Option<f64>,
    /// When it last spoke (ms).
    ts: u64,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Where {
    pub(crate) home: Option<bool>,
    pub(crate) zone: Option<String>,
    pub(crate) at: Option<(f64, f64)>,
    pub(crate) accuracy: Option<f64>,
    pub(crate) battery: Option<f64>,
    pub(crate) since: Option<u64>,
}

impl Whereabouts {
    /// The zones, the home one included (from the phones' home when the
    /// owner drew none named « maison »).
    fn zones(&self) -> (Vec<Zone>, Option<Zone>) {
        let mut zones = self.people.book().zones;
        let home = zones.iter().find(|z| z.id == HOUSE).cloned().or_else(|| {
            let h = self.phones.as_ref()?.home()?;
            let z = Zone {
                id: HOUSE.into(),
                name: moli_i18n::tr!("serveur.personnes.zone_maison"),
                latitude: h.latitude,
                longitude: h.longitude,
                radius: h.radius,
                icon: Some("home".into()),
            };
            zones.push(z.clone());
            Some(z)
        });
        (zones, home)
    }

    /// Each phone's word, by the person it belongs to.
    fn phones(&self) -> BTreeMap<String, Vec<Phone>> {
        let Some(gateway) = &self.phones else {
            return BTreeMap::new();
        };
        let mut by_person: BTreeMap<String, Vec<Phone>> = BTreeMap::new();
        let ids: HashSet<String> = gateway.list().into_iter().map(|p| p.id).collect();
        for view in self.hub.snapshot().devices {
            let id = view.device.id.as_str();
            let native = id.split_once(':').map_or(id, |(_, n)| n);
            if !ids.contains(native) {
                continue;
            }
            let Some(person) = view.state.get("person").and_then(|s| match &s.value {
                Value::Text(t) => Some(t.to_string()),
                _ => None,
            }) else {
                continue;
            };
            let num = |k: &str| view.state.get(k).and_then(|s| s.value.as_f64());
            let ts = view.state.values().map(|s| s.ts).max().unwrap_or(0);
            let phone = Phone {
                home: view.state.get("home").and_then(|s| match s.value {
                    Value::Bool(b) => Some(b),
                    _ => None,
                }),
                at: num("latitude").zip(num("longitude")),
                accuracy: num("accuracy"),
                battery: num("battery"),
                ts,
            };
            by_person.entry(person).or_default().push(phone);
        }
        by_person
    }
}

/// Where a person is, from their phones.
fn locate(phones: &[Phone], zones: &[Zone], home_zone: Option<&Zone>) -> Where {
    let home = if phones.iter().any(|p| p.home == Some(true)) {
        Some(true)
    } else if phones.iter().any(|p| p.home == Some(false)) {
        Some(false)
    } else {
        None
    };
    let latest = phones
        .iter()
        .filter(|p| p.at.is_some())
        .max_by_key(|p| p.ts);
    let at = latest.and_then(|p| p.at);
    let zone = match (home, at) {
        (Some(true), _) => Some(home_zone.map_or_else(
            || moli_i18n::tr!("serveur.personnes.zone_maison"),
            |z| z.name.clone(),
        )),
        (_, Some(at)) => Some(
            zone_of(zones, at)
                .filter(|z| z.id != HOUSE || home != Some(false))
                .map_or_else(
                    || moli_i18n::tr!("serveur.personnes.ailleurs"),
                    |z| z.name.clone(),
                ),
        ),
        (Some(false), None) => Some(moli_i18n::tr!("serveur.personnes.ailleurs")),
        (None, None) => None,
    };
    Where {
        home,
        zone,
        at,
        accuracy: latest.and_then(|p| p.accuracy),
        battery: phones.iter().max_by_key(|p| p.ts).and_then(|p| p.battery),
        since: None,
    }
}

fn person_device(ctx: &DriverCtx, id: &str, name: &str) -> Device {
    Device {
        id: ctx.device_id(id),
        instance: ctx.instance().clone(),
        native_name: name.into(),
        manufacturer: Some("Moli".into()),
        model: Some(moli_i18n::tr!("serveur.personnes.modele").into()),
        description: None,
        native_room: None,
        members: Vec::new(),
        points: vec![
            spec(
                "home",
                &moli_i18n::tr!("pilotes.phones.a_la_maison"),
                Kind::Binary,
                None,
                Semantic::Occupancy,
            ),
            spec(
                "zone",
                &moli_i18n::tr!("serveur.personnes.point_zone"),
                Kind::Text,
                None,
                Semantic::Other,
            ),
            spec("latitude", "Latitude", number(), None, Semantic::Other),
            spec("longitude", "Longitude", number(), None, Semantic::Other),
            spec(
                "accuracy",
                &moli_i18n::tr!("serveur.personnes.point_precision"),
                number(),
                Some(Unit::Other("m".into())),
                Semantic::Other,
            ),
            spec(
                "battery",
                &moli_i18n::tr!("serveur.personnes.point_batterie"),
                number(),
                Some(Unit::Percent),
                Semantic::Battery,
            ),
            spec(
                "since",
                &moli_i18n::tr!("serveur.personnes.point_depuis"),
                number(),
                None,
                Semantic::Other,
            ),
        ],
    }
}

fn house_device(ctx: &DriverCtx) -> Device {
    Device {
        id: ctx.device_id(HOUSE),
        instance: ctx.instance().clone(),
        native_name: moli_i18n::tr!("serveur.personnes.foyer").into(),
        manufacturer: Some("Moli".into()),
        model: Some(moli_i18n::tr!("serveur.personnes.modele_foyer").into()),
        description: None,
        native_room: None,
        members: Vec::new(),
        points: vec![
            spec(
                "count",
                &moli_i18n::tr!("serveur.personnes.point_nombre"),
                number(),
                None,
                Semantic::Other,
            ),
            spec(
                "anyone_home",
                &moli_i18n::tr!("serveur.personnes.point_quelquun"),
                Kind::Binary,
                None,
                Semantic::Occupancy,
            ),
            spec(
                "everyone_away",
                &moli_i18n::tr!("serveur.personnes.point_tous_partis"),
                Kind::Binary,
                None,
                Semantic::Other,
            ),
        ],
    }
}

fn opt_num(v: Option<f64>) -> Value {
    v.map_or(Value::Null, Value::Float)
}

impl Whereabouts {
    fn step(&self, ctx: &DriverCtx, published: &mut BTreeMap<String, (String, Where)>) {
        let book = self.people.book();
        let (zones, home_zone) = self.zones();
        let phones = self.phones();
        let mut seen = HashSet::new();
        let (mut count, mut known) = (0u32, false);
        for person in &book.people {
            seen.insert(person.id.clone());
            let id = ctx.device_id(&person.id);
            let mut now = locate(
                phones.get(&person.id).map_or(&[][..], Vec::as_slice),
                &zones,
                home_zone.as_ref(),
            );
            let previous = published.get(&person.id);
            if previous.is_none_or(|(name, _)| *name != person.name) {
                ctx.upsert_device(person_device(ctx, &person.id, &person.name));
                ctx.set_availability(&id, true);
            }
            // « since »: when the zone last changed.
            now.since = match previous {
                Some((_, before)) if before.zone == now.zone => before.since,
                _ => Some(moli_core::now_ms()),
            };
            if now.home == Some(true) {
                count += 1;
            }
            known |= now.home.is_some();
            if previous.is_none_or(|(_, before)| *before != now) {
                ctx.set_state(&id, "home", now.home.map_or(Value::Null, Value::Bool));
                ctx.set_state(
                    &id,
                    "zone",
                    now.zone
                        .clone()
                        .map_or(Value::Null, |z| Value::Text(z.into())),
                );
                ctx.set_state(&id, "latitude", opt_num(now.at.map(|a| a.0)));
                ctx.set_state(&id, "longitude", opt_num(now.at.map(|a| a.1)));
                ctx.set_state(&id, "accuracy", opt_num(now.accuracy));
                ctx.set_state(&id, "battery", opt_num(now.battery));
                #[allow(clippy::cast_precision_loss)]
                ctx.set_state(&id, "since", opt_num(now.since.map(|s| s as f64)));
            }
            published.insert(person.id.clone(), (person.name.clone(), now));
        }
        // Someone removed: their device goes.
        let gone: Vec<String> = published
            .keys()
            .filter(|k| !seen.contains(*k))
            .cloned()
            .collect();
        for id in gone {
            ctx.remove_device(&ctx.device_id(&id));
            published.remove(&id);
        }
        let house = ctx.device_id(HOUSE);
        ctx.set_state(&house, "count", Value::Int(i64::from(count)));
        if known {
            ctx.set_state(&house, "anyone_home", Value::Bool(count > 0));
            ctx.set_state(&house, "everyone_away", Value::Bool(count == 0));
        }
    }
}

impl Driver for Whereabouts {
    fn kind(&self) -> &'static str {
        "people"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            ctx.upsert_device(house_device(ctx));
            ctx.set_availability(&ctx.device_id(HOUSE), true);
            ctx.ready();
            let mut published = BTreeMap::new();
            let mut tick = tokio::time::interval(EVERY);
            loop {
                tokio::select! {
                    command = ctx.next_command() => match command {
                        // Read-only devices: nothing to obey.
                        Some(command) => command.reply(Err("read only".into())),
                        None => return Ok(()),
                    },
                    _ = tick.tick() => self.step(ctx, &mut published),
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zone(id: &str, name: &str, at: (f64, f64), radius: f64) -> Zone {
        Zone {
            id: id.into(),
            name: name.into(),
            latitude: at.0,
            longitude: at.1,
            radius,
            icon: None,
        }
    }

    #[test]
    fn distances_are_metres() {
        let d = distance((48.8584, 2.2945), (48.8606, 2.3376));
        assert!((3100.0..3250.0).contains(&d), "{d}");
    }

    #[test]
    fn a_person_is_where_their_phones_say() {
        let home = zone("maison", "Maison", (43.0, 3.0), 150.0);
        let work = zone("travail", "Travail", (43.01, 3.0), 300.0);
        let city = zone("ville", "Ville", (43.0, 3.0), 5000.0);
        let zones = vec![home.clone(), work, city];
        let phone = |home: Option<bool>, at: Option<(f64, f64)>, ts: u64| Phone {
            home,
            at,
            accuracy: Some(10.0),
            battery: Some(50.0),
            ts,
        };
        // On the home Wi-Fi: home, whatever an old position says.
        let w = locate(
            &[phone(Some(true), Some((43.01, 3.0)), 1)],
            &zones,
            Some(&home),
        );
        assert_eq!((w.home, w.zone.as_deref()), (Some(true), Some("Maison")));
        // Away, at work: the smallest zone wins over the city around it.
        let w = locate(
            &[phone(Some(false), Some((43.01, 3.0)), 1)],
            &zones,
            Some(&home),
        );
        assert_eq!(w.zone.as_deref(), Some("Travail"));
        // Away, in town.
        let w = locate(
            &[phone(Some(false), Some((43.02, 3.02)), 1)],
            &zones,
            Some(&home),
        );
        assert_eq!(w.zone.as_deref(), Some("Ville"));
        // Two phones: one home is enough.
        let w = locate(
            &[
                phone(Some(false), Some((44.0, 3.0)), 5),
                phone(Some(true), None, 1),
            ],
            &zones,
            Some(&home),
        );
        assert_eq!(w.home, Some(true));
        // Nothing known.
        assert_eq!(locate(&[], &zones, None), Where::default());
    }
}
