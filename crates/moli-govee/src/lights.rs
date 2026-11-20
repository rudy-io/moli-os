//! The lights one instance knows, and what each datagram and each poll
//! changes: decisions only, no socket. The loop in `lib.rs` carries out the
//! [`Event`]s.
//!
//! The LAN API has no authentication and its answers carry neither a request
//! number nor, for `devStatus`, the light's identifier: an answer is tied to
//! a light by the address it comes from, and nothing else can be done. So a
//! light is known by its identifier, holds one address, and an address is
//! held by one light: a light that still answers is never moved nor pushed
//! out by a datagram claiming its identifier or its address.

use std::net::Ipv4Addr;
use std::time::Duration;

use tokio::time::Instant;

use crate::wire::{Found, Status};

/// One light.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Light {
    /// Its identifier (hexadecimal digits): the device's identity.
    pub native: String,
    pub sku: String,
    /// Where it answers; `None` once another light took its address.
    pub at: Option<Ipv4Addr>,
    pub online: bool,
    /// `devStatus` sent since its last answer.
    pub unanswered: u32,
    /// When it last answered a `devStatus`.
    pub answered: Option<Instant>,
}

impl Light {
    /// Whether it answers where it is: online, and at most one question
    /// still open (the poll under way).
    fn answers(&self) -> bool {
        self.online && self.unanswered < 2
    }
}

/// What the loop has to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Publish this light (new, or its model changed).
    Publish(String),
    /// Send a `devStatus` there.
    Ask(Ipv4Addr),
    Online(String, bool),
    /// A reading of this light.
    State(String, Status),
    /// This light answers at a new address.
    Moved(String, Ipv4Addr),
}

#[derive(Debug)]
pub struct Lights {
    all: Vec<Light>,
    /// Addresses given in the configuration.
    hosts: Vec<Ipv4Addr>,
    /// Lights may be met anywhere (discovery), not only at `hosts`.
    anywhere: bool,
    max: usize,
    /// A light was left out: the instance is full.
    pub full: bool,
    /// Polls without an answer before a light is shown offline.
    misses: u32,
    /// How old a light's last answer may be for an order to leave.
    fresh: Duration,
}

impl Lights {
    pub fn new(
        hosts: Vec<Ipv4Addr>,
        anywhere: bool,
        max: usize,
        misses: u32,
        fresh: Duration,
    ) -> Self {
        Self {
            all: Vec::new(),
            hosts,
            anywhere,
            max,
            full: false,
            misses,
            fresh,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.all.is_empty()
    }

    pub fn get(&self, native: &str) -> Option<&Light> {
        self.all.iter().find(|l| l.native == native)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Light> {
        self.all.iter()
    }

    fn holder(&mut self, at: Ipv4Addr) -> Option<&mut Light> {
        self.all.iter_mut().find(|l| l.at == Some(at))
    }

    /// A light answered a `scan` from `from`. The address is the one the
    /// datagram came from, never the one it claims.
    pub fn heard(&mut self, from: Ipv4Addr, found: &Found) -> Vec<Event> {
        if !self.anywhere && !self.hosts.contains(&from) {
            return Vec::new();
        }
        let known = self.all.iter().position(|l| l.native == found.device);
        if let Some(i) = known
            && self.all[i].at == Some(from)
        {
            let light = &mut self.all[i];
            if !found.sku.is_empty() && light.sku != found.sku {
                light.sku.clone_from(&found.sku);
                return vec![Event::Publish(light.native.clone())];
            }
            return Vec::new();
        }
        // A new address for a known light, or a new light: only once what
        // is there now (the light elsewhere, the address's holder) is silent.
        if known.is_some_and(|i| self.all[i].answers())
            || self.holder(from).is_some_and(|l| l.answers())
        {
            return Vec::new();
        }
        if known.is_none() && self.all.len() >= self.max {
            self.full = true;
            return Vec::new();
        }
        if let Some(holder) = self.holder(from) {
            holder.at = None;
        }
        let mut events = Vec::new();
        if let Some(i) = known {
            let light = &mut self.all[i];
            light.at = Some(from);
            light.unanswered = 0;
            if !found.sku.is_empty() && light.sku != found.sku {
                light.sku.clone_from(&found.sku);
                events.push(Event::Publish(light.native.clone()));
            }
            events.push(Event::Moved(light.native.clone(), from));
        } else {
            self.all.push(Light {
                native: found.device.clone(),
                sku: found.sku.clone(),
                at: Some(from),
                // Not before it has given its state.
                online: false,
                unanswered: 0,
                answered: None,
            });
            events.push(Event::Publish(found.device.clone()));
        }
        // Its state at once, without waiting for the next poll.
        if let Some(light) = self.holder(from) {
            light.unanswered += 1;
            events.push(Event::Ask(from));
        }
        events
    }

    /// A `devStatus` answer came from `from`. Which light it was, if one is
    /// there.
    pub fn status(&mut self, from: Ipv4Addr, status: Status, now: Instant) -> Vec<Event> {
        let Some(light) = self.holder(from) else {
            return Vec::new();
        };
        light.unanswered = 0;
        light.answered = Some(now);
        let mut events = vec![Event::State(light.native.clone(), status)];
        if !light.online {
            light.online = true;
            events.push(Event::Online(light.native.clone(), true));
        }
        events
    }

    /// A poll: a light silent for `misses` polls goes offline (its last
    /// values stay), and every light with an address is asked again, the
    /// offline ones too (one datagram, nothing waits for the answer).
    pub fn poll(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        for light in &mut self.all {
            if light.online && light.unanswered >= self.misses {
                light.online = false;
                events.push(Event::Online(light.native.clone(), false));
            }
            if let Some(at) = light.at {
                light.unanswered = light.unanswered.saturating_add(1);
                events.push(Event::Ask(at));
            }
        }
        events
    }

    /// Where an order for this light may be sent now: only to a light
    /// online that answered recently. An order is never kept for later.
    pub fn target(&self, native: &str, now: Instant) -> Option<Ipv4Addr> {
        let light = self.get(native)?;
        let recent = light
            .answered
            .is_some_and(|at| now.saturating_duration_since(at) <= self.fresh);
        (light.online && recent).then_some(light.at).flatten()
    }

    /// Whether to search again soon: no light, one offline or without an
    /// address, or an address of `hosts` nobody answers from.
    pub fn missing(&self) -> bool {
        self.all.is_empty()
            || self.all.iter().any(|l| !l.online || l.at.is_none())
            || !self
                .hosts
                .iter()
                .all(|h| self.all.iter().any(|l| l.at == Some(*h)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Ipv4Addr = Ipv4Addr::new(192, 168, 77, 10);
    const B: Ipv4Addr = Ipv4Addr::new(192, 168, 77, 11);
    const C: Ipv4Addr = Ipv4Addr::new(192, 168, 77, 12);
    const ONE: &str = "aabbccddeeff0001";
    const TWO: &str = "aabbccddeeff0002";
    const FRESH: Duration = Duration::from_secs(16);

    fn found(device: &str) -> Found {
        Found {
            device: device.into(),
            sku: "H6163".into(),
        }
    }

    fn lights() -> Lights {
        Lights::new(Vec::new(), true, 100, 3, FRESH)
    }

    fn on() -> Status {
        Status {
            on: Some(true),
            ..Status::default()
        }
    }

    /// A light met at `at` that has answered once.
    fn met(lights: &mut Lights, device: &str, at: Ipv4Addr, now: Instant) {
        lights.heard(at, &found(device));
        lights.status(at, on(), now);
    }

    #[test]
    fn a_light_is_published_then_online_once_it_gives_its_state() {
        let mut lights = lights();
        let now = Instant::now();
        assert_eq!(
            lights.heard(A, &found(ONE)),
            [Event::Publish(ONE.into()), Event::Ask(A)]
        );
        assert!(!lights.get(ONE).unwrap().online);
        assert_eq!(lights.target(ONE, now), None);
        assert_eq!(
            lights.status(A, on(), now),
            [
                Event::State(ONE.into(), on()),
                Event::Online(ONE.into(), true)
            ]
        );
        // Heard again at the same address: nothing new.
        assert_eq!(lights.heard(A, &found(ONE)), []);
        assert_eq!(
            lights.status(A, on(), now),
            [Event::State(ONE.into(), on())]
        );
        assert_eq!(lights.target(ONE, now), Some(A));
        assert!(!lights.missing());
    }

    #[test]
    fn a_state_from_an_unknown_address_is_ignored() {
        let mut lights = lights();
        met(&mut lights, ONE, A, Instant::now());
        assert_eq!(lights.status(B, on(), Instant::now()), []);
    }

    #[test]
    fn three_missed_polls_put_a_light_offline_and_it_comes_back() {
        let mut lights = lights();
        let now = Instant::now();
        met(&mut lights, ONE, A, now);
        for _ in 0..3 {
            assert_eq!(lights.poll(), [Event::Ask(A)]);
        }
        assert_eq!(
            lights.poll(),
            [Event::Online(ONE.into(), false), Event::Ask(A)]
        );
        assert!(lights.missing());
        // Still asked, and said offline once only.
        assert_eq!(lights.poll(), [Event::Ask(A)]);
        assert_eq!(
            lights.status(A, on(), now),
            [
                Event::State(ONE.into(), on()),
                Event::Online(ONE.into(), true)
            ]
        );
        // An answer between two polls keeps it online for ever.
        for _ in 0..10 {
            lights.poll();
            assert_eq!(
                lights.status(A, on(), now),
                [Event::State(ONE.into(), on())]
            );
        }
    }

    #[test]
    fn an_order_leaves_only_for_a_light_that_answered_recently() {
        let mut lights = lights();
        let now = Instant::now();
        met(&mut lights, ONE, A, now);
        assert_eq!(lights.target(ONE, now + FRESH), Some(A));
        assert_eq!(
            lights.target(ONE, now + FRESH + Duration::from_millis(1)),
            None
        );
        assert_eq!(lights.target(TWO, now), None);
        // Offline: refused, however recent the last answer.
        for _ in 0..4 {
            lights.poll();
        }
        assert_eq!(lights.target(ONE, now), None);
    }

    #[test]
    fn a_light_that_answers_is_not_moved() {
        let mut lights = lights();
        let now = Instant::now();
        met(&mut lights, ONE, A, now);
        // The poll under way does not make it silent.
        lights.poll();
        assert_eq!(lights.heard(B, &found(ONE)), []);
        assert_eq!(lights.get(ONE).unwrap().at, Some(A));
    }

    #[test]
    fn a_silent_light_is_followed_to_its_new_address() {
        let mut lights = lights();
        let now = Instant::now();
        met(&mut lights, ONE, A, now);
        lights.poll();
        lights.poll();
        assert_eq!(
            lights.heard(B, &found(ONE)),
            [Event::Moved(ONE.into(), B), Event::Ask(B)]
        );
        assert_eq!(lights.status(A, on(), now), []);
        assert_eq!(
            lights.status(B, on(), now),
            [Event::State(ONE.into(), on())]
        );
    }

    #[test]
    fn an_address_is_taken_only_from_a_silent_light() {
        let mut lights = lights();
        let now = Instant::now();
        met(&mut lights, ONE, A, now);
        // ONE answers at A: another identifier there is not believed.
        assert_eq!(lights.heard(A, &found(TWO)), []);
        assert!(lights.get(TWO).is_none());
        for _ in 0..4 {
            lights.poll();
        }
        // ONE is offline: TWO takes the address, ONE has none any more.
        assert_eq!(
            lights.heard(A, &found(TWO)),
            [Event::Publish(TWO.into()), Event::Ask(A)]
        );
        assert_eq!(lights.get(ONE).unwrap().at, None);
        assert_eq!(
            lights.status(A, on(), now),
            [
                Event::State(TWO.into(), on()),
                Event::Online(TWO.into(), true)
            ]
        );
        // ONE is no longer asked, and is missing until it is heard again.
        assert_eq!(lights.poll(), [Event::Ask(A)]);
        assert!(lights.missing());
        assert_eq!(
            lights.heard(C, &found(ONE)),
            [Event::Moved(ONE.into(), C), Event::Ask(C)]
        );
    }

    #[test]
    fn without_discovery_only_the_given_addresses_count() {
        let mut lights = Lights::new(vec![A, B], false, 100, 3, FRESH);
        assert!(lights.missing());
        assert_eq!(lights.heard(C, &found(ONE)), []);
        assert!(lights.is_empty());
        met(&mut lights, ONE, A, Instant::now());
        // B has not answered yet.
        assert!(lights.missing());
        met(&mut lights, TWO, B, Instant::now());
        assert!(!lights.missing());
    }

    #[test]
    fn the_instance_has_a_ceiling() {
        let mut lights = Lights::new(Vec::new(), true, 1, 3, FRESH);
        met(&mut lights, ONE, A, Instant::now());
        assert_eq!(lights.heard(B, &found(TWO)), []);
        assert!(lights.full);
        assert_eq!(lights.iter().count(), 1);
    }

    #[test]
    fn a_model_learnt_later_is_published() {
        let mut lights = lights();
        lights.heard(
            A,
            &Found {
                device: ONE.into(),
                sku: String::new(),
            },
        );
        assert_eq!(lights.heard(A, &found(ONE)), [Event::Publish(ONE.into())]);
        assert_eq!(lights.get(ONE).unwrap().sku, "H6163");
    }
}
