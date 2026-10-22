//! Pairing, as HAP over IP defines it: *pair-setup* once (the setup code,
//! SRP, then each side's Ed25519 long-term key, signed and encrypted), and
//! *pair-verify* at every connection (X25519, signatures with those keys),
//! which yields the session's two ChaCha20-Poly1305 keys.

use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, Nonce, UnboundKey};
use ring::agreement::{self, EphemeralPrivateKey, UnparsedPublicKey, X25519};
use ring::hkdf;
use ring::rand::{SecureRandom as _, SystemRandom};
use ring::signature::{self, ED25519, Ed25519KeyPair, KeyPair as _};
use serde::{Deserialize, Serialize};

use crate::srp;
use crate::tlv8::{self, Items, Writer};

/// Who Moli is to HomeKit, and who may talk to it. Kept in the vault.
#[derive(Clone, Default, Deserialize, Serialize)]
pub struct Identity {
    /// `AA:BB:CC:DD:EE:FF`-shaped, random, Moli's « device id ».
    pub id: String,
    /// The Ed25519 key, PKCS#8 (hex).
    pub key: String,
    pub controllers: Vec<Controller>,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("id", &self.id)
            .field("controllers", &self.controllers.len())
            .finish_non_exhaustive()
    }
}

/// A paired Apple device (or person's iCloud identity, as HomeKit shares).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Controller {
    pub id: String,
    /// Its Ed25519 public key (hex).
    pub key: String,
    pub admin: bool,
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

pub fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    // The system's randomness does not fail on Linux; zeros would only
    // make a weak key, never a wrong protocol.
    let _ = SystemRandom::new().fill(&mut out);
    out
}

impl Identity {
    pub fn new() -> anyhow::Result<Self> {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
            .map_err(|_| anyhow::anyhow!("no Ed25519 key"))?;
        let id = random::<6>()
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(":");
        Ok(Self {
            id,
            key: hex(pkcs8.as_ref()),
            controllers: Vec::new(),
        })
    }

    fn keys(&self) -> Option<Ed25519KeyPair> {
        Ed25519KeyPair::from_pkcs8(&unhex(&self.key)?).ok()
    }

    pub fn paired(&self) -> bool {
        !self.controllers.is_empty()
    }

    fn controller(&self, id: &[u8]) -> Option<&Controller> {
        let id = std::str::from_utf8(id).ok()?;
        self.controllers.iter().find(|c| c.id == id)
    }

    pub fn is_admin(&self, id: &str) -> bool {
        self.controllers.iter().any(|c| c.id == id && c.admin)
    }
}

struct Len(usize);

impl hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

pub fn hkdf32(ikm: &[u8], salt: &str, info: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA512, salt.as_bytes()).extract(ikm);
    // 32 bytes out of SHA-512 always fit.
    if let Ok(okm) = prk.expand(&[info.as_bytes()], Len(32)) {
        let _ = okm.fill(&mut out);
    }
    out
}

/// A pairing message's nonce: four zeros and eight letters (`PS-Msg05`).
fn named_nonce(name: &str) -> Nonce {
    let mut nonce = [0u8; 12];
    nonce[4..].copy_from_slice(&name.as_bytes()[..8]);
    Nonce::assume_unique_for_key(nonce)
}

pub fn seal(key: &[u8; 32], nonce: Nonce, aad: &[u8], data: &mut Vec<u8>) {
    let key = LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, key).expect("32 bytes"));
    // Fails only on a message longer than ChaCha20's 256 GiB.
    let _ = key.seal_in_place_append_tag(nonce, Aad::from(aad), data);
}

pub fn open(key: &[u8; 32], nonce: Nonce, aad: &[u8], data: &mut [u8]) -> Option<usize> {
    let key = LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, key).expect("32 bytes"));
    key.open_in_place(nonce, Aad::from(aad), data)
        .ok()
        .map(|plain| plain.len())
}

pub fn counter_nonce(counter: u64) -> Nonce {
    let mut nonce = [0u8; 12];
    nonce[4..].copy_from_slice(&counter.to_le_bytes());
    Nonce::assume_unique_for_key(nonce)
}

fn error(state: u8, code: u8) -> Vec<u8> {
    Writer::new()
        .byte(tlv8::STATE, state)
        .byte(tlv8::ERROR, code)
        .finish()
}

/// What a pair-setup step changed: a new controller to keep.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Continue,
    Paired(Controller),
}

/// One pair-setup, from M1 to M6.
#[derive(Debug, Default)]
pub struct Setup {
    srp: Option<srp::Server>,
    key: Option<Vec<u8>>,
}

impl Setup {
    /// The answer to one message; `code` is the person's setup code.
    pub fn step(&mut self, identity: &Identity, code: &str, request: &[u8]) -> (Vec<u8>, Outcome) {
        let Some(items) = Items::parse(request) else {
            return (error(2, tlv8::ERR_UNAVAILABLE), Outcome::Continue);
        };
        match items.byte(tlv8::STATE) {
            Some(1) => {
                if identity.paired() {
                    return (error(2, tlv8::ERR_UNAVAILABLE), Outcome::Continue);
                }
                let salt = random::<16>();
                let server = srp::Server::new(
                    srp::Group::homekit(),
                    "Pair-Setup",
                    code,
                    &salt,
                    &random::<32>(),
                );
                let answer = Writer::new()
                    .byte(tlv8::STATE, 2)
                    .add(tlv8::SALT, &salt)
                    .add(tlv8::PUBLIC_KEY, &server.public())
                    .finish();
                self.srp = Some(server);
                (answer, Outcome::Continue)
            }
            Some(3) => {
                let verified = match (
                    &self.srp,
                    items.get(tlv8::PUBLIC_KEY),
                    items.get(tlv8::PROOF),
                ) {
                    (Some(server), Some(a), Some(proof)) => server.verify(a, proof),
                    _ => None,
                };
                let Some((proof, key)) = verified else {
                    self.srp = None;
                    return (error(4, tlv8::ERR_AUTHENTICATION), Outcome::Continue);
                };
                self.key = Some(key);
                let answer = Writer::new()
                    .byte(tlv8::STATE, 4)
                    .add(tlv8::PROOF, &proof)
                    .finish();
                (answer, Outcome::Continue)
            }
            Some(5) => match self.exchange(identity, &items) {
                Some((answer, controller)) => (answer, Outcome::Paired(controller)),
                None => (error(6, tlv8::ERR_AUTHENTICATION), Outcome::Continue),
            },
            _ => (error(2, tlv8::ERR_UNAVAILABLE), Outcome::Continue),
        }
    }

    /// M5 → M6: the controller's key, signed; ours, signed back.
    fn exchange(&mut self, identity: &Identity, items: &Items) -> Option<(Vec<u8>, Controller)> {
        let shared = self.key.take()?;
        let session = hkdf32(
            &shared,
            "Pair-Setup-Encrypt-Salt",
            "Pair-Setup-Encrypt-Info",
        );
        let mut sealed = items.get(tlv8::ENCRYPTED_DATA)?.to_vec();
        let len = open(&session, named_nonce("PS-Msg05"), &[], &mut sealed)?;
        let sub = Items::parse(&sealed[..len])?;
        let (id, key, sig) = (
            sub.get(tlv8::IDENTIFIER)?,
            sub.get(tlv8::PUBLIC_KEY)?,
            sub.get(tlv8::SIGNATURE)?,
        );
        let x = hkdf32(
            &shared,
            "Pair-Setup-Controller-Sign-Salt",
            "Pair-Setup-Controller-Sign-Info",
        );
        let info = [&x[..], id, key].concat();
        signature::UnparsedPublicKey::new(&ED25519, key)
            .verify(&info, sig)
            .ok()?;
        let ours = identity.keys()?;
        let x = hkdf32(
            &shared,
            "Pair-Setup-Accessory-Sign-Salt",
            "Pair-Setup-Accessory-Sign-Info",
        );
        let public = ours.public_key().as_ref();
        let info = [&x[..], identity.id.as_bytes(), public].concat();
        let signature = ours.sign(&info);
        let mut answer = Writer::new()
            .add(tlv8::IDENTIFIER, identity.id.as_bytes())
            .add(tlv8::PUBLIC_KEY, public)
            .add(tlv8::SIGNATURE, signature.as_ref())
            .finish();
        seal(&session, named_nonce("PS-Msg06"), &[], &mut answer);
        let controller = Controller {
            id: String::from_utf8(id.to_vec()).ok()?,
            key: hex(key),
            admin: true,
        };
        let answer = Writer::new()
            .byte(tlv8::STATE, 6)
            .add(tlv8::ENCRYPTED_DATA, &answer)
            .finish();
        Some((answer, controller))
    }
}

/// The two keys of a verified session, and who is on the other side.
#[derive(Clone)]
pub struct Keys {
    pub read: [u8; 32],
    pub write: [u8; 32],
    pub controller: String,
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keys")
            .field("controller", &self.controller)
            .finish_non_exhaustive()
    }
}

/// One pair-verify, M1 to M4.
#[derive(Default)]
pub struct Verify {
    state: Option<(Vec<u8>, Vec<u8>, Vec<u8>)>,
}

impl std::fmt::Debug for Verify {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Verify")
    }
}

impl Verify {
    /// The answer, and the session's keys once verified.
    pub fn step(&mut self, identity: &Identity, request: &[u8]) -> (Vec<u8>, Option<Keys>) {
        let Some(items) = Items::parse(request) else {
            return (error(2, tlv8::ERR_AUTHENTICATION), None);
        };
        match items.byte(tlv8::STATE) {
            Some(1) => match self.start(identity, &items) {
                Some(answer) => (answer, None),
                None => (error(2, tlv8::ERR_AUTHENTICATION), None),
            },
            Some(3) => match self.finish(identity, &items) {
                Some(keys) => (Writer::new().byte(tlv8::STATE, 4).finish(), Some(keys)),
                None => (error(4, tlv8::ERR_AUTHENTICATION), None),
            },
            _ => (error(2, tlv8::ERR_AUTHENTICATION), None),
        }
    }

    fn start(&mut self, identity: &Identity, items: &Items) -> Option<Vec<u8>> {
        let theirs = items.get(tlv8::PUBLIC_KEY)?.to_vec();
        let rng = SystemRandom::new();
        let private = EphemeralPrivateKey::generate(&X25519, &rng).ok()?;
        let ours = private.compute_public_key().ok()?.as_ref().to_vec();
        let shared = agreement::agree_ephemeral(
            private,
            &UnparsedPublicKey::new(&X25519, &theirs),
            <[u8]>::to_vec,
        )
        .ok()?;
        let keys = identity.keys()?;
        let info = [&ours[..], identity.id.as_bytes(), &theirs].concat();
        let signature = keys.sign(&info);
        let session = hkdf32(
            &shared,
            "Pair-Verify-Encrypt-Salt",
            "Pair-Verify-Encrypt-Info",
        );
        let mut sealed = Writer::new()
            .add(tlv8::IDENTIFIER, identity.id.as_bytes())
            .add(tlv8::SIGNATURE, signature.as_ref())
            .finish();
        seal(&session, named_nonce("PV-Msg02"), &[], &mut sealed);
        let answer = Writer::new()
            .byte(tlv8::STATE, 2)
            .add(tlv8::PUBLIC_KEY, &ours)
            .add(tlv8::ENCRYPTED_DATA, &sealed)
            .finish();
        self.state = Some((shared, ours, theirs));
        Some(answer)
    }

    fn finish(&mut self, identity: &Identity, items: &Items) -> Option<Keys> {
        let (shared, ours, theirs) = self.state.take()?;
        let session = hkdf32(
            &shared,
            "Pair-Verify-Encrypt-Salt",
            "Pair-Verify-Encrypt-Info",
        );
        let mut sealed = items.get(tlv8::ENCRYPTED_DATA)?.to_vec();
        let len = open(&session, named_nonce("PV-Msg03"), &[], &mut sealed)?;
        let sub = Items::parse(&sealed[..len])?;
        let id = sub.get(tlv8::IDENTIFIER)?;
        let controller = identity.controller(id)?;
        let info = [&theirs[..], id, &ours].concat();
        signature::UnparsedPublicKey::new(&ED25519, unhex(&controller.key)?)
            .verify(&info, sub.get(tlv8::SIGNATURE)?)
            .ok()?;
        Some(Keys {
            read: hkdf32(&shared, "Control-Salt", "Control-Read-Encryption-Key"),
            write: hkdf32(&shared, "Control-Salt", "Control-Write-Encryption-Key"),
            controller: controller.id.clone(),
        })
    }
}

/// `/pairings`: add, remove, list (admins only).
pub fn manage(identity: &mut Identity, from: &str, request: &[u8]) -> (Vec<u8>, bool) {
    let Some(items) = Items::parse(request) else {
        return (error(2, tlv8::ERR_UNAVAILABLE), false);
    };
    if !identity.is_admin(from) {
        return (error(2, tlv8::ERR_AUTHENTICATION), false);
    }
    let id = items
        .get(tlv8::IDENTIFIER)
        .and_then(|i| std::str::from_utf8(i).ok());
    match items.byte(tlv8::METHOD) {
        // Add (or update the permissions of) a pairing.
        Some(3) => {
            let (Some(id), Some(key)) = (id, items.get(tlv8::PUBLIC_KEY)) else {
                return (error(2, tlv8::ERR_UNAVAILABLE), false);
            };
            let admin = items.byte(tlv8::PERMISSIONS) == Some(1);
            match identity.controllers.iter_mut().find(|c| c.id == id) {
                Some(c) if c.key == hex(key) => c.admin = admin,
                Some(_) => return (error(2, tlv8::ERR_UNAVAILABLE), false),
                None => {
                    if identity.controllers.len() >= 16 {
                        return (error(2, tlv8::ERR_MAX_PEERS), false);
                    }
                    identity.controllers.push(Controller {
                        id: id.to_owned(),
                        key: hex(key),
                        admin,
                    });
                }
            }
            (Writer::new().byte(tlv8::STATE, 2).finish(), true)
        }
        Some(4) => {
            let Some(id) = id else {
                return (error(2, tlv8::ERR_UNAVAILABLE), false);
            };
            identity.controllers.retain(|c| c.id != id);
            (Writer::new().byte(tlv8::STATE, 2).finish(), true)
        }
        Some(5) => {
            let mut out = Writer::new().byte(tlv8::STATE, 2);
            for (i, c) in identity.controllers.iter().enumerate() {
                if i > 0 {
                    out = out.add(tlv8::SEPARATOR, &[]);
                }
                out = out
                    .add(tlv8::IDENTIFIER, c.id.as_bytes())
                    .add(tlv8::PUBLIC_KEY, &unhex(&c.key).unwrap_or_default())
                    .byte(tlv8::PERMISSIONS, u8::from(c.admin));
            }
            (out.finish(), false)
        }
        _ => (error(2, tlv8::ERR_UNAVAILABLE), false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A controller played from the spec, against our two exchanges.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn a_controller_pairs_then_verifies() {
        let identity = Identity::new().unwrap();
        let rng = SystemRandom::new();
        let ctrl =
            Ed25519KeyPair::from_pkcs8(Ed25519KeyPair::generate_pkcs8(&rng).unwrap().as_ref())
                .unwrap();
        let ctrl_id = b"0A1B2C3D-0000-4000-8000-000000000000";
        // Pair-setup: SRP is played by srp.rs's own tests; here, steps
        // M5/M6 with a known session key.
        let shared = vec![42u8; 64];
        let mut setup = Setup {
            srp: None,
            key: Some(shared.clone()),
        };
        let x = hkdf32(
            &shared,
            "Pair-Setup-Controller-Sign-Salt",
            "Pair-Setup-Controller-Sign-Info",
        );
        let info = [&x[..], ctrl_id, ctrl.public_key().as_ref()].concat();
        let mut sub = Writer::new()
            .add(tlv8::IDENTIFIER, ctrl_id)
            .add(tlv8::PUBLIC_KEY, ctrl.public_key().as_ref())
            .add(tlv8::SIGNATURE, ctrl.sign(&info).as_ref())
            .finish();
        let session = hkdf32(
            &shared,
            "Pair-Setup-Encrypt-Salt",
            "Pair-Setup-Encrypt-Info",
        );
        seal(&session, named_nonce("PS-Msg05"), &[], &mut sub);
        let m5 = Writer::new()
            .byte(tlv8::STATE, 5)
            .add(tlv8::ENCRYPTED_DATA, &sub)
            .finish();
        let (m6, outcome) = setup.step(&identity, "111-22-333", &m5);
        let Outcome::Paired(controller) = outcome else {
            panic!("not paired");
        };
        assert!(controller.admin);
        let m6 = Items::parse(&m6).unwrap();
        assert_eq!(m6.byte(tlv8::STATE), Some(6));
        let mut sealed = m6.get(tlv8::ENCRYPTED_DATA).unwrap().to_vec();
        let len = open(&session, named_nonce("PS-Msg06"), &[], &mut sealed).unwrap();
        let ours = Items::parse(&sealed[..len]).unwrap();
        let accessory_key = ours.get(tlv8::PUBLIC_KEY).unwrap().to_vec();
        let x = hkdf32(
            &shared,
            "Pair-Setup-Accessory-Sign-Salt",
            "Pair-Setup-Accessory-Sign-Info",
        );
        let info = [&x[..], identity.id.as_bytes(), &accessory_key].concat();
        signature::UnparsedPublicKey::new(&ED25519, &accessory_key)
            .verify(&info, ours.get(tlv8::SIGNATURE).unwrap())
            .unwrap();

        // Pair-verify.
        let mut identity = identity;
        identity.controllers.push(controller);
        let mut verify = Verify::default();
        let private = EphemeralPrivateKey::generate(&X25519, &rng).unwrap();
        let theirs = private.compute_public_key().unwrap().as_ref().to_vec();
        let m1 = Writer::new()
            .byte(tlv8::STATE, 1)
            .add(tlv8::PUBLIC_KEY, &theirs)
            .finish();
        let (m2, keys) = verify.step(&identity, &m1);
        assert!(keys.is_none());
        let m2 = Items::parse(&m2).unwrap();
        let accessory_curve = m2.get(tlv8::PUBLIC_KEY).unwrap().to_vec();
        let shared = agreement::agree_ephemeral(
            private,
            &UnparsedPublicKey::new(&X25519, &accessory_curve),
            <[u8]>::to_vec,
        )
        .unwrap();
        let session = hkdf32(
            &shared,
            "Pair-Verify-Encrypt-Salt",
            "Pair-Verify-Encrypt-Info",
        );
        let mut sealed = m2.get(tlv8::ENCRYPTED_DATA).unwrap().to_vec();
        let len = open(&session, named_nonce("PV-Msg02"), &[], &mut sealed).unwrap();
        let sub = Items::parse(&sealed[..len]).unwrap();
        let info = [&accessory_curve[..], identity.id.as_bytes(), &theirs].concat();
        signature::UnparsedPublicKey::new(&ED25519, &accessory_key)
            .verify(&info, sub.get(tlv8::SIGNATURE).unwrap())
            .unwrap();
        let info = [&theirs[..], ctrl_id, &accessory_curve].concat();
        let mut m3 = Writer::new()
            .add(tlv8::IDENTIFIER, ctrl_id)
            .add(tlv8::SIGNATURE, ctrl.sign(&info).as_ref())
            .finish();
        seal(&session, named_nonce("PV-Msg03"), &[], &mut m3);
        let m3 = Writer::new()
            .byte(tlv8::STATE, 3)
            .add(tlv8::ENCRYPTED_DATA, &m3)
            .finish();
        let (m4, keys) = verify.step(&identity, &m3);
        assert_eq!(Items::parse(&m4).unwrap().byte(tlv8::STATE), Some(4));
        let keys = keys.unwrap();
        assert_eq!(
            keys.read,
            hkdf32(&shared, "Control-Salt", "Control-Read-Encryption-Key")
        );
        assert_eq!(keys.controller, std::str::from_utf8(ctrl_id).unwrap());
    }

    #[test]
    fn a_second_pairing_is_refused_and_only_admins_manage() {
        let mut identity = Identity::new().unwrap();
        identity.controllers.push(Controller {
            id: "admin".into(),
            key: "00".into(),
            admin: true,
        });
        let m1 = Writer::new()
            .byte(tlv8::STATE, 1)
            .byte(tlv8::METHOD, 0)
            .finish();
        let (answer, _) = Setup::default().step(&identity, "111-22-333", &m1);
        assert_eq!(
            Items::parse(&answer).unwrap().byte(tlv8::ERROR),
            Some(tlv8::ERR_UNAVAILABLE)
        );
        let add = Writer::new()
            .byte(tlv8::STATE, 1)
            .byte(tlv8::METHOD, 3)
            .add(tlv8::IDENTIFIER, b"guest")
            .add(tlv8::PUBLIC_KEY, &[1u8; 32])
            .byte(tlv8::PERMISSIONS, 0)
            .finish();
        let (answer, changed) = manage(&mut identity, "guest", &add);
        assert!(!changed);
        assert_eq!(
            Items::parse(&answer).unwrap().byte(tlv8::ERROR),
            Some(tlv8::ERR_AUTHENTICATION)
        );
        let (_, changed) = manage(&mut identity, "admin", &add);
        assert!(changed);
        assert!(!identity.is_admin("guest"));
        let remove = Writer::new()
            .byte(tlv8::STATE, 1)
            .byte(tlv8::METHOD, 4)
            .add(tlv8::IDENTIFIER, b"admin")
            .finish();
        let (_, changed) = manage(&mut identity, "admin", &remove);
        assert!(changed);
        assert_eq!(identity.controllers.len(), 1);
    }
}
