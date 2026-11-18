//! HomeKit pairing (HAP) as the Apple TV's Companion protocol runs it, the
//! way pyatv does (MIT):
//!
//! - **pair setup**, once: the Apple TV shows a four-digit code; SRP proves
//!   both sides know it, then each gives the other its long-term Ed25519
//!   key, encrypted under a key SRP derived. Moli keeps the result
//!   ([`Credentials`]) in its vault.
//! - **pair verify**, at each connection: an X25519 exchange signed by both
//!   long-term keys, from which the session's two ChaCha20-Poly1305 keys
//!   come.
//!
//! Each step takes the device's TLV8 and gives the next one to send; the
//! transport is the caller's.

use anyhow::{Context as _, bail, ensure};
use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, Nonce, UnboundKey};
use ring::agreement::{self, EphemeralPrivateKey, UnparsedPublicKey, X25519};
use ring::hkdf;
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair as _};
use serde::{Deserialize, Serialize};

use crate::opack;
use crate::srp;
use crate::tlv8::{self, Tlv};

/// What a pairing leaves, enough to verify at every connection.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Credentials {
    /// Seed of Moli's long-term Ed25519 key (hex).
    seed: String,
    /// The name Moli paired under (a UUID, as text).
    client_id: String,
    /// The Apple TV's pairing identifier (hex of its bytes).
    device_id: String,
    /// The Apple TV's long-term Ed25519 key (hex).
    device_key: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the seed, even in a log.
        f.debug_struct("Credentials")
            .field("client_id", &self.client_id)
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn unhex(s: &str) -> anyhow::Result<Vec<u8>> {
    // Cut by bytes below: anything but ASCII is refused first.
    ensure!(s.is_ascii(), "not hex");
    ensure!(s.len().is_multiple_of(2), "odd hex");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).context("not hex"))
        .collect()
}

impl Credentials {
    /// As kept in the vault.
    #[must_use]
    pub fn to_secret(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_secret(s: &str) -> anyhow::Result<Self> {
        let c: Self = serde_json::from_str(s).context("Apple TV credentials unreadable")?;
        ensure!(unhex(&c.seed)?.len() == 32, "Apple TV credentials: bad key");
        ensure!(
            unhex(&c.device_key)?.len() == 32,
            "Apple TV credentials: bad device key"
        );
        unhex(&c.device_id)?;
        Ok(c)
    }

    /// A stable name for the paired Apple TV (its pairing identifier).
    #[must_use]
    pub fn device(&self) -> String {
        let bytes = unhex(&self.device_id).unwrap_or_default();
        match String::from_utf8(bytes) {
            Ok(text) if !text.is_empty() && text.chars().all(|c| c.is_ascii_graphic()) => text,
            _ => self.device_id.clone(),
        }
    }

    /// The name Moli paired under.
    #[must_use]
    pub fn client(&self) -> &str {
        &self.client_id
    }

    fn signing_key(&self) -> anyhow::Result<Ed25519KeyPair> {
        Ed25519KeyPair::from_seed_unchecked(&unhex(&self.seed)?)
            .map_err(|_| anyhow::anyhow!("Apple TV credentials: bad key"))
    }
}

/// HKDF-SHA512, 32 bytes.
fn derive(salt: &str, info: &str, secret: &[u8]) -> [u8; 32] {
    struct Len;
    impl hkdf::KeyType for Len {
        fn len(&self) -> usize {
            32
        }
    }
    let mut out = [0u8; 32];
    let info = [info.as_bytes()];
    let filled = hkdf::Salt::new(hkdf::HKDF_SHA512, salt.as_bytes())
        .extract(secret)
        .expand(&info, Len)
        .and_then(|okm| okm.fill(&mut out));
    debug_assert!(filled.is_ok(), "32 bytes are always within HKDF's reach");
    out
}

#[cfg(test)]
pub(crate) fn tests_derive(salt: &str, info: &str, secret: &[u8]) -> [u8; 32] {
    derive(salt, info, secret)
}

/// A nonce of HAP's pairing messages: four zero bytes, then the eight
/// letters (`PS-Msg05`).
fn label_nonce(label: [u8; 8]) -> Nonce {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&label);
    Nonce::assume_unique_for_key(n)
}

fn key(bytes: &[u8; 32]) -> LessSafeKey {
    LessSafeKey::new(
        UnboundKey::new(&CHACHA20_POLY1305, bytes)
            .unwrap_or_else(|_| unreachable!("a 32-byte key is what ChaCha20-Poly1305 takes")),
    )
}

pub(crate) fn seal(k: &[u8; 32], nonce: Nonce, aad: &[u8], data: &[u8]) -> Vec<u8> {
    let mut out = data.to_vec();
    key(k)
        .seal_in_place_append_tag(nonce, Aad::from(aad), &mut out)
        .unwrap_or_else(|_| unreachable!("sealing in memory does not fail"));
    out
}

pub(crate) fn open(k: &[u8; 32], nonce: Nonce, aad: &[u8], data: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut buf = data.to_vec();
    let plain = key(k)
        .open_in_place(nonce, Aad::from(aad), &mut buf)
        .map_err(|_| anyhow::anyhow!("decryption failed"))?;
    Ok(plain.to_vec())
}

/// The device's answer, or the error it sent instead.
pub(crate) fn answer(tlv: &Tlv, state: u8) -> anyhow::Result<()> {
    if let Some(code) = tlv.get(tlv8::ERROR) {
        if let Some(wait) = tlv.get(tlv8::BACK_OFF) {
            let seconds = wait
                .iter()
                .rev()
                .fold(0u64, |acc, b| acc << 8 | u64::from(*b));
            bail!(
                "the Apple TV refused: {} (try again in {seconds} s)",
                tlv8::error_name(code)
            );
        }
        bail!("the Apple TV refused: {}", tlv8::error_name(code));
    }
    if let Some(got) = tlv.get(tlv8::STATE) {
        ensure!(
            got == [state],
            "the Apple TV answered step {got:?}, not {state}"
        );
    }
    Ok(())
}

fn random<const N: usize>(rng: &SystemRandom) -> [u8; N] {
    let mut out = [0u8; N];
    rng.fill(&mut out)
        .unwrap_or_else(|_| unreachable!("the system's random source does not fail"));
    out
}

fn uuid(rng: &SystemRandom) -> String {
    let b: [u8; 16] = random(rng);
    let h = hex(&b);
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

/// Pair setup, from the first message to the credentials.
pub struct Setup {
    seed: [u8; 32],
    client_id: String,
    srp: Option<srp::Client>,
    session_key: Option<[u8; 32]>,
    /// What the Apple TV signs in M6 starts with this (from SRP's key).
    device_x: Option<[u8; 32]>,
}

impl std::fmt::Debug for Setup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never a key, even in a log.
        f.debug_struct("Setup")
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

impl Setup {
    #[must_use]
    pub fn new() -> Self {
        let rng = SystemRandom::new();
        Self {
            seed: random(&rng),
            client_id: uuid(&rng).to_uppercase(),
            srp: None,
            session_key: None,
            device_x: None,
        }
    }

    /// M1: « pair me » (the Apple TV then shows its code).
    #[must_use]
    pub fn start() -> Vec<u8> {
        tlv8::write(&[(tlv8::METHOD, &[0x00]), (tlv8::STATE, &[0x01])])
    }

    /// M2 → M3: the code, proven by SRP.
    pub fn code(&mut self, m2: &Tlv, pin: &str) -> anyhow::Result<Vec<u8>> {
        answer(m2, 2)?;
        let salt = m2.get(tlv8::SALT).context("no salt from the Apple TV")?;
        let public = m2
            .get(tlv8::PUBLIC_KEY)
            .context("no key from the Apple TV")?;
        // SRP's secret `a`: drawn for this pairing, not the long-term key.
        let private: [u8; 32] = random(&SystemRandom::new());
        let client = srp::Client::new(&private, pin, salt, public)?;
        let m3 = tlv8::write(&[
            (tlv8::STATE, &[0x03]),
            (tlv8::PUBLIC_KEY, &client.public),
            (tlv8::PROOF, &client.proof),
        ]);
        self.srp = Some(client);
        Ok(m3)
    }

    /// M4 → M5: the device knew the code; Moli's long-term key, signed,
    /// encrypted, with the name the Apple TV lists it under.
    pub fn exchange(&mut self, m4: &Tlv, name: &str) -> anyhow::Result<Vec<u8>> {
        answer(m4, 4)?;
        let srp = self.srp.as_ref().context("pairing out of order")?;
        srp.check(m4.get(tlv8::PROOF).context("no proof from the Apple TV")?)?;
        let sign_x = derive(
            "Pair-Setup-Controller-Sign-Salt",
            "Pair-Setup-Controller-Sign-Info",
            &srp.key,
        );
        let session = derive(
            "Pair-Setup-Encrypt-Salt",
            "Pair-Setup-Encrypt-Info",
            &srp.key,
        );
        let pair =
            Ed25519KeyPair::from_seed_unchecked(&self.seed).map_err(|_| anyhow::anyhow!("key"))?;
        let public = pair.public_key().as_ref().to_vec();
        let signed = [sign_x.as_slice(), self.client_id.as_bytes(), &public].concat();
        let signature = pair.sign(&signed);
        let named = opack::pack(&opack::Value::dict([("name", opack::Value::from(name))]));
        let inner = tlv8::write(&[
            (tlv8::IDENTIFIER, self.client_id.as_bytes()),
            (tlv8::PUBLIC_KEY, &public),
            (tlv8::SIGNATURE, signature.as_ref()),
            (tlv8::NAME, &named),
        ]);
        let sealed = seal(&session, label_nonce(*b"PS-Msg05"), &[], &inner);
        self.session_key = Some(session);
        self.device_x = Some(derive(
            "Pair-Setup-Accessory-Sign-Salt",
            "Pair-Setup-Accessory-Sign-Info",
            &srp.key,
        ));
        Ok(tlv8::write(&[
            (tlv8::STATE, &[0x05]),
            (tlv8::ENCRYPTED_DATA, &sealed),
        ]))
    }

    /// M6: the Apple TV's identity and long-term key, signed by that key over
    /// what SRP derived (as HAP has it), and encrypted under the session key
    /// only a device that knew the code holds.
    pub fn finish(self, m6: &Tlv) -> anyhow::Result<Credentials> {
        answer(m6, 6)?;
        let session = self.session_key.context("pairing out of order")?;
        let sealed = m6
            .get(tlv8::ENCRYPTED_DATA)
            .context("no encrypted data from the Apple TV")?;
        let inner = tlv8::read(&open(&session, label_nonce(*b"PS-Msg06"), &[], sealed)?)?;
        let device_id = inner.get(tlv8::IDENTIFIER).context("no identifier")?;
        let device_key = inner.get(tlv8::PUBLIC_KEY).context("no key")?;
        ensure!(device_key.len() == 32, "the Apple TV's key is not Ed25519");
        let device_x = self.device_x.context("pairing out of order")?;
        let signature = inner.get(tlv8::SIGNATURE).context("no signature")?;
        ring::signature::UnparsedPublicKey::new(&ED25519, device_key)
            .verify(
                &[device_x.as_slice(), device_id, device_key].concat(),
                signature,
            )
            .map_err(|_| anyhow::anyhow!("the Apple TV's signature does not match its key"))?;
        Ok(Credentials {
            seed: hex(&self.seed),
            client_id: self.client_id,
            device_id: hex(device_id),
            device_key: hex(device_key),
        })
    }
}

impl Default for Setup {
    fn default() -> Self {
        Self::new()
    }
}

/// Pair verify, once per connection.
pub struct Verify {
    private: Option<EphemeralPrivateKey>,
    public: Vec<u8>,
    shared: Option<Vec<u8>>,
}

impl std::fmt::Debug for Verify {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the shared secret, even in a log.
        f.debug_struct("Verify").finish_non_exhaustive()
    }
}

impl Verify {
    /// With M1 to send: Moli's key for this session.
    pub fn start() -> anyhow::Result<(Self, Vec<u8>)> {
        let rng = SystemRandom::new();
        let private = EphemeralPrivateKey::generate(&X25519, &rng)
            .map_err(|_| anyhow::anyhow!("X25519 key"))?;
        let public = private
            .compute_public_key()
            .map_err(|_| anyhow::anyhow!("X25519 key"))?
            .as_ref()
            .to_vec();
        let m1 = tlv8::write(&[(tlv8::STATE, &[0x01]), (tlv8::PUBLIC_KEY, &public)]);
        Ok((
            Self {
                private: Some(private),
                public,
                shared: None,
            },
            m1,
        ))
    }

    /// M2 → M3: the Apple TV is the one paired (its signature), and Moli
    /// says it is too.
    pub fn prove(&mut self, credentials: &Credentials, m2: &Tlv) -> anyhow::Result<Vec<u8>> {
        answer(m2, 2)?;
        let theirs = m2
            .get(tlv8::PUBLIC_KEY)
            .context("no key from the Apple TV")?
            .to_vec();
        let sealed = m2
            .get(tlv8::ENCRYPTED_DATA)
            .context("no encrypted data from the Apple TV")?;
        let private = self.private.take().context("verify out of order")?;
        let shared =
            agreement::agree_ephemeral(private, &UnparsedPublicKey::new(&X25519, &theirs), |s| {
                s.to_vec()
            })
            .map_err(|_| anyhow::anyhow!("the Apple TV's session key is not valid"))?;
        let session = derive(
            "Pair-Verify-Encrypt-Salt",
            "Pair-Verify-Encrypt-Info",
            &shared,
        );
        let inner = tlv8::read(&open(&session, label_nonce(*b"PV-Msg02"), &[], sealed)?)?;
        let id = inner.get(tlv8::IDENTIFIER).context("no identifier")?;
        let signature = inner.get(tlv8::SIGNATURE).context("no signature")?;
        ensure!(
            hex(id) == credentials.device_id,
            "another Apple TV answers at this address"
        );
        let device_key = unhex(&credentials.device_key)?;
        ring::signature::UnparsedPublicKey::new(&ED25519, &device_key)
            .verify(&[theirs.as_slice(), id, &self.public].concat(), signature)
            .map_err(|_| anyhow::anyhow!("the Apple TV's signature does not match its pairing"))?;
        let ours = credentials.signing_key()?;
        let signed = [
            self.public.as_slice(),
            credentials.client_id.as_bytes(),
            &theirs,
        ]
        .concat();
        let reply = tlv8::write(&[
            (tlv8::IDENTIFIER, credentials.client_id.as_bytes()),
            (tlv8::SIGNATURE, ours.sign(&signed).as_ref()),
        ]);
        let sealed = seal(&session, label_nonce(*b"PV-Msg03"), &[], &reply);
        self.shared = Some(shared);
        Ok(tlv8::write(&[
            (tlv8::STATE, &[0x03]),
            (tlv8::ENCRYPTED_DATA, &sealed),
        ]))
    }

    /// M4, then the session's keys: (Moli → Apple TV, Apple TV → Moli).
    pub fn keys(self, m4: &Tlv) -> anyhow::Result<([u8; 32], [u8; 32])> {
        answer(m4, 4)?;
        let shared = self.shared.context("verify out of order")?;
        Ok((
            derive("", "ClientEncrypt-main", &shared),
            derive("", "ServerEncrypt-main", &shared),
        ))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The Apple TV's half of pair verify, to test Moli's against.
    pub(crate) struct Device {
        pub id: Vec<u8>,
        pub pair: Ed25519KeyPair,
    }

    impl Device {
        pub(crate) fn new() -> Self {
            Self {
                id: b"ATV-TEST-0001".to_vec(),
                pair: Ed25519KeyPair::from_seed_unchecked(&[9u8; 32]).unwrap(),
            }
        }

        pub(crate) fn credentials(&self) -> Credentials {
            Credentials {
                seed: hex(&[3u8; 32]),
                client_id: "A1B2C3D4-0000-0000-0000-000000000001".into(),
                device_id: hex(&self.id),
                device_key: hex(self.pair.public_key().as_ref()),
            }
        }

        /// M1 → M2, and what the device keeps: (session key, shared, its key, Moli's key).
        pub(crate) fn answer(&self, m1: &[u8]) -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
            let m1 = tlv8::read(m1).unwrap();
            let theirs = m1.get(tlv8::PUBLIC_KEY).unwrap().to_vec();
            let rng = SystemRandom::new();
            let private = EphemeralPrivateKey::generate(&X25519, &rng).unwrap();
            let public = private.compute_public_key().unwrap().as_ref().to_vec();
            let shared = agreement::agree_ephemeral(
                private,
                &UnparsedPublicKey::new(&X25519, &theirs),
                <[u8]>::to_vec,
            )
            .unwrap();
            let session = derive(
                "Pair-Verify-Encrypt-Salt",
                "Pair-Verify-Encrypt-Info",
                &shared,
            );
            let signature = self
                .pair
                .sign(&[public.as_slice(), &self.id, &theirs].concat());
            let inner = tlv8::write(&[
                (tlv8::IDENTIFIER, &self.id),
                (tlv8::SIGNATURE, signature.as_ref()),
            ]);
            let sealed = seal(&session, label_nonce(*b"PV-Msg02"), &[], &inner);
            let m2 = tlv8::write(&[
                (tlv8::STATE, &[2]),
                (tlv8::PUBLIC_KEY, &public),
                (tlv8::ENCRYPTED_DATA, &sealed),
            ]);
            (m2, shared, public, theirs)
        }
    }

    #[test]
    fn credentials_round_trip_through_the_vault_and_never_show_the_seed() {
        let c = Device::new().credentials();
        let back = Credentials::from_secret(&c.to_secret()).unwrap();
        assert_eq!(back, c);
        assert_eq!(back.device(), "ATV-TEST-0001");
        assert!(!format!("{c:?}").contains(&c.seed));
        assert!(Credentials::from_secret("{}").is_err());
        assert!(Credentials::from_secret(&c.to_secret().replace(&c.seed, "00")).is_err());
    }

    #[test]
    fn pair_verify_agrees_on_the_session_keys() {
        let device = Device::new();
        let credentials = device.credentials();
        let (mut verify, m1) = Verify::start().unwrap();
        let (m2, shared, device_public, moli_public) = device.answer(&m1);
        let m3 = verify
            .prove(&credentials, &tlv8::read(&m2).unwrap())
            .unwrap();
        // The device checks Moli's signature with the key Moli paired with.
        let session = derive(
            "Pair-Verify-Encrypt-Salt",
            "Pair-Verify-Encrypt-Info",
            &shared,
        );
        let m3 = tlv8::read(&m3).unwrap();
        let inner = tlv8::read(
            &open(
                &session,
                label_nonce(*b"PV-Msg03"),
                &[],
                m3.get(tlv8::ENCRYPTED_DATA).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        let moli_key = credentials.signing_key().unwrap();
        ring::signature::UnparsedPublicKey::new(&ED25519, moli_key.public_key().as_ref())
            .verify(
                &[
                    moli_public.as_slice(),
                    credentials.client_id.as_bytes(),
                    &device_public,
                ]
                .concat(),
                inner.get(tlv8::SIGNATURE).unwrap(),
            )
            .unwrap();
        let (out, inn) = verify
            .keys(&tlv8::read(&tlv8::write(&[(tlv8::STATE, &[4])])).unwrap())
            .unwrap();
        assert_eq!(out, derive("", "ClientEncrypt-main", &shared));
        assert_eq!(inn, derive("", "ServerEncrypt-main", &shared));
        assert_ne!(out, inn);
    }

    #[test]
    fn pair_verify_refuses_another_device() {
        let device = Device::new();
        let mut credentials = device.credentials();
        // Paired with a key the answering device does not hold.
        let other = Ed25519KeyPair::from_seed_unchecked(&[8u8; 32]).unwrap();
        credentials.device_key = hex(other.public_key().as_ref());
        let (mut verify, m1) = Verify::start().unwrap();
        let (m2, ..) = device.answer(&m1);
        let err = verify
            .prove(&credentials, &tlv8::read(&m2).unwrap())
            .unwrap_err();
        assert!(err.to_string().contains("signature"), "{err}");
    }

    #[test]
    fn a_refusal_says_why() {
        let refused =
            tlv8::read(&tlv8::write(&[(tlv8::STATE, &[2]), (tlv8::ERROR, &[2])])).unwrap();
        let err = Setup::new().code(&refused, "1234").unwrap_err();
        assert!(err.to_string().contains("wrong code"), "{err}");
        let wait = tlv8::read(&tlv8::write(&[
            (tlv8::ERROR, &[3]),
            (tlv8::BACK_OFF, &[0x2c, 0x01]),
        ]))
        .unwrap();
        let err = Setup::new().code(&wait, "1234").unwrap_err();
        assert!(err.to_string().contains("300 s"), "{err}");
    }

    #[test]
    fn pair_setup_sends_what_pyatv_sends_and_seals_its_key() {
        assert_eq!(Setup::start(), [0x00, 1, 0x00, 0x06, 1, 0x01]);
        // The device side of SRP is in srp.rs's vectors; here, M5's sealing
        // under the key SRP gave, and M6 read back the same way.
        let k = vec![5u8; 64];
        let session = derive("Pair-Setup-Encrypt-Salt", "Pair-Setup-Encrypt-Info", &k);
        let device_x = derive(
            "Pair-Setup-Accessory-Sign-Salt",
            "Pair-Setup-Accessory-Sign-Info",
            &k,
        );
        let ready = || {
            let mut setup = Setup::new();
            setup.session_key = Some(session);
            setup.device_x = Some(device_x);
            setup
        };
        let device = Ed25519KeyPair::from_seed_unchecked(&[7u8; 32]).unwrap();
        let device_key = device.public_key().as_ref().to_vec();
        let m6 = |signer: &Ed25519KeyPair| {
            let signed = [device_x.as_slice(), b"ATV", &device_key].concat();
            let inner = tlv8::write(&[
                (tlv8::IDENTIFIER, b"ATV"),
                (tlv8::PUBLIC_KEY, &device_key),
                (tlv8::SIGNATURE, signer.sign(&signed).as_ref()),
            ]);
            tlv8::read(&tlv8::write(&[
                (tlv8::STATE, &[6]),
                (
                    tlv8::ENCRYPTED_DATA,
                    &seal(&session, label_nonce(*b"PS-Msg06"), &[], &inner),
                ),
            ]))
            .unwrap()
        };
        let credentials = ready().finish(&m6(&device)).unwrap();
        assert_eq!(credentials.device(), "ATV");
        assert_eq!(credentials.device_key, hex(&device_key));
        // Signed by another key than the one it gives: refused.
        let other = Ed25519KeyPair::from_seed_unchecked(&[9u8; 32]).unwrap();
        let err = ready().finish(&m6(&other)).unwrap_err();
        assert!(err.to_string().contains("signature"), "{err}");
        // A message sealed under another key is refused.
        let mut stranger = ready();
        stranger.session_key = Some([1u8; 32]);
        assert!(stranger.finish(&m6(&device)).is_err());
        // Hex that is not ASCII is refused, not cut inside a character.
        assert!(unhex("é0").is_err());
    }
}
