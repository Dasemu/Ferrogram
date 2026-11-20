//! Diffie-Hellman for one-to-one calls: parameter checks, `g_a`/`g_b`, the
//! shared key, its 64-bit fingerprint and the four-emoji visualisation.
//!
//! The checks follow the MTProto rules for end-to-end keys: `p` is a 2048-bit
//! safe prime, `g` generates the subgroup of order `(p-1)/2`, and every public
//! value lies in `[2^(2048-64), p - 2^(2048-64)]`.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use num_bigint::BigUint;
use rand::RngCore;
use sha1::Digest as _;

use super::emoji::EMOJI;

/// Size of `p`, `g_a`, `g_b` and the shared key in bytes.
pub const KEY_LEN: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DhError {
    #[error("p is not a 2048-bit safe prime")]
    BadPrime,
    #[error("g does not generate the right subgroup")]
    BadGenerator,
    #[error("public value out of range")]
    BadPublic,
    #[error("server random too short")]
    BadRandom,
    #[error("g_a does not match its hash")]
    HashMismatch,
    #[error("key fingerprint mismatch")]
    FingerprintMismatch,
}

/// `messages.dhConfig` as the app keeps it.
#[derive(Clone, PartialEq, Eq)]
pub struct DhConfig {
    pub g: i32,
    pub p: Vec<u8>,
    pub version: i32,
    pub random: Vec<u8>,
}

impl std::fmt::Debug for DhConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DhConfig")
            .field("g", &self.g)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

/// Our secret exponent. Never printed.
#[derive(Clone)]
pub struct Secret(Vec<u8>);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// The 256-byte call key. Never printed.
#[derive(Clone, PartialEq, Eq)]
pub struct CallKey(pub [u8; KEY_LEN]);

impl std::fmt::Debug for CallKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CallKey(..)")
    }
}

fn verified_primes() -> &'static Mutex<HashSet<Vec<u8>>> {
    static CACHE: OnceLock<Mutex<HashSet<Vec<u8>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Check `g` and `p` as the protocol requires. The primality test is slow
/// (tens of 2048-bit exponentiations): call it off the UI thread; a prime
/// that passed once is remembered for the process lifetime.
pub fn check_config(g: i32, p: &[u8]) -> Result<(), DhError> {
    if p.len() != KEY_LEN || p[0] & 0x80 == 0 {
        return Err(DhError::BadPrime);
    }
    let pn = BigUint::from_bytes_be(p);
    check_generator(g, &pn)?;
    let known = verified_primes()
        .lock()
        .map(|c| c.contains(p))
        .unwrap_or(false);
    if known {
        return Ok(());
    }
    let q: BigUint = (&pn - 1u32) >> 1;
    if !is_probable_prime(&pn, 16) || !is_probable_prime(&q, 16) {
        return Err(DhError::BadPrime);
    }
    if let Ok(mut c) = verified_primes().lock() {
        if c.len() > 8 {
            c.clear();
        }
        c.insert(p.to_vec());
    }
    Ok(())
}

/// The quadratic-residue conditions that make `g` generate the subgroup of
/// order `(p-1)/2` for a safe prime `p`.
fn check_generator(g: i32, p: &BigUint) -> Result<(), DhError> {
    let m = |n: u32| (p % n).to_u32_digits().first().copied().unwrap_or(0);
    let ok = match g {
        2 => m(8) == 7,
        3 => m(3) == 2,
        4 => true,
        5 => matches!(m(5), 1 | 4),
        6 => matches!(m(24), 19 | 23),
        7 => matches!(m(7), 3 | 5 | 6),
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(DhError::BadGenerator)
    }
}

/// Miller-Rabin with random bases (plus small-prime trial division).
pub fn is_probable_prime(n: &BigUint, rounds: usize) -> bool {
    let two = BigUint::from(2u32);
    if *n < two {
        return false;
    }
    for sp in [2u32, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37] {
        let spn = BigUint::from(sp);
        if *n == spn {
            return true;
        }
        if (n % sp) == BigUint::ZERO {
            return false;
        }
    }
    let one = BigUint::from(1u32);
    let n1 = n - &one;
    let s = n1.trailing_zeros().unwrap_or(0);
    let d = &n1 >> s;
    let mut rng = rand::rng();
    let span = n - 3u32;
    let mut buf = vec![0u8; usize::try_from(n.bits().div_ceil(8)).unwrap_or(256) + 8];
    'outer: for _ in 0..rounds {
        rng.fill_bytes(&mut buf);
        // A random base in [2, n-2].
        let a = BigUint::from_bytes_be(&buf) % &span + &two;
        let mut x = a.modpow(&d, n);
        if x == one || x == n1 {
            continue;
        }
        for _ in 1..s {
            x = x.modpow(&two, n);
            if x == n1 {
                continue 'outer;
            }
        }
        return false;
    }
    true
}

/// `1 < x < p-1` and `2^(2048-64) <= x <= p - 2^(2048-64)`.
pub fn check_public(x: &[u8], p: &[u8]) -> Result<(), DhError> {
    if x.len() > KEY_LEN || x.is_empty() {
        return Err(DhError::BadPublic);
    }
    let xn = BigUint::from_bytes_be(x);
    let pn = BigUint::from_bytes_be(p);
    let margin = BigUint::from(1u32) << (2048 - 64);
    if xn <= BigUint::from(1u32) || xn >= &pn - 1u32 {
        return Err(DhError::BadPublic);
    }
    if xn < margin || pn < &margin + &margin || xn > &pn - &margin {
        return Err(DhError::BadPublic);
    }
    Ok(())
}

/// A fresh secret: 256 random bytes XOR the server's random (as the docs ask,
/// so a weak local RNG alone cannot predict it).
pub fn new_secret(server_random: &[u8]) -> Result<Secret, DhError> {
    if server_random.len() < KEY_LEN {
        return Err(DhError::BadRandom);
    }
    let mut a = vec![0u8; KEY_LEN];
    rand::rng().fill_bytes(&mut a);
    for (x, r) in a.iter_mut().zip(server_random) {
        *x ^= r;
    }
    Ok(Secret(a))
}

#[cfg(test)]
pub fn secret_from_bytes(b: &[u8]) -> Secret {
    Secret(b.to_vec())
}

fn to_fixed(n: &BigUint) -> [u8; KEY_LEN] {
    let bytes = n.to_bytes_be();
    let mut out = [0u8; KEY_LEN];
    let start = KEY_LEN.saturating_sub(bytes.len());
    let src = &bytes[bytes.len().saturating_sub(KEY_LEN)..];
    out[start..].copy_from_slice(src);
    out
}

/// `g^secret mod p`, 256 bytes big-endian. Fails if it is out of range (then
/// pick a new secret).
pub fn public_value(g: i32, secret: &Secret, p: &[u8]) -> Result<Vec<u8>, DhError> {
    let g = u32::try_from(g).map_err(|_| DhError::BadGenerator)?;
    let pn = BigUint::from_bytes_be(p);
    let x = BigUint::from(g).modpow(&BigUint::from_bytes_be(&secret.0), &pn);
    let out = to_fixed(&x).to_vec();
    check_public(&out, p)?;
    Ok(out)
}

/// The shared key `other^secret mod p` (checks `other` first).
pub fn shared_key(other: &[u8], secret: &Secret, p: &[u8]) -> Result<CallKey, DhError> {
    check_public(other, p)?;
    let pn = BigUint::from_bytes_be(p);
    let k = BigUint::from_bytes_be(other).modpow(&BigUint::from_bytes_be(&secret.0), &pn);
    Ok(CallKey(to_fixed(&k)))
}

/// Lower 64 bits of SHA-1(key), little-endian (like an auth key id).
pub fn key_fingerprint(key: &CallKey) -> i64 {
    let h = sha1::Sha1::digest(key.0);
    let mut b = [0u8; 8];
    b.copy_from_slice(&h[12..20]);
    i64::from_le_bytes(b)
}

/// `SHA-256(g_a)`, sent by the caller before it reveals `g_a`.
pub fn g_a_hash(g_a: &[u8]) -> [u8; 32] {
    sha2::Sha256::digest(g_a).into()
}

/// Check the revealed `g_a` against the hash from `phoneCallRequested`.
pub fn check_g_a_hash(g_a: &[u8], hash: &[u8]) -> Result<(), DhError> {
    if g_a_hash(g_a).as_slice() == hash {
        Ok(())
    } else {
        Err(DhError::HashMismatch)
    }
}

/// The four emoji both parties compare: SHA-256(key || g_a), split into four
/// 64-bit big-endian numbers (top bit cleared), each mod the table size.
pub fn emoji_fingerprint(key: &CallKey, g_a: &[u8]) -> [&'static str; 4] {
    let mut h = sha2::Sha256::new();
    h.update(key.0);
    h.update(g_a);
    let d = h.finalize();
    let mut out = [""; 4];
    for (i, slot) in out.iter_mut().enumerate() {
        let mut b = [0u8; 8];
        b.copy_from_slice(&d[i * 8..i * 8 + 8]);
        b[0] &= 0x7f;
        let v = u64::from_be_bytes(b);
        let idx = usize::try_from(v % EMOJI.len() as u64).unwrap_or(0);
        *slot = EMOJI[idx];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 3526 group 14 (2048-bit MODP): a well-known safe prime with g = 2.
    pub const RFC3526_P: &str = "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7EDEE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF0598DA48361C55D39A69163FA8FD24CF5F83655D23DCA3AD961C62F356208552BB9ED529077096966D670C354E4ABC9804F1746C08CA18217C32905E462E36CE3BE39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF6955817183995497CEA956AE515D2261898FA051015728E5A8AACAA68FFFFFFFFFFFFFFFF";

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn rfc3526_prime_passes() {
        let p = hex(RFC3526_P);
        // RFC 3526 p mod 8 == 7, so g = 2 is valid.
        assert_eq!(check_config(2, &p), Ok(()));
        // Cached the second time.
        assert_eq!(check_config(2, &p), Ok(()));
    }

    #[test]
    fn bad_primes_and_generators_fail() {
        let mut p = hex(RFC3526_P);
        assert_eq!(check_config(9, &p), Err(DhError::BadGenerator));
        // RFC 3526 p is 7 mod 8, 2 mod 3, 4 mod 5, 23 mod 24, 5 mod 7: every g is fine.
        for g in 2..=7 {
            assert_eq!(check_generator(g, &BigUint::from_bytes_be(&p)), Ok(()));
        }
        let small = |n: u32| BigUint::from(n);
        assert!(check_generator(2, &small(13)).is_err()); // 5 mod 8
        assert!(check_generator(3, &small(13)).is_err()); // 1 mod 3
        assert!(check_generator(5, &small(13)).is_err()); // 3 mod 5
        assert!(check_generator(6, &small(13)).is_err());
        assert!(check_generator(7, &small(15)).is_err()); // 1 mod 7
        p[200] ^= 0x10;
        assert!(check_config(4, &p).is_err());
        assert_eq!(check_config(2, &p[1..]), Err(DhError::BadPrime));
        assert_eq!(check_config(2, &[0u8; 256]), Err(DhError::BadPrime));
    }

    #[test]
    fn small_primes() {
        let primes: Vec<u32> = (2..200)
            .filter(|n| is_probable_prime(&BigUint::from(*n), 8))
            .collect();
        assert_eq!(&primes[..10], &[2, 3, 5, 7, 11, 13, 17, 19, 23, 29]);
        assert!(!is_probable_prime(&BigUint::from(561u32), 8)); // Carmichael
        assert!(is_probable_prime(&BigUint::from(1_000_000_007u32), 8));
    }

    #[test]
    fn public_value_range() {
        let p = hex(RFC3526_P);
        assert_eq!(check_public(&[1], &p), Err(DhError::BadPublic));
        assert_eq!(check_public(&[2], &p), Err(DhError::BadPublic));
        let mut big = p.clone();
        big[255] -= 1; // p - 1
        assert_eq!(check_public(&big, &p), Err(DhError::BadPublic));
        assert_eq!(check_public(&p, &p), Err(DhError::BadPublic));
        assert_eq!(check_public(&[0u8; 300], &p), Err(DhError::BadPublic));
        let mut mid = vec![0u8; 256];
        mid[0] = 0x40;
        assert_eq!(check_public(&mid, &p), Ok(()));
    }

    #[test]
    fn both_sides_agree() {
        let p = hex(RFC3526_P);
        let random = vec![7u8; 256];
        let a = new_secret(&random).unwrap();
        let b = new_secret(&random).unwrap();
        let ga = public_value(2, &a, &p).unwrap();
        let gb = public_value(2, &b, &p).unwrap();
        assert_eq!(ga.len(), 256);
        let ka = shared_key(&gb, &a, &p).unwrap();
        let kb = shared_key(&ga, &b, &p).unwrap();
        assert_eq!(ka, kb);
        assert_eq!(key_fingerprint(&ka), key_fingerprint(&kb));
        assert_eq!(emoji_fingerprint(&ka, &ga), emoji_fingerprint(&kb, &ga));
        check_g_a_hash(&ga, &g_a_hash(&ga)).unwrap();
        assert_eq!(
            check_g_a_hash(&gb, &g_a_hash(&ga)),
            Err(DhError::HashMismatch)
        );
        assert_eq!(new_secret(&[0u8; 10]).unwrap_err(), DhError::BadRandom);
    }

    /// Known vectors computed independently (Python hashlib):
    /// key = bytes(range(256)); g_a = bytes(255 - i for i in range(256)).
    #[test]
    fn fingerprint_vectors() {
        let mut k = [0u8; 256];
        for (i, b) in k.iter_mut().enumerate() {
            *b = i as u8;
        }
        let key = CallKey(k);
        let ga: Vec<u8> = (0..256).map(|i| 255 - i as u8).collect();
        assert_eq!(key_fingerprint(&key), FP_VECTOR);
        assert_eq!(emoji_fingerprint(&key, &ga), EMOJI_VECTOR.map(|i| EMOJI[i]));
        assert_eq!(hex_str(&g_a_hash(&ga)), GA_HASH_VECTOR);
    }

    fn hex_str(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    const FP_VECTOR: i64 = -3972359982579920590;
    const EMOJI_VECTOR: [usize; 4] = [122, 225, 65, 23];
    const GA_HASH_VECTOR: &str =
        "cd6816b77f68d70001fc3eaa4d42bdd67cb5973b3151cc5292ecc02a3daac6ab";
}
