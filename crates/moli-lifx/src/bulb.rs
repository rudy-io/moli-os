//! One bulb: what it can do, its points, and the orders it takes.

use std::collections::BTreeSet;
use std::net::SocketAddrV4;

use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::DriverCtx;
use tokio::time::Instant;

use crate::link::{self, Answer, Link};
use crate::products::{self, Product};
use crate::proto::{self, Hsbk, LightState, Parts, Reply, Serial};

pub fn serial_hex(serial: Serial) -> String {
    serial.iter().fold(String::with_capacity(12), |mut out, b| {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// A bulb this run talks to.
pub struct Bulb {
    pub serial: Serial,
    pub id: DeviceId,
    pub at: SocketAddrV4,
    /// What `GetVersion` said (vendor, product), if it answered.
    pub version: Option<(u32, u32)>,
    pub firmware: Option<(u16, u16)>,
    pub name: String,
    pub room: Option<String>,
    /// Messages its firmware said it does not know: not sent again.
    pub unhandled: BTreeSet<u16>,
    /// Its colour and power as last read or set.
    pub last: Option<(Hsbk, bool)>,
    pub misses: u32,
    pub online: bool,
    pub polls: u32,
    /// No reading before this: a fade is under way.
    pub settle_until: Instant,
}

impl Bulb {
    pub fn new(ctx: &DriverCtx, serial: Serial, at: SocketAddrV4, state: &LightState) -> Self {
        let native = serial_hex(serial);
        Self {
            serial,
            id: ctx.device_id(&native),
            at,
            version: None,
            firmware: None,
            name: state
                .label
                .clone()
                .unwrap_or_else(|| moli_i18n::tr!("pilotes.lifx.ampoule", serial = native)),
            room: None,
            unhandled: BTreeSet::new(),
            last: Some((state.color, state.on)),
            misses: 0,
            online: true,
            polls: 0,
            settle_until: Instant::now(),
        }
    }

    pub fn product(&self) -> Option<&'static Product> {
        self.version
            .and_then(|(vendor, pid)| products::find(vendor, pid))
    }

    /// Whether it takes a colour, and its kelvin range if the temperature
    /// can be set. Unknown model: neither.
    pub fn caps(&self) -> (bool, Option<(u16, u16)>) {
        match self.product() {
            Some(p) => {
                let (min, max) = p.kelvin(self.firmware);
                (p.color, (min < max).then_some((min, max)))
            }
            None => (false, None),
        }
    }

    /// Something is still to ask: model, firmware, group.
    pub fn incomplete(&self) -> bool {
        self.product().is_none()
            || self.firmware.is_none()
            || (self.room.is_none() && !self.unhandled.contains(&proto::GET_GROUP))
    }

    fn model(&self) -> String {
        match (self.product(), self.version) {
            (Some(p), _) => p.name.to_owned(),
            (None, Some((_, pid))) => moli_i18n::tr!("pilotes.lifx.produit", pid = pid),
            (None, None) => "LIFX".to_owned(),
        }
    }

    pub fn device(&self, ctx: &DriverCtx) -> Device {
        let (color, kelvin) = self.caps();
        let mut points = vec![
            spec(
                "on",
                &moli_i18n::tr!("pilotes.lifx.allumee"),
                Kind::Binary,
                None,
                Semantic::OnOff,
            ),
            spec(
                "brightness",
                &moli_i18n::tr!("pilotes.lifx.luminosite"),
                range(1.0, 100.0),
                Some(Unit::Percent),
                Semantic::Brightness,
            ),
        ];
        if let Some((min, max)) = kelvin {
            points.push(spec(
                "color_temp",
                &moli_i18n::tr!("pilotes.lifx.temperature_couleur"),
                range(f64::from(min), f64::from(max)),
                Unit::parse("K"),
                Semantic::ColorTemp,
            ));
        }
        if color {
            points.push(spec(
                "color",
                &moli_i18n::tr!("pilotes.lifx.couleur"),
                Kind::Text,
                None,
                Semantic::Color,
            ));
        }
        let mut signal = spec(
            "rssi",
            &moli_i18n::tr!("pilotes.lifx.signal"),
            Kind::Numeric {
                min: None,
                max: None,
                step: Some(1.0),
            },
            Unit::parse("dBm"),
            Semantic::SignalStrength,
        );
        signal.access.write = false;
        points.push(signal);
        Device {
            id: self.id.clone(),
            instance: ctx.instance().clone(),
            native_name: self.name.as_str().into(),
            manufacturer: Some("LIFX".into()),
            model: Some(self.model().into()),
            description: None,
            native_room: self.room.as_deref().map(Into::into),
            members: Vec::new(),
            points,
        }
    }

    /// Its points' values in this reading.
    pub fn values(&self, state: &LightState) -> Vec<(&'static str, Value)> {
        let (color, kelvin) = self.caps();
        let mut out = vec![
            ("on", Value::Bool(state.on)),
            (
                "brightness",
                Value::Int(proto::percent(state.color.brightness)),
            ),
        ];
        if kelvin.is_some() {
            out.push(("color_temp", Value::Int(i64::from(state.color.kelvin))));
        }
        if color {
            out.push(("color", Value::from(proto::hex(state.color).as_str())));
        }
        out
    }

    /// One question to this bulb.
    pub async fn ask(
        &mut self,
        link: &mut Link,
        (kind, expect): (u16, u16),
        (until, retry): (Instant, bool),
    ) -> Answer {
        let answer = link
            .ask(
                self.at,
                self.serial,
                (kind, &[]),
                (Reply::Response, expect),
                (until, retry),
            )
            .await;
        if matches!(answer, Answer::Unhandled) {
            self.unhandled.insert(kind);
        }
        answer
    }

    /// Model, firmware and group, as far as they answer (each asked once,
    /// the group with resends when `meeting`: the guard knows a protected
    /// room by it; what is missing is asked again at the next search). The
    /// model is asked again only when the firmware changed or it is
    /// unknown, and kept until a new answer replaces it. Whether anything
    /// changed.
    pub async fn complete(&mut self, link: &mut Link, meeting: bool) -> bool {
        let before = (self.version, self.firmware, self.room.clone());
        let once = || (Instant::now() + link::ONCE, false);
        let mut ask_version = self.product().is_none();
        if let Some(m) = self
            .ask(
                link,
                (proto::GET_HOST_FIRMWARE, proto::STATE_HOST_FIRMWARE),
                once(),
            )
            .await
            .message()
            && let Some(firmware) = proto::firmware(&m.payload)
        {
            // Changed, not merely found after a lost answer.
            if self.firmware.is_some_and(|known| known != firmware) {
                ask_version = true;
            }
            self.firmware = Some(firmware);
        }
        if ask_version
            && let Some(m) = self
                .ask(link, (proto::GET_VERSION, proto::STATE_VERSION), once())
                .await
                .message()
            && let Some(version) = proto::version(&m.payload)
        {
            self.version = Some(version);
        }
        if self.room.is_none()
            && !self.unhandled.contains(&proto::GET_GROUP)
            && let Some(m) = self
                .ask(
                    link,
                    (proto::GET_GROUP, proto::STATE_GROUP),
                    if meeting {
                        (Instant::now() + link::READ, true)
                    } else {
                        once()
                    },
                )
                .await
                .message()
        {
            self.room = proto::group(&m.payload);
        }
        before != (self.version, self.firmware, self.room.clone())
    }

    /// The Wi-Fi signal in dBm, asked once.
    pub async fn signal(&mut self, link: &mut Link) -> Option<i64> {
        if self.unhandled.contains(&proto::GET_WIFI_INFO) {
            return None;
        }
        self.ask(
            link,
            (proto::GET_WIFI_INFO, proto::STATE_WIFI_INFO),
            (Instant::now() + link::ONCE, false),
        )
        .await
        .message()
        .and_then(|m| proto::rssi(&m.payload))
    }

    /// Carries out an order; the values to publish once it is acknowledged.
    pub async fn apply(
        &mut self,
        link: &mut Link,
        key: &str,
        value: &Value,
        transition: u32,
    ) -> Result<Vec<(&'static str, Value)>, String> {
        let until = Instant::now() + link::ORDER;
        let change = change(self.caps(), key, value)?;
        let no_answer = || moli_i18n::tr!("pilotes.lifx.pas_de_reponse");
        let (parts, wanted) = match change {
            Change::Power(on) => {
                self.order(
                    link,
                    proto::LIGHT_SET_POWER,
                    &proto::set_power(on, transition),
                    until,
                )
                .await
                .ok_or_else(no_answer)?;
                if let Some((_, last)) = &mut self.last {
                    *last = on;
                }
                self.settle(transition);
                return Ok(vec![("on", Value::Bool(on))]);
            }
            Change::Color(parts, wanted) => (parts, wanted),
        };
        let (base, on) = match self.last {
            Some(last) => last,
            None => self
                .ask(link, (proto::LIGHT_GET, proto::LIGHT_STATE), (until, true))
                .await
                .message()
                .and_then(|m| proto::light_state(&m.payload))
                .map(|s| (s.color, s.on))
                .ok_or_else(no_answer)?,
        };
        let color = merge(base, parts, wanted);
        // Off: the new colour at once, then the fade on, never a flash of
        // the old one.
        let fade = if on { transition } else { 0 };
        self.write_color(link, color, parts, fade, until)
            .await
            .ok_or_else(no_answer)?;
        let mut published = Vec::new();
        if !on {
            self.order(
                link,
                proto::LIGHT_SET_POWER,
                &proto::set_power(true, transition),
                until,
            )
            .await
            .ok_or_else(no_answer)?;
            published.push(("on", Value::Bool(true)));
        }
        self.last = Some((color, true));
        self.settle(transition);
        published.push(match key {
            "brightness" => ("brightness", Value::Int(proto::percent(color.brightness))),
            "color_temp" => ("color_temp", Value::Int(i64::from(color.kelvin))),
            _ => ("color", Value::from(proto::hex(color).as_str())),
        });
        Ok(published)
    }

    fn settle(&mut self, transition: u32) {
        self.settle_until =
            Instant::now() + std::time::Duration::from_millis(u64::from(transition) + 300);
    }

    /// Only the parts changed (`LightSetWaveformOptional`), or the whole
    /// colour if the firmware does not know that message.
    async fn write_color(
        &mut self,
        link: &mut Link,
        color: Hsbk,
        parts: Parts,
        fade: u32,
        until: Instant,
    ) -> Option<()> {
        if !self.unhandled.contains(&proto::LIGHT_SET_WAVEFORM_OPTIONAL) {
            let payload = proto::set_waveform_optional(color, fade, parts);
            match self
                .send(link, proto::LIGHT_SET_WAVEFORM_OPTIONAL, &payload, until)
                .await
            {
                Answer::Got(_) => return Some(()),
                Answer::Silent => return None,
                Answer::Unhandled => {}
            }
        }
        self.order(
            link,
            proto::LIGHT_SET_COLOR,
            &proto::set_color(color, fade),
            until,
        )
        .await
    }

    async fn order(
        &mut self,
        link: &mut Link,
        kind: u16,
        payload: &[u8],
        until: Instant,
    ) -> Option<()> {
        self.send(link, kind, payload, until)
            .await
            .message()
            .map(drop)
    }

    async fn send(&mut self, link: &mut Link, kind: u16, payload: &[u8], until: Instant) -> Answer {
        let answer = link
            .ask(
                self.at,
                self.serial,
                (kind, payload),
                (Reply::Ack, proto::ACKNOWLEDGEMENT),
                (until, true),
            )
            .await;
        if matches!(answer, Answer::Unhandled) {
            self.unhandled.insert(kind);
        }
        answer
    }
}

/// What an order changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Power(bool),
    /// These parts of the colour, to these values.
    Color(Parts, Hsbk),
}

/// The change an order asks, if this bulb takes it.
pub fn change(
    (color, kelvin): (bool, Option<(u16, u16)>),
    key: &str,
    value: &Value,
) -> Result<Change, String> {
    let refuse = || moli_i18n::tr!("pilotes.lifx.ne_se_regle_pas", point = key);
    let mut wanted = Hsbk {
        hue: 0,
        saturation: 0,
        brightness: 0,
        kelvin: 0,
    };
    let mut parts = Parts::default();
    match key {
        "on" => {
            let Value::Bool(on) = *value else {
                return Err(refuse());
            };
            return Ok(Change::Power(on));
        }
        "brightness" => {
            wanted.brightness = proto::level(value.as_f64().ok_or_else(refuse)?);
            parts.brightness = true;
        }
        "color_temp" => {
            let (min, max) = kelvin.ok_or_else(refuse)?;
            let k = value.as_f64().ok_or_else(refuse)?;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let k = k.round().clamp(f64::from(min), f64::from(max)) as u16;
            wanted.kelvin = k;
            parts.kelvin = true;
            // A colour bulb leaves its colour for the white.
            parts.saturation = color;
        }
        "color" if color => {
            let text = match value {
                Value::Text(text) => Some(&**text),
                _ => None,
            };
            let (hue, saturation) = text
                .and_then(proto::hue_saturation)
                .ok_or_else(|| moli_i18n::tr!("pilotes.lifx.couleur_invalide"))?;
            wanted.hue = hue;
            wanted.saturation = saturation;
            parts.hue = true;
            parts.saturation = true;
        }
        _ => return Err(refuse()),
    }
    Ok(Change::Color(parts, wanted))
}

/// `base` with the `parts` of `wanted`.
pub fn merge(base: Hsbk, parts: Parts, wanted: Hsbk) -> Hsbk {
    Hsbk {
        hue: if parts.hue { wanted.hue } else { base.hue },
        saturation: if parts.saturation {
            wanted.saturation
        } else {
            base.saturation
        },
        brightness: if parts.brightness {
            wanted.brightness
        } else {
            base.brightness
        },
        kelvin: if parts.kelvin {
            wanted.kelvin
        } else {
            base.kelvin
        },
    }
}

fn spec(key: &str, label: &str, kind: Kind, unit: Option<Unit>, semantic: Semantic) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: Access {
            read: true,
            write: true,
        },
        unit,
        semantic,
    }
}

fn range(min: f64, max: f64) -> Kind {
    Kind::Numeric {
        min: Some(min),
        max: Some(max),
        step: Some(1.0),
    }
}
