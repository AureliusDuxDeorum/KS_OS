//! Bluetooth LE security toolbox (Core Spec Vol 3, Part H, 2.2): AES-128, AES-CMAC, the
//! LE Secure Connections functions f4/f5/f6/g2, and P-256 ECDH.
//!
//! Byte-order convention: the specification writes multi-octet values most-significant-first;
//! on the wire (and in this crate's public API) they are little-endian. The functions below take
//! and return **little-endian** values and reverse internally where the algorithm is defined on
//! big-endian numbers.

use aes::cipher::{BlockEncrypt, KeyInit};
use aes::Aes128;
use cmac::{Cmac, Mac};
use p256::ecdh::diffie_hellman;
use p256::elliptic_curve::sec1::{FromEncodedPoint, ToEncodedPoint};
use p256::{EncodedPoint, PublicKey, SecretKey};
use zeroize::Zeroize;

fn rev<const N: usize>(mut a: [u8; N]) -> [u8; N] {
    a.reverse();
    a
}

/// AES-128 single-block encryption: `e(key, plaintext)`; all values big-endian (as in the AES spec).
pub fn aes128(key: &[u8; 16], block: &[u8; 16]) -> [u8; 16] {
    let cipher = Aes128::new(key.into());
    let mut b = (*block).into();
    cipher.encrypt_block(&mut b);
    b.into()
}

/// AES-CMAC (RFC 4493) over `msg` with `key`; big-endian message and key as in the RFC.
pub fn aes_cmac(key: &[u8; 16], msg: &[u8]) -> [u8; 16] {
    // `new_from_slice` cannot fail for a 16-byte key.
    let mut mac = <Cmac<Aes128> as Mac>::new_from_slice(key).expect("16-byte key");
    mac.update(msg);
    mac.finalize().into_bytes().into()
}

/// f4 (confirm value): `AES-CMAC_X(U || V || Z)`; U, V are 32-byte little-endian X coordinates,
/// X is the 16-byte little-endian key, Z a single byte. Returns the 16-byte little-endian result.
pub fn f4(u: &[u8; 32], v: &[u8; 32], x: &[u8; 16], z: u8) -> [u8; 16] {
    let mut m = [0u8; 65];
    m[..32].copy_from_slice(&rev(*u));
    m[32..64].copy_from_slice(&rev(*v));
    m[64] = z;
    rev(aes_cmac(&rev(*x), &m))
}

/// f5 (key generation): derives `(MacKey, LTK)` from the DH key, both nonces and both
/// 7-byte addresses (type byte followed by the address, little-endian wire order).
pub fn f5(
    dhkey: &[u8; 32],
    n1: &[u8; 16],
    n2: &[u8; 16],
    a1: &[u8; 7],
    a2: &[u8; 7],
) -> ([u8; 16], [u8; 16]) {
    // SALT = 6C888391 AAF5A538 60370BDB 5A6083BE (big-endian as printed in the spec).
    const SALT: [u8; 16] = [
        0x6C, 0x88, 0x83, 0x91, 0xAA, 0xF5, 0xA5, 0x38, 0x60, 0x37, 0x0B, 0xDB, 0x5A, 0x60, 0x83,
        0xBE,
    ];
    let mut w = rev(*dhkey);
    let t = aes_cmac(&SALT, &w);
    w.zeroize();
    // keyID = "btle" = 0x62746C65
    let key_id = [0x62, 0x74, 0x6C, 0x65];
    let block = |counter: u8| {
        let mut m = [0u8; 53];
        m[0] = counter;
        m[1..5].copy_from_slice(&key_id);
        m[5..21].copy_from_slice(&rev(*n1));
        m[21..37].copy_from_slice(&rev(*n2));
        m[37..44].copy_from_slice(&rev(*a1));
        m[44..51].copy_from_slice(&rev(*a2));
        m[51..53].copy_from_slice(&[0x01, 0x00]); // length = 256 bits
        rev(aes_cmac(&t, &m))
    };
    (block(0), block(1))
}

/// f6 (check value): `AES-CMAC_W(N1 || N2 || R || IOcap || A1 || A2)`.
pub fn f6(
    w: &[u8; 16],
    n1: &[u8; 16],
    n2: &[u8; 16],
    r: &[u8; 16],
    io_cap: &[u8; 3],
    a1: &[u8; 7],
    a2: &[u8; 7],
) -> [u8; 16] {
    let mut m = [0u8; 65];
    m[..16].copy_from_slice(&rev(*n1));
    m[16..32].copy_from_slice(&rev(*n2));
    m[32..48].copy_from_slice(&rev(*r));
    m[48..51].copy_from_slice(&rev(*io_cap));
    m[51..58].copy_from_slice(&rev(*a1));
    m[58..65].copy_from_slice(&rev(*a2));
    rev(aes_cmac(&rev(*w), &m))
}

/// g2 (numeric comparison value): the six-digit number both devices display.
pub fn g2(u: &[u8; 32], v: &[u8; 32], x: &[u8; 16], y: &[u8; 16]) -> u32 {
    let mut m = [0u8; 80];
    m[..32].copy_from_slice(&rev(*u));
    m[32..64].copy_from_slice(&rev(*v));
    m[64..80].copy_from_slice(&rev(*y));
    let mac = aes_cmac(&rev(*x), &m);
    u32::from_be_bytes([mac[12], mac[13], mac[14], mac[15]]) % 1_000_000
}

/// A P-256 key pair for one pairing; the secret is wiped when dropped.
pub struct EphemeralKey {
    secret: SecretKey,
}

#[derive(Debug, PartialEq, Eq)]
pub enum EcdhError {
    /// The peer's public key is not a valid point on P-256 (invalid-curve attack or corruption).
    InvalidPublicKey,
}

impl EphemeralKey {
    /// Generates a key pair using `rng`.
    pub fn generate(rng: &mut dyn crate::Rng) -> Self {
        loop {
            let mut bytes = [0u8; 32];
            rng.fill(&mut bytes);
            let key = SecretKey::from_slice(&bytes);
            bytes.zeroize();
            if let Ok(secret) = key {
                return Self { secret };
            }
        }
    }

    /// Builds a key pair from a fixed secret (little-endian, as printed in SMP test data reversed).
    pub fn from_secret_be(secret: &[u8; 32]) -> Option<Self> {
        SecretKey::from_slice(secret)
            .ok()
            .map(|secret| Self { secret })
    }

    /// Public key as little-endian `(X, Y)`.
    pub fn public(&self) -> ([u8; 32], [u8; 32]) {
        let point = self.secret.public_key().to_encoded_point(false);
        let (mut x, mut y) = ([0u8; 32], [0u8; 32]);
        x.copy_from_slice(point.x().expect("uncompressed point"));
        y.copy_from_slice(point.y().expect("uncompressed point"));
        (rev(x), rev(y))
    }

    /// Computes the shared DH key with the peer's little-endian public key, validating that the
    /// point lies on the curve. The result is the little-endian X coordinate.
    pub fn dh(&self, peer_x: &[u8; 32], peer_y: &[u8; 32]) -> Result<[u8; 32], EcdhError> {
        let point = EncodedPoint::from_affine_coordinates(
            &rev(*peer_x).into(),
            &rev(*peer_y).into(),
            false,
        );
        let public = Option::<PublicKey>::from(PublicKey::from_encoded_point(&point))
            .ok_or(EcdhError::InvalidPublicKey)?;
        let shared = diffie_hellman(self.secret.to_nonzero_scalar(), public.as_affine());
        let mut out = [0u8; 32];
        out.copy_from_slice(shared.raw_secret_bytes());
        Ok(rev(out))
    }
}

/// Resolvable-private-address hash function `ah(k, r)`: the low 24 bits of `e(k, padding || r)`.
/// `r` is the 3-byte `prand` and the result the 3-byte hash, both little-endian.
pub fn ah(irk: &[u8; 16], prand: &[u8; 3]) -> [u8; 3] {
    let mut block = [0u8; 16];
    block[13..16].copy_from_slice(&rev(*prand));
    let out = aes128(&rev(*irk), &block);
    let mut h = [out[13], out[14], out[15]];
    h.reverse();
    h
}

/// True if `address` is a resolvable private address generated from `irk`.
pub fn resolves(irk: &[u8; 16], address: &[u8; 6]) -> bool {
    // Wire order is little-endian: bytes 0..3 are the hash, bytes 3..6 the prand; the top two
    // bits of the most significant prand byte must be 0b01.
    if address[5] & 0xC0 != 0x40 {
        return false;
    }
    ah(irk, &[address[3], address[4], address[5]]) == [address[0], address[1], address[2]]
}
