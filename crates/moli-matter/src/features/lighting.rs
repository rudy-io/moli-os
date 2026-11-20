#![allow(clippy::many_single_char_names)]
//! Standard Matter lighting. Wire IDs follow connectedhomeip's Level/Color Control XML.
use super::{Cluster, Plan, Report, attribute, uint};
use matter_codec::Tag;
use matter_controller::{AttributePath, ReadPath, Value as Tlv};
use moli_core::{Access, Kind, PointSpec, Semantic, Unit, Value, color};

pub(crate) const CLUSTERS: &[u32] = &[8, 0x300];
pub(crate) const METADATA: &[(u32, u32)] = &[
    (8, 2),
    (8, 3),
    (8, 15),
    (0x300, 15),
    (0x300, 0x400a),
    (0x300, 0x400b),
    (0x300, 0x400c),
];
pub(crate) const DISCOVERY: &[(u32, u32)] = &[
    (8, 0),
    (8, 2),
    (8, 3),
    (8, 15),
    (0x300, 0),
    (0x300, 1),
    (0x300, 3),
    (0x300, 4),
    (0x300, 7),
    (0x300, 8),
    (0x300, 0x4000),
    (0x300, 0x4001),
    (0x300, 15),
    (0x300, 0x400a),
    (0x300, 0x400b),
    (0x300, 0x400c),
];

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Lighting {
    ep: u16,
    level: Option<Cluster>,
    color: Option<Cluster>,
    levels: Option<(u16, u16)>,
    ct: Option<(u16, u16)>,
    xy: bool,
    hs: bool,
    samples: [Option<u16>; 7], // hue, saturation, x, y, mireds, mode
}
fn bounded(v: &Tlv, max: u16) -> Option<u16> {
    u16::try_from(uint(v)?).ok().filter(|n| *n <= max)
}
impl Lighting {
    pub(crate) fn discover(ep: u16, report: &Report) -> Option<Self> {
        let level = Cluster::discover(ep, 8, report)
            .filter(|c| c.features & 2 != 0 && c.attributes.contains(&0));
        let color = Cluster::discover(ep, 0x300, report).filter(|c| c.attributes.contains(&8));
        let get = |c, a, max| bounded(attribute(report, ep, c, a)?, max);
        let levels = level.as_ref().and_then(|c| {
            let lo = if c.attributes.contains(&2) {
                get(8, 2, 254)?
            } else {
                1
            };
            let hi = if c.attributes.contains(&3) {
                get(8, 3, 254)?
            } else {
                254
            };
            (lo > 0 && lo <= hi).then_some((lo, hi))
        });
        let caps = get(0x300, 0x400a, u16::MAX).unwrap_or(0);
        let supports = |bit, attrs: &[u32]| {
            color.as_ref().is_some_and(|c| {
                c.features & bit != 0
                    && u32::from(caps) & bit != 0
                    && attrs.iter().all(|a| c.attributes.contains(a))
            })
        };
        let xy = supports(8, &[3, 4, 0x400a]);
        let hs = supports(1, &[0, 1, 0x400a])
            && color.as_ref().is_some_and(|c| {
                c.features & 2 == 0 || [0x4000, 0x4001].iter().all(|a| c.attributes.contains(a))
            });
        let ct = supports(16, &[7, 0x400a, 0x400b, 0x400c])
            .then(|| {
                let lo = get(0x300, 0x400b, 0xfeff)?;
                let hi = get(0x300, 0x400c, 0xfeff)?;
                (lo > 0 && lo <= hi).then_some((lo, hi))
            })
            .flatten();
        if level.is_none() && !xy && !hs && ct.is_none() {
            return None;
        }
        let mut this = Self {
            ep,
            level,
            color,
            levels,
            ct,
            xy,
            hs,
            samples: [None; 7],
        };
        for (p, v) in report {
            this.update(*p, v);
        }
        Some(this)
    }
    fn command(&self, cluster: u32, cmd: u32) -> bool {
        (if cluster == 8 {
            self.level.as_ref()
        } else {
            self.color.as_ref()
        })
        .is_some_and(|c| c.commands.contains(&cmd))
    }
    pub(crate) fn points(&self) -> Vec<PointSpec> {
        let mut out = Vec::new();
        let mut add = |key: &str, label: String, range: Option<(f64, f64)>, unit, write| {
            out.push(PointSpec {
                key: key.into(),
                label: label.into(),
                kind: range.map_or(Kind::Text, |(min, max)| Kind::Numeric {
                    min: Some(min),
                    max: Some(max),
                    step: Some(1.0),
                }),
                access: Access { read: true, write },
                unit,
                semantic: Semantic::infer(key, None),
            });
        };
        if self.level.is_some() {
            add(
                "brightness",
                moli_i18n::tr!("pilotes.matter.brightness"),
                Some((0.0, 100.0)),
                Some(Unit::Percent),
                self.levels.is_some() && self.command(8, 0),
            );
        }
        if let Some((lo, hi)) = self.ct {
            add(
                "color_temp",
                moli_i18n::tr!("pilotes.matter.color_temp"),
                Some((1e6 / f64::from(hi), 1e6 / f64::from(lo))),
                Some(Unit::Other("K".into())),
                self.command(0x300, 10),
            );
        }
        if self.xy || self.hs {
            add(
                "color",
                moli_i18n::tr!("pilotes.matter.color"),
                None,
                None,
                (self.xy && self.command(0x300, 7)) || (self.hs && self.command(0x300, 6)),
            );
        }
        out
    }
    pub(crate) fn paths(&self, ep: u16) -> Vec<ReadPath> {
        DISCOVERY
            .iter()
            .filter(|(c, a)| {
                (if *c == 8 {
                    self.level.as_ref()
                } else {
                    self.color.as_ref()
                })
                .is_some_and(|c| c.attributes.contains(a))
            })
            .map(|(c, a)| ReadPath::concrete(ep, *c, *a))
            .collect()
    }
    pub(crate) fn update(
        &mut self,
        path: AttributePath,
        value: &Tlv,
    ) -> Vec<(&'static str, Value)> {
        if path.endpoint != self.ep {
            return vec![];
        }
        if path.cluster == 8 && path.attribute == 0 && self.level.is_some() {
            return vec![(
                "brightness",
                bounded(value, 254)
                    .map_or(Value::Null, |n| Value::Float(f64::from(n) * 100.0 / 254.0)),
            )];
        }
        if path.cluster != 0x300 {
            return vec![];
        }
        let enhanced = self.color.as_ref().is_some_and(|c| c.features & 2 != 0);
        if path.attribute == 8 && enhanced {
            return vec![];
        }
        let (i, max) = match path.attribute {
            0 => (0, 254),
            1 => (1, 254),
            3 => (2, 0xfeff),
            4 => (3, 0xfeff),
            7 => (4, 0xfeff),
            8 => (5, 2),
            0x4001 if enhanced => (5, 3),
            0x4000 if enhanced => (6, u16::MAX),
            _ => return vec![],
        };
        let next = bounded(value, max);
        if i == 5 && self.samples[5].is_some() && self.samples[5] != next {
            self.samples[..5].fill(None);
            self.samples[6] = None;
        }
        // Never retain samples reported for a known inactive color representation.
        let active = match i {
            0 | 1 => Some(0),
            2 | 3 => Some(1),
            4 => Some(2),
            6 => Some(3),
            _ => None,
        };
        if i != 5
            && self.samples[5].is_some()
            && self.samples[5] != active
            && !(i == 1 && self.samples[5] == Some(3))
        {
            return vec![];
        }
        self.samples[i] = next;
        let [h, s, x, y, t, mode, eh] = self.samples;
        let hex = match mode {
            Some(1) if self.xy => x.zip(y).and_then(|(x, y)| {
                color::xy_to_hex(f64::from(x) / 65536.0, f64::from(y) / 65536.0)
            }),
            Some(0) if self.hs => h.zip(s).map(|(h, s)| hs_hex(h, s)),
            Some(3) if self.hs => eh
                .zip(s)
                .map(|(h, s)| hsv_hex(f64::from(h) * 6.0 / 65536.0, s)),
            _ => None,
        };
        let mut out = vec![];
        if self.xy || self.hs {
            out.push(("color", hex.map_or(Value::Null, |s| Value::Text(s.into()))));
        }
        if let Some((lo, hi)) = self.ct {
            out.push((
                "color_temp",
                t.filter(|t| mode == Some(2) && *t >= lo && *t <= hi)
                    .map_or(Value::Null, |t| Value::Float((1e6 / f64::from(t)).round())),
            ));
        }
        out
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub(crate) fn plan(&self, key: &str, value: &Value) -> Option<Plan> {
        let invoke = |cluster, command, values: Vec<u64>| {
            Some(Plan::Invoke {
                cluster,
                command,
                fields: Tlv::Structure(
                    values
                        .into_iter()
                        .enumerate()
                        .map(|(i, v)| {
                            (
                                Tag::Context(u8::try_from(i).expect("bounded fields")),
                                Tlv::Uint(v),
                            )
                        })
                        .collect(),
                ),
                timed: false,
            })
        };
        match key {
            "brightness" if self.command(8, 0) => {
                let n = value.as_f64()?;
                if !n.is_finite() || !(0.0..=100.0).contains(&n) {
                    return None;
                }
                let (lo, hi) = self.levels?;
                // UI percent rounds to nearest wire level, then respects physical limits.
                let level = (n * 254.0 / 100.0)
                    .round()
                    .clamp(f64::from(lo), f64::from(hi)) as u64;
                invoke(8, 0, vec![level, 0, 0, 0])
            }
            "color_temp" if self.command(0x300, 10) => {
                let k = value.as_f64()?;
                let (lo, hi) = self.ct?;
                if !k.is_finite() || k < 1e6 / f64::from(hi) || k > 1e6 / f64::from(lo) {
                    return None;
                }
                // Kelvin to nearest whole mired; validate before casting.
                let m = (1e6 / k).round();
                if m < f64::from(lo) || m > f64::from(hi) {
                    return None;
                }
                invoke(0x300, 10, vec![m as u64, 0, 0, 0])
            }
            "color" => {
                let Value::Text(hex) = value else {
                    return None;
                };
                let (x, y) = color::hex_to_xy(hex)?;
                if self.xy && self.command(0x300, 7) {
                    return invoke(
                        0x300,
                        7,
                        vec![
                            (x * 65536.0).round() as u64,
                            (y * 65536.0).round() as u64,
                            0,
                            0,
                            0,
                        ],
                    );
                }
                if self.hs && self.command(0x300, 6) {
                    let (h, s) = hex_hs(hex)?;
                    return invoke(0x300, 6, vec![h, s, 0, 0, 0]);
                }
                None
            }
            _ => None,
        }
    }
}
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn hs_hex(h: u16, s: u16) -> String {
    hsv_hex(f64::from(h) * 6.0 / 254.0, s)
}
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn hsv_hex(h: f64, s: u16) -> String {
    let s = f64::from(s) / 254.0;
    let x = s * (1.0 - (h % 2.0 - 1.0).abs());
    let rgb = match h as u8 {
        0 | 6 => [s, x, 0.0],
        1 => [x, s, 0.0],
        2 => [0.0, s, x],
        3 => [0.0, x, s],
        4 => [x, 0.0, s],
        _ => [s, 0.0, x],
    }
    .map(|v| ((v + 1.0 - s) * 255.0).round().clamp(0.0, 255.0) as u8);
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
#[allow(clippy::float_cmp)] // max is one of these exact operands.
fn hex_hs(hex: &str) -> Option<(u64, u64)> {
    color::hex_to_xy(hex)?;
    let r = f64::from(u8::from_str_radix(&hex[1..3], 16).ok()?) / 255.0;
    let g = f64::from(u8::from_str_radix(&hex[3..5], 16).ok()?) / 255.0;
    let b = f64::from(u8::from_str_radix(&hex[5..7], 16).ok()?) / 255.0;
    let max = r.max(g).max(b);
    let d = max - r.min(g).min(b);
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    Some((
        (h * 254.0 / 6.0).round() as u64,
        (d / max * 254.0).round() as u64,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn path(c: u32, a: u32) -> AttributePath {
        AttributePath {
            endpoint: 1,
            cluster: c,
            attribute: a,
        }
    }
    fn fixture() -> Vec<(AttributePath, Tlv)> {
        let mut r = vec![];
        for (c, f, attrs, cmds) in [
            (8, 2, vec![0, 2, 3], vec![0]),
            (
                0x300,
                25,
                vec![0, 1, 3, 4, 7, 8, 0x400a, 0x400b, 0x400c],
                vec![6, 7, 10],
            ),
        ] {
            for (a, v) in [
                (0xfffc, Tlv::Uint(f)),
                (0xfffd, Tlv::Uint(7)),
                (
                    0xfffb,
                    Tlv::Array(attrs.into_iter().map(Tlv::Uint).collect()),
                ),
                (
                    0xfff9,
                    Tlv::Array(cmds.into_iter().map(Tlv::Uint).collect()),
                ),
            ] {
                r.push((path(c, a), v));
            }
        }
        for (c, a, n) in [
            (8, 2, 1),
            (8, 3, 254),
            (0x300, 0x400a, 25),
            (0x300, 0x400b, 153),
            (0x300, 0x400c, 500),
        ] {
            r.push((path(c, a), Tlv::Uint(n)));
        }
        r
    }
    #[test]
    fn exact_commands_and_limits() {
        let l = Lighting::discover(1, &fixture()).unwrap();
        assert_eq!(l.points().len(), 3);
        let Some(Plan::Invoke {
            cluster,
            command,
            fields,
            ..
        }) = l.plan("brightness", &Value::Float(50.0))
        else {
            panic!()
        };
        assert_eq!((cluster, command), (8, 0));
        assert_eq!(
            fields,
            Tlv::Structure(vec![
                (Tag::Context(0), Tlv::Uint(127)),
                (Tag::Context(1), Tlv::Uint(0)),
                (Tag::Context(2), Tlv::Uint(0)),
                (Tag::Context(3), Tlv::Uint(0))
            ])
        );
        assert!(
            matches!(l.plan("brightness",&Value::Int(0)),Some(Plan::Invoke{fields:Tlv::Structure(f),..}) if f[0].1==Tlv::Uint(1))
        );
        assert!(
            matches!(l.plan("color_temp",&Value::Int(4000)),Some(Plan::Invoke{command:10,fields:Tlv::Structure(f),..}) if f[0].1==Tlv::Uint(250))
        );
        assert!(
            matches!(l.plan("color",&Value::Text("#ff0000".into())),Some(Plan::Invoke{command:7,fields:Tlv::Structure(f),..}) if f[0].1==Tlv::Uint(45915) && f[1].1==Tlv::Uint(19615))
        );
        for n in [f64::NAN, f64::INFINITY, -1.0, 101.0] {
            assert!(l.plan("brightness", &Value::Float(n)).is_none());
        }
        assert!(l.plan("color_temp", &Value::Int(1000)).is_none());
        assert!(l.plan("color", &Value::Text("#000000".into())).is_none());
    }
    #[test]
    fn nullable_and_mode_exclusivity() {
        let mut l = Lighting::discover(1, &fixture()).unwrap();
        for v in [Tlv::Null, Tlv::Uint(255), Tlv::Int(10), Tlv::Bool(true)] {
            assert_eq!(l.update(path(8, 0), &v), vec![("brightness", Value::Null)]);
        }
        l.update(path(0x300, 8), &Tlv::Uint(2));
        assert_eq!(
            l.update(path(0x300, 7), &Tlv::Uint(250)),
            vec![("color", Value::Null), ("color_temp", Value::Float(4000.0))]
        );
        assert_eq!(
            l.update(path(0x300, 8), &Tlv::Uint(1)),
            vec![("color", Value::Null), ("color_temp", Value::Null)]
        );
        assert!(l.update(path(0x300, 7), &Tlv::Uint(250)).is_empty());
        l.update(path(0x300, 3), &Tlv::Uint(21148));
        assert!(matches!(
            l.update(path(0x300, 4), &Tlv::Uint(21561))[0].1,
            Value::Text(_)
        ));
        assert_eq!(l.update(path(0x300, 4), &Tlv::Null)[0].1, Value::Null);
    }
    #[test]
    fn missing_features_and_read_only() {
        let mut r = fixture();
        r.retain(|(p, _)| p.cluster != 8);
        for (p, v) in &mut r {
            if p.attribute == 0xfffc {
                *v = Tlv::Uint(0);
            }
        }
        assert!(Lighting::discover(1, &r).is_none());
        let mut r = fixture();
        for (p, v) in &mut r {
            if p.attribute == 0xfff9 {
                *v = Tlv::Array(vec![]);
            }
        }
        let l = Lighting::discover(1, &r).unwrap();
        assert!(l.points().iter().all(|p| !p.access.write));
        assert!(l.plan("color", &Value::Text("#ff0000".into())).is_none());
        assert_eq!(hs_hex(0, 254), "#ff0000");
        assert_eq!(hex_hs("#00ff00"), Some((85, 254)));
    }
    #[test]
    fn enhanced_mode_uses_enhanced_hue_and_clears_old_samples() {
        let mut r = fixture();
        for (p, v) in &mut r {
            if p.cluster == 0x300 && p.attribute == 0xfffc {
                *v = Tlv::Uint(27);
            }
            if p.cluster == 0x300 && p.attribute == 0xfffb {
                let Tlv::Array(a) = v else { panic!() };
                a.extend([Tlv::Uint(0x4000), Tlv::Uint(0x4001)]);
            }
        }
        let mut l = Lighting::discover(1, &r).unwrap();
        l.update(path(0x300, 0x4001), &Tlv::Uint(3));
        l.update(path(0x300, 0x4000), &Tlv::Uint(0));
        assert_eq!(
            l.update(path(0x300, 1), &Tlv::Uint(254))[0].1,
            Value::Text("#ff0000".into())
        );
        assert!(l.update(path(0x300, 8), &Tlv::Uint(2)).is_empty());
        assert_eq!(
            l.update(path(0x300, 0x4001), &Tlv::Uint(2))[0].1,
            Value::Null
        );
        assert_eq!(
            l.update(path(0x300, 0x4001), &Tlv::Uint(3))[0].1,
            Value::Null
        );
    }
}
