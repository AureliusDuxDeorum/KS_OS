//! Cryptographic functions against the Bluetooth Core Specification sample data
//! (Vol 3, Part H, Appendix D) and RFC 4493.

use ksbt::crypto::*;

fn hex<const N: usize>(s: &str) -> [u8; N] {
    let s: std::string::String = s.split_whitespace().collect();
    assert_eq!(s.len(), N * 2, "bad test vector length");
    let mut out = [0u8; N];
    for i in 0..N {
        out[i] = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
    }
    out
}

/// Spec sample data is printed most-significant-byte first; the API is little-endian.
fn le<const N: usize>(s: &str) -> [u8; N] {
    let mut a = hex::<N>(s);
    a.reverse();
    a
}

#[test]
fn aes_cmac_rfc4493() {
    let key = hex::<16>("2b7e1516 28aed2a6 abf71588 09cf4f3c");
    assert_eq!(
        aes_cmac(&key, &[]),
        hex::<16>("bb1d6929 e9593728 7fa37d12 9b756746")
    );
    assert_eq!(
        aes_cmac(&key, &hex::<16>("6bc1bee2 2e409f96 e93d7e11 7393172a")),
        hex::<16>("070a16b4 6b4d4144 f79bdd9d d04a287c")
    );
    let m40 = hex::<40>(
        "6bc1bee2 2e409f96 e93d7e11 7393172a ae2d8a57 1e03ac9c 9eb76fac 45af8e51 30c81c46 a35ce411",
    );
    assert_eq!(
        aes_cmac(&key, &m40),
        hex::<16>("dfa66747 de9ae630 30ca3261 1497c827")
    );
}

#[test]
fn aes128_fips197() {
    let key = hex::<16>("000102030405060708090a0b0c0d0e0f");
    assert_eq!(
        aes128(&key, &hex::<16>("00112233445566778899aabbccddeeff")),
        hex::<16>("69c4e0d86a7b0430d8cdb78070b4c55a")
    );
}

#[test]
fn p256_ecdh_spec_sample_data() {
    // Appendix D.1: private keys, public keys and the resulting DH key.
    let a = EphemeralKey::from_secret_be(&hex(
        "3f49f6d4 a3c55f38 74c9b3e3 d2103f50 4aff607b eb40b799 5899b8a6 cd3c1abd",
    ))
    .unwrap();
    let b = EphemeralKey::from_secret_be(&hex(
        "55188b3d 32f6bb9a 900afcfb eed4e72a 59cb9ac2 f19d7cfb 6b4fdd49 f47fc5fd",
    ))
    .unwrap();
    let (ax, ay) = a.public();
    let (bx, by) = b.public();
    assert_eq!(
        ax,
        le("20b003d2 f297be2c 5e2c83a7 e9f9a5b9 eff49111 acf4fddb cc030148 0e359de6")
    );
    assert_eq!(
        ay,
        le("dc809c49 652aeb6d 63329abf 5a52155c 766345c2 8fed3024 741c8ed0 1589d28b")
    );
    assert_eq!(
        bx,
        le("1ea1f0f0 1faf1d96 09592284 f19e4c00 47b58afd 8615a69f 559077b2 2faaa190")
    );
    assert_eq!(
        by,
        le("4c55f33e 429dad37 7356703a 9ab85160 472d1130 e28e3676 5f89aff9 15b1214a")
    );
    let dh = le::<32>("ec0234a3 57c8ad05 341010a6 0a397d9b 99796b13 b4f866f1 868d34f3 73bfa698");
    assert_eq!(a.dh(&bx, &by).unwrap(), dh);
    assert_eq!(b.dh(&ax, &ay).unwrap(), dh);
}

#[test]
fn p256_rejects_invalid_curve_points() {
    let a = EphemeralKey::from_secret_be(&hex(
        "3f49f6d4 a3c55f38 74c9b3e3 d2103f50 4aff607b eb40b799 5899b8a6 cd3c1abd",
    ))
    .unwrap();
    let (x, mut y) = a.public();
    y[0] ^= 1; // off the curve
    assert!(a.dh(&x, &y).is_err());
    assert!(
        a.dh(&[0; 32], &[0; 32]).is_err(),
        "the point at infinity / origin is not a valid public key"
    );
    assert!(a.dh(&[0xFF; 32], &[0xFF; 32]).is_err());
}

#[test]
fn f4_spec_sample_data() {
    // Appendix D.2.
    let u = le("20b003d2 f297be2c 5e2c83a7 e9f9a5b9 eff49111 acf4fddb cc030148 0e359de6");
    let v = le("55188b3d 32f6bb9a 900afcfb eed4e72a 59cb9ac2 f19d7cfb 6b4fdd49 f47fc5fd");
    let x = le("d5cb8454 d177733e ffffb2ec 712baeab");
    assert_eq!(f4(&u, &v, &x, 0), le("f2c916f1 07a9bd1c f1eda1be a974872d"));
}

#[test]
fn f5_spec_sample_data() {
    // Appendix D.3.
    let w = le("ec0234a3 57c8ad05 341010a6 0a397d9b 99796b13 b4f866f1 868d34f3 73bfa698");
    let n1 = le("d5cb8454 d177733e ffffb2ec 712baeab");
    let n2 = le("a6e8e7cc 25a75f6e 216583f7 ff3dc4cf");
    let a1 = le("00 56123737bfce");
    let a2 = le("00 a713702dcfc1");
    let (mac_key, ltk) = f5(&w, &n1, &n2, &a1, &a2);
    assert_eq!(mac_key, le("2965f176 a1084a02 fd3f6a20 ce636e20"));
    assert_eq!(ltk, le("69867911 69d7cd23 980522b5 94750a38"));
}

#[test]
fn f6_spec_sample_data() {
    // Appendix D.4.
    let w = le("2965f176 a1084a02 fd3f6a20 ce636e20");
    let n1 = le("d5cb8454 d177733e ffffb2ec 712baeab");
    let n2 = le("a6e8e7cc 25a75f6e 216583f7 ff3dc4cf");
    let r = le("12a3343b b453bb54 08da42d2 0c2d0fc8");
    let io = le("010102");
    let a1 = le("00 56123737bfce");
    let a2 = le("00 a713702dcfc1");
    assert_eq!(
        f6(&w, &n1, &n2, &r, &io, &a1, &a2),
        le("e3c47398 9cd0e8c5 d26c0b09 da958f61")
    );
}

#[test]
fn g2_spec_sample_data() {
    // Appendix D.5.
    let u = le("20b003d2 f297be2c 5e2c83a7 e9f9a5b9 eff49111 acf4fddb cc030148 0e359de6");
    let v = le("55188b3d 32f6bb9a 900afcfb eed4e72a 59cb9ac2 f19d7cfb 6b4fdd49 f47fc5fd");
    let x = le("d5cb8454 d177733e ffffb2ec 712baeab");
    let y = le("a6e8e7cc 25a75f6e 216583f7 ff3dc4cf");
    assert_eq!(g2(&u, &v, &x, &y), 0x2f9ed5ba % 1_000_000);
}
