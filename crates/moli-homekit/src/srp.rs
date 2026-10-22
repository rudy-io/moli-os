//! SRP-6a, the server's side, as HomeKit's pair-setup uses it: the
//! 3072-bit group of RFC 5054, SHA-512, the user `Pair-Setup` and the
//! setup code as password. `k` and `u` hash padded values; the session key
//! is the hash of the padded secret.

use num_bigint::BigUint;
use ring::digest::{self, Algorithm};

/// RFC 5054, 3072-bit group, generator 5.
const N_3072: &str = "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7EDEE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF0598DA48361C55D39A69163FA8FD24CF5F83655D23DCA3AD961C62F356208552BB9ED529077096966D670C354E4ABC9804F1746C08CA18217C32905E462E36CE3BE39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF6955817183995497CEA956AE515D2261898FA051015728E5A8AAAC42DAD33170D04507A33A85521ABDF1CBA64ECFB850458DBEF0A8AEA71575D060C7DB3970F85A6E1E4C7ABF5AE8CDB0933D71E8C94E04A25619DCEE3D2261AD2EE6BF12FFA06D98A0864D87602733EC86A64521F2B18177B200CBBE117577A615D6C770988C0BAD946E208E24FA074E5AB3143DB5BFCE0FD108E4B82D120A93AD2CAFFFFFFFFFFFFFFFF";

#[derive(Debug)]
pub struct Group {
    n: BigUint,
    g: BigUint,
    len: usize,
    hash: &'static Algorithm,
}

impl Group {
    pub fn homekit() -> Self {
        Self::new(N_3072, 5, &digest::SHA512)
    }

    fn new(n_hex: &str, g: u32, hash: &'static Algorithm) -> Self {
        let n = BigUint::parse_bytes(n_hex.as_bytes(), 16).expect("a known group");
        let len = usize::try_from(n.bits().div_ceil(8)).expect("small");
        Self {
            n,
            g: BigUint::from(g),
            len,
            hash,
        }
    }

    fn pad(&self, n: &BigUint) -> Vec<u8> {
        let bytes = n.to_bytes_be();
        let mut out = vec![0u8; self.len.saturating_sub(bytes.len())];
        out.extend_from_slice(&bytes);
        out
    }

    fn h(&self, parts: &[&[u8]]) -> Vec<u8> {
        let mut ctx = digest::Context::new(self.hash);
        for p in parts {
            ctx.update(p);
        }
        ctx.finish().as_ref().to_vec()
    }

    fn h_int(&self, parts: &[&[u8]]) -> BigUint {
        BigUint::from_bytes_be(&self.h(parts))
    }
}

/// One pair-setup attempt: the verifier, our secret and public values.
pub struct Server {
    group: Group,
    username: Vec<u8>,
    salt: Vec<u8>,
    v: BigUint,
    b: BigUint,
    big_b: BigUint,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Server(<redacted>)")
    }
}

impl Server {
    /// `b`: our secret, random (32 bytes or more).
    pub fn new(group: Group, username: &str, password: &str, salt: &[u8], b: &[u8]) -> Self {
        let inner = group.h(&[format!("{username}:{password}").as_bytes()]);
        let x = group.h_int(&[salt, &inner]);
        let v = group.g.modpow(&x, &group.n);
        let k = group.h_int(&[&group.pad(&group.n), &group.pad(&group.g)]);
        let b = BigUint::from_bytes_be(b);
        let big_b = (k * &v + group.g.modpow(&b, &group.n)) % &group.n;
        Self {
            username: username.as_bytes().to_vec(),
            salt: salt.to_vec(),
            v,
            b,
            big_b,
            group,
        }
    }

    /// `B`, padded to the group's size.
    pub fn public(&self) -> Vec<u8> {
        self.group.pad(&self.big_b)
    }

    fn secret(&self, a: &BigUint) -> BigUint {
        let g = &self.group;
        let u = g.h_int(&[&g.pad(a), &g.pad(&self.big_b)]);
        (a * self.v.modpow(&u, &g.n)).modpow(&self.b, &g.n)
    }

    /// The client's `A` and proof: our proof and the session key, if the
    /// client knew the code.
    pub fn verify(&self, a_bytes: &[u8], proof: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
        let g = &self.group;
        let a = BigUint::from_bytes_be(a_bytes);
        // A ≡ 0 mod N would make the secret predictable.
        if (&a % &g.n) == BigUint::ZERO {
            return None;
        }
        let key = g.h(&[&g.pad(&self.secret(&a))]);
        let hn = g.h(&[&g.n.to_bytes_be()]);
        let hg = g.h(&[&g.g.to_bytes_be()]);
        let xor: Vec<u8> = hn.iter().zip(&hg).map(|(x, y)| x ^ y).collect();
        let expected = g.h(&[
            &xor,
            &g.h(&[&self.username]),
            &self.salt,
            a_bytes,
            &self.public(),
            &key,
        ]);
        if !same(&expected, proof) {
            return None;
        }
        let ours = g.h(&[a_bytes, proof, &key]);
        Some((ours, key))
    }
}

/// Equal, in a time that does not depend on where they differ.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(hex: &str) -> BigUint {
        let clean: String = hex.chars().filter(char::is_ascii_hexdigit).collect();
        BigUint::parse_bytes(clean.as_bytes(), 16).unwrap()
    }

    fn bytes(hex: &str) -> Vec<u8> {
        int(hex).to_bytes_be()
    }

    // RFC 5054, appendix B (1024-bit group, SHA-1): the same formulas.
    const N_1024: &str = "EEAF0AB9ADB38DD69C33F80AFA8FC5E86072618775FF3C0B9EA2314C9C256576D674DF7496EA81D3383B4813D692C6E0E0D5D8E250B98BE48E495C1D6089DAD15DC7D7B46154D6B6CE8EF4AD69B15D4982559B297BCF1885C529F566660E57EC68EDBC3C05726CC02FD4CBF4976EAA9AFD5138FE8376435B9FC61D2FC0EB06E3";

    #[test]
    fn the_rfc_5054_vectors_hold() {
        let group = Group::new(N_1024, 2, &digest::SHA1_FOR_LEGACY_USE_ONLY);
        let salt = bytes("BEB25379 D1A8581E B5A72767 3A2441EE");
        let b = bytes("E487CB59 D31AC550 471E81F0 0F6928E0 1DDA08E9 74A004F4 9E61F5D1 05284D20");
        let server = Server::new(group, "alice", "password123", &salt, &b);
        assert_eq!(
            server.v,
            int(
                "7E273DE8 696FFC4F 4E337D05 B4B375BE B0DDE156 9E8FA00A 9886D812 9BADA1F1
                 822223CA 1A605B53 0E379BA4 729FDC59 F105B478 7E5186F5 C671085A 1447B52A
                 48CF1970 B4FB6F84 00BBF4CE BFBB1681 52E08AB5 EA53D15C 1AFF87B2 B9DA6E04
                 E058AD51 CC72BFC9 033B564E 26480D78 E955A5E2 9E7AB245 DB2BE315 E2099AFB"
            )
        );
        assert_eq!(
            server.big_b,
            int(
                "BD0C6151 2C692C0C B6D041FA 01BB152D 4916A1E7 7AF46AE1 05393011 BAF38964
                 DC46A067 0DD125B9 5A981652 236F99D9 B681CBF8 7837EC99 6C6DA044 53728610
                 D0C6DDB5 8B318885 D7D82C7F 8DEB75CE 7BD4FBAA 37089E6F 9C6059F3 88838E7A
                 00030B33 1EB76840 910440B1 B27AAEAE EB4012B7 D7665238 A8E3FB00 4B117B58"
            )
        );
        let a = int(
            "61D5E490 F6F1B795 47B0704C 436F523D D0E560F0 C64115BB 72557EC4 4352E890
                     3211C046 92272D8B 2D1A5358 A2CF1B6E 0BFCF99F 921530EC 8E393561 79EAE45E
                     42BA92AE ACED8251 71E1E8B9 AF6D9C03 E1327F44 BE087EF0 6530E69F 66615261
                     EEF54073 CA11CF58 58F0EDFD FE15EFEA B349EF5D 76988A36 72FAC47B 0769447B",
        );
        assert_eq!(
            server.secret(&a),
            int(
                "B0DC82BA BCF30674 AE450C02 87745E79 90A3381F 63B387AA F271A10D 233861E3
                 59B48220 F7C4693C 9AE12B0A 6F67809F 0876E2D0 13800D6C 41BB59B6 D5979B5C
                 00A172B4 A2A5903A 0BDCAF8A 709585EB 2AFAFA8F 3499B200 210DCC1F 10EB3394
                 3CD67FC8 8A2F39A4 BE5BEC4E C0A3212D C346D7E4 74B29EDE 8A469FFE CA686E5A"
            )
        );
    }

    /// A client written from the same formulas, in HomeKit's group.
    #[test]
    #[allow(clippy::many_single_char_names)]
    fn a_client_with_the_code_is_accepted_and_others_not() {
        let salt = [3u8; 16];
        let server = Server::new(
            Group::homekit(),
            "Pair-Setup",
            "123-45-678",
            &salt,
            &[9u8; 32],
        );
        let g = Group::homekit();
        let client = |code: &str| {
            let a = BigUint::from_bytes_be(&[5u8; 32]);
            let big_a = g.g.modpow(&a, &g.n);
            let a_bytes = g.pad(&big_a);
            let big_b = BigUint::from_bytes_be(&server.public());
            let k = g.h_int(&[&g.pad(&g.n), &g.pad(&g.g)]);
            let u = g.h_int(&[&a_bytes, &server.public()]);
            let inner = g.h(&[format!("Pair-Setup:{code}").as_bytes()]);
            let x = g.h_int(&[&salt, &inner]);
            let gx = g.g.modpow(&x, &g.n);
            let base = (&big_b + &g.n * &k - (&k * gx) % &g.n) % &g.n;
            let s = base.modpow(&(&a + u * x), &g.n);
            let key = g.h(&[&g.pad(&s)]);
            let hn = g.h(&[&g.n.to_bytes_be()]);
            let hg = g.h(&[&g.g.to_bytes_be()]);
            let xor: Vec<u8> = hn.iter().zip(&hg).map(|(x, y)| x ^ y).collect();
            let m1 = g.h(&[
                &xor,
                &g.h(&[b"Pair-Setup"]),
                &salt,
                &a_bytes,
                &server.public(),
                &key,
            ]);
            (a_bytes, m1, key)
        };
        let (a, m1, key) = client("123-45-678");
        let (m2, server_key) = server.verify(&a, &m1).expect("the right code");
        assert_eq!(server_key, key);
        assert_eq!(m2, g.h(&[&a, &m1, &key]));
        let (a, m1, _) = client("123-45-679");
        assert!(server.verify(&a, &m1).is_none());
        assert!(server.verify(&[0], &m1).is_none());
    }
}
