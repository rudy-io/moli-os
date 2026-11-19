//! The client side of SRP-6a as HomeKit pairing uses it: the 3072-bit group
//! of RFC 5054, SHA-512, user `Pair-Setup`, the code on the screen as the
//! password. Byte for byte what pyatv computes through srptools (MIT):
//! numbers are hashed in their shortest big-endian form, except `g` in `k`
//! and `A`, `B` in `u`, padded to the group's length.

use anyhow::{bail, ensure};
use num_bigint::BigUint;
use ring::digest::{SHA512, digest};

/// RFC 5054, 3072 bits.
const N_HEX: &str = concat!(
    "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A08798E3404DD",
    "EF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7ED",
    "EE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF0598DA48361C55D39A69163FA8FD24CF5F",
    "83655D23DCA3AD961C62F356208552BB9ED529077096966D670C354E4ABC9804F1746C08CA18217C32905E462E36CE3B",
    "E39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF6955817183995497CEA956AE515D2261898FA0510",
    "15728E5A8AAAC42DAD33170D04507A33A85521ABDF1CBA64ECFB850458DBEF0A8AEA71575D060C7DB3970F85A6E1E4C7",
    "ABF5AE8CDB0933D71E8C94E04A25619DCEE3D2261AD2EE6BF12FFA06D98A0864D87602733EC86A64521F2B18177B200C",
    "BBE117577A615D6C770988C0BAD946E208E24FA074E5AB3143DB5BFCE0FD108E4B82D120A93AD2CAFFFFFFFFFFFFFFFF",
);
const G: u32 = 5;
pub const USER: &str = "Pair-Setup";

fn n() -> BigUint {
    BigUint::parse_bytes(N_HEX.as_bytes(), 16).unwrap_or_default()
}

/// Shortest big-endian form (zero is one byte), as srptools writes a number.
fn short(v: &BigUint) -> Vec<u8> {
    v.to_bytes_be()
}

fn pad(v: &BigUint, len: usize) -> Vec<u8> {
    let bytes = short(v);
    let mut out = vec![0u8; len.saturating_sub(bytes.len())];
    out.extend_from_slice(&bytes);
    out
}

fn hash(parts: &[&[u8]]) -> Vec<u8> {
    digest(&SHA512, &parts.concat()).as_ref().to_vec()
}

fn hash_int(parts: &[&[u8]]) -> BigUint {
    BigUint::from_bytes_be(&hash(parts))
}

/// What a client sends and keeps once the device's salt and key are known.
pub struct Client {
    /// `A`, to send.
    pub public: Vec<u8>,
    /// `M1`, to send.
    pub proof: Vec<u8>,
    /// `K`, the shared key the rest of the pairing derives from.
    pub key: Vec<u8>,
    /// `M2`, what the device must answer.
    expected: Vec<u8>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the shared key, even in a log.
        f.debug_struct("Client").finish_non_exhaustive()
    }
}

impl Client {
    /// `private` is the client's secret `a` (random, at least 32 bytes).
    #[allow(clippy::many_single_char_names)] // SRP's own names: N, g, a, b, k, x, u, v, s
    pub fn new(
        private: &[u8],
        pin: &str,
        salt: &[u8],
        device_public: &[u8],
    ) -> anyhow::Result<Self> {
        let n = n();
        let len = short(&n).len();
        let g = BigUint::from(G);
        let a = BigUint::from_bytes_be(private);
        let b = BigUint::from_bytes_be(device_public);
        ensure!(
            &b % &n != BigUint::ZERO,
            "SRP: the device's key is not valid"
        );
        let k = hash_int(&[&short(&n), &pad(&g, len)]);
        let x = hash_int(&[salt, &hash(&[USER.as_bytes(), b":", pin.as_bytes()])]);
        let a_pub = g.modpow(&a, &n);
        let u = hash_int(&[&pad(&a_pub, len), &pad(&b, len)]);
        let v = g.modpow(&x, &n);
        // (B - k·v) mod N, kept positive.
        let kv = (&k * &v) % &n;
        let base = ((&b % &n) + &n - kv) % &n;
        let s = base.modpow(&(&a + &u * &x), &n);
        let key = hash(&[&short(&s)]);
        let hn = hash_int(&[&short(&n)]);
        let hg = hash_int(&[&short(&g)]);
        let hi = hash_int(&[USER.as_bytes()]);
        let proof = hash(&[
            &short(&(hn ^ hg)),
            &short(&hi),
            salt,
            &short(&a_pub),
            &short(&b),
            &key,
        ]);
        let expected = hash(&[&short(&a_pub), &proof, &key]);
        Ok(Self {
            public: short(&a_pub),
            proof,
            key,
            expected,
        })
    }

    /// The device proved it knows the same code.
    pub fn check(&self, device_proof: &[u8]) -> anyhow::Result<()> {
        // Compared in constant time: no early exit on the first difference.
        let same = device_proof.len() == self.expected.len()
            && device_proof
                .iter()
                .zip(&self.expected)
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0;
        if !same {
            bail!("SRP: the device's proof does not match (wrong code?)");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Computed by an independent port of srptools' formulas (Python, standard
    // library), with the device side alongside: salt 00..0f, a = 0x11 × 32,
    // b = 0x22 × 32, code « 1234 ».
    const B: &str = concat!(
        "28a99552d3331b6a630094bf3ab909b64d178727f8faa5da9d7eda140ee7f0738a7948230bb5637fd01df4b9b484d3f7",
        "bb4670fe92bc0d7356c72e8adea978a16e81c5912652a80791cab85f29e8cfb1b2c26059cb4fc5b79cb671f30363d613",
        "b4e3f8b6248511249e3f626f52cf375ca8c538cd5b90bf07eab7902a9fbc8712200d4ffa1d5e08f6c379e971adf9683b",
        "29fe84b6d6ea9e6a12eafb2da9088e32b967895d17346809c4ae5f1ae7810632d43c723a361694a154b8d1bdb1abd5a8",
        "6ddf2d8ec134c0e6099a7cf5e40b5d0eda73d488554fdaff8f3171b109c525ce0f8d3112a134295642aae1250e789bb1",
        "f059764b7449f9a74b7e865e2fd55949d85456141a4d810a11343badbd5488fdf450d367634735d48e81accdfda0dba0",
        "4b1e9389f3fa6d8406fb3f14e6c0014c75e5019080a0402010969c9fdca0a2a9acfe31bfe861d39ba5bf6582bdae9704",
        "1b0d45a89b3bdc393e75dc1a2c73c263b2fd6398172038d99eedbbb2a98690543ce6f514e63284be8280ab23964c9222",
    );
    const A: &str = concat!(
        "c95341372ca4d0b469f87c0ae37bda635713223b577ef5e8d64a959a54e5395722fea50e71bbd44306055760924a6488",
        "5805ae13545fcfc7e65f4e7b75c765a112d267f79bffb8353d96ae81cfa64ca7eb687a5dba4cf223b21e1362489ca3e0",
        "56254b25f610c00643490ea19944f6d3d0872bdcc5fc338bebc8936f30b695924e117f364a4cfc3898eeaa1a4bb98957",
        "df8eb046eba3cf42558138ca616a1dc7991426292dd258693bbd4c11816e3064e4a7c536f7255ae61735db25e3cf0349",
        "eb8b3e9848d0aa572b0f809d8983f1b280581aba96104422fcf088b698f8d899115a5653cc473e87f5e45552a5a126c8",
        "b7fc273bd2cd12bfb15ff3501a572b17608f16c33f6e971081f3af1be37a0a96748f60d204e935f6a5d2a71303aea434",
        "b45bcb9ab6b4444208d3fdf7a00c07a3ba2a09b56b578170c78ba1573ca2692406ba8d461e43bb3d160f0024a388d723",
        "4236537e29c3604f666c69d7244eae29a0e8b9fa5ee490890e5892973fa535707fbe8f06e90dcbb99c7b39b5eb2b45fb",
    );
    const K: &str = concat!(
        "2f626fe5ae691b367fc11f23f0f14dcaf752d8af654c656c30a67985318e92f7ba1196c05c4f7747c0294c009cba4de3",
        "d4bd9c3c3e379fe3a7dcf621ac7167ca",
    );
    const M1: &str = concat!(
        "07e0a5ba1ad1f5ba640c21060bd63491bf81346f5227b45af783934d01470055dd06a6019391d1e6a02127ecc456f3b3",
        "b870bd3e36b7a0bf5360a8f8c3b2da12",
    );
    const M2: &str = concat!(
        "1dd6aea43546d37b835ab8590beaf2210d6233ab74613e01079076d467aac30effb9f4ff57c6f6e5692cbff741613c5b",
        "a58e7c44b94bf80c70204edc13867355",
    );

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn the_group_is_rfc_5054s() {
        let n = n();
        assert_eq!(n.bits(), 3072);
        assert_eq!(short(&n)[..4], [0xFF; 4]);
    }

    #[test]
    fn matches_srptools_byte_for_byte() {
        let salt: Vec<u8> = (0..16).collect();
        let client = Client::new(&[0x11; 32], "1234", &salt, &hex(B)).unwrap();
        assert_eq!(client.public, hex(A));
        assert_eq!(client.key, hex(K));
        assert_eq!(client.proof, hex(M1));
        client.check(&hex(M2)).unwrap();
        assert!(client.check(&hex(M1)).is_err());
        // Another code gives another key: the device's proof would not match.
        let wrong = Client::new(&[0x11; 32], "4321", &salt, &hex(B)).unwrap();
        assert_ne!(wrong.key, client.key);
        assert!(wrong.check(&hex(M2)).is_err());
    }

    #[test]
    fn a_device_key_of_zero_is_refused() {
        assert!(Client::new(&[0x11; 32], "1234", &[0; 16], &[0]).is_err());
        assert!(Client::new(&[0x11; 32], "1234", &[0; 16], &short(&n())).is_err());
    }
}
