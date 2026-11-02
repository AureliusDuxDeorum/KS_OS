// SPDX-License-Identifier: Apache-2.0
//! The TPM client against a real TPM 2.0 implementation (`swtpm`, IBM's reference software TPM).
//! Skipped when `swtpm` is not installed.
use kstpm::{Error, PcrSelection, SealedBlob, Tpm, Transport};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct Swtpm {
    child: Child,
    sock: PathBuf,
}

impl Swtpm {
    fn start(dir: &Path) -> Option<Self> {
        let sock = dir.join("data.sock");
        let _ = std::fs::remove_file(&sock);
        let child = Command::new("swtpm")
            .args(["socket", "--tpm2", "--tpmstate"])
            .arg(format!("dir={}", dir.display()))
            .arg("--server")
            .arg(format!("type=unixio,path={}", sock.display()))
            .arg("--ctrl")
            .arg(format!(
                "type=unixio,path={}",
                dir.join("ctrl.sock").display()
            ))
            .args(["--flags", "not-need-init,startup-clear"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        for _ in 0..100 {
            if sock.exists() {
                return Some(Self { child, sock });
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }

    fn connect(&self) -> Tpm<Link> {
        let s = UnixStream::connect(&self.sock).expect("connect to swtpm");
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        Tpm::new(Link(s))
    }
}

impl Drop for Swtpm {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Link(UnixStream);

impl Transport for Link {
    fn transmit(&mut self, command: &[u8], response: &mut [u8]) -> Result<usize, Error> {
        self.0.write_all(command).map_err(|_| Error::Transport)?;
        self.0
            .read_exact(&mut response[..10])
            .map_err(|_| Error::Transport)?;
        let size = u32::from_be_bytes(response[2..6].try_into().unwrap()) as usize;
        if size < 10 || size > response.len() {
            return Err(Error::Malformed);
        }
        self.0
            .read_exact(&mut response[10..size])
            .map_err(|_| Error::Transport)?;
        Ok(size)
    }
}

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("kstpm-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn have_swtpm() -> bool {
    Command::new("swtpm").arg("--version").output().is_ok()
}

/// TPM2_PCR_Extend of a SHA-256 digest into one PCR (password session with an empty auth).
fn extend(tpm: &mut Tpm<Link>, pcr: u32, digest: [u8; 32]) {
    let mut c = vec![0x80, 0x02, 0, 0, 0, 0, 0, 0, 0x01, 0x82];
    c.extend_from_slice(&pcr.to_be_bytes());
    c.extend_from_slice(&[0, 0, 0, 9, 0x40, 0, 0, 9, 0, 0, 0, 0, 0]); // auth area: RS_PW, empty nonce, attrs 0, empty hmac
    c.extend_from_slice(&[0, 0, 0, 1, 0, 0x0B]);
    c.extend_from_slice(&digest);
    let n = (c.len() as u32).to_be_bytes();
    c[2..6].copy_from_slice(&n);
    let mut r = vec![0u8; 512];
    let len = tpm.transport().transmit(&c, &mut r).unwrap();
    assert_eq!(
        u32::from_be_bytes(r[6..10].try_into().unwrap()),
        0,
        "PCR_Extend failed ({len} bytes)"
    );
}

#[test]
fn random_numbers_and_pcr_reads() {
    if !have_swtpm() {
        eprintln!("skip: swtpm not installed");
        return;
    }
    let dir = tmpdir("rand");
    let sw = Swtpm::start(&dir).expect("start swtpm");
    let mut tpm = sw.connect();
    tpm.startup()
        .expect("startup tolerates an already started TPM");
    let a = tpm.random(100).unwrap();
    let b = tpm.random(100).unwrap();
    assert_eq!((a.len(), b.len()), (100, 100));
    assert_ne!(a, b, "two draws differ");
    assert!(a.iter().any(|&x| x != 0));
    let pcr7 = tpm.pcr_read(7).unwrap();
    assert_eq!(
        pcr7, [0u8; 32],
        "a fresh software TPM has PCR 7 at its initial value"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn seal_unseal_roundtrip_and_pcr_binding() {
    if !have_swtpm() {
        eprintln!("skip: swtpm not installed");
        return;
    }
    let dir = tmpdir("seal");
    let sw = Swtpm::start(&dir).expect("start swtpm");
    let mut tpm = sw.connect();
    let secret: Vec<u8> = (0..32u8)
        .map(|i| i.wrapping_mul(7).wrapping_add(1))
        .collect();
    let blob = tpm
        .seal(&secret, PcrSelection::pcr(7), b"seed")
        .expect("seal");
    assert!(
        !blob.private.windows(secret.len()).any(|w| w == &secret[..]),
        "the secret is not stored in the clear"
    );
    assert_eq!(tpm.unseal(&blob, b"seed2").expect("unseal"), secret);

    // The blob survives serialisation.
    let again = SealedBlob::from_bytes(&blob.to_bytes()).expect("parse");
    assert_eq!(again, blob);
    assert_eq!(tpm.unseal(&again, b"x").unwrap(), secret);

    // A tampered blob is refused by the TPM.
    let mut bad = blob.clone();
    let last = bad.private.len() - 1;
    bad.private[last] ^= 1;
    assert!(matches!(tpm.unseal(&bad, b"x"), Err(Error::Tpm(_))));

    // Changing the platform state (PCR 7 is extended, e.g. Secure Boot configuration changed) locks the secret.
    extend(&mut tpm, 7, [0xAB; 32]);
    assert!(
        matches!(tpm.unseal(&blob, b"x"), Err(Error::Tpm(_))),
        "unsealing must fail after PCR 7 changed"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn sealed_data_survives_a_tpm_restart() {
    if !have_swtpm() {
        eprintln!("skip: swtpm not installed");
        return;
    }
    let dir = tmpdir("restart");
    let secret = vec![0x5A; 32];
    let blob = {
        let sw = Swtpm::start(&dir).expect("start swtpm");
        sw.connect()
            .seal(&secret, PcrSelection::pcr(7), b"s")
            .expect("seal")
    };
    let sw = Swtpm::start(&dir).expect("restart swtpm with the same state");
    assert_eq!(
        sw.connect()
            .unseal(&blob, b"t")
            .expect("unseal after reboot"),
        secret
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn argument_and_response_validation() {
    struct Garbage(Vec<u8>);
    impl Transport for Garbage {
        fn transmit(&mut self, _c: &[u8], r: &mut [u8]) -> Result<usize, Error> {
            r[..self.0.len()].copy_from_slice(&self.0);
            Ok(self.0.len())
        }
    }
    let mut t = Tpm::new(Garbage(vec![0x80, 1, 0, 0, 0, 12, 0, 0, 0, 0]));
    assert_eq!(
        t.random(8),
        Err(Error::Malformed),
        "size field disagrees with the length"
    );
    let mut t = Tpm::new(Garbage(vec![0x80, 1, 0, 0, 0, 10, 0, 0, 1, 0x9D]));
    assert_eq!(t.random(8), Err(Error::Tpm(0x19D)));
    let mut t = Tpm::new(Garbage(vec![0x80, 1, 0, 0, 0, 12, 0, 0, 0, 0, 0, 0]));
    assert_eq!(
        t.random(8),
        Err(Error::Malformed),
        "no random bytes returned"
    );
    let mut t = Tpm::new(Garbage(vec![]));
    assert_eq!(t.seal(&[], PcrSelection::pcr(7), b""), Err(Error::Argument));
    assert_eq!(
        t.seal(&[0; 65], PcrSelection::pcr(7), b""),
        Err(Error::Argument)
    );
    assert!(SealedBlob::from_bytes(&[0; 3]).is_none());
    assert!(SealedBlob::from_bytes(&[0, 0, 0, 7, 0xFF, 0xFF]).is_none());
}

#[test]
fn vault_records_are_bound_to_the_tpm_and_authenticated() {
    use kstpm::vault::{Vault, VaultError};
    if !have_swtpm() {
        eprintln!("skip: swtpm not installed");
        return;
    }
    let dir = tmpdir("vault");
    let sw = Swtpm::start(&dir).expect("start swtpm");
    let mut tpm = sw.connect();
    let label = b"KSOS-BT-BONDS";
    let secret_records = b"bond store bytes: ltk=00112233445566778899aabbccddeeff".to_vec();

    let v = Vault::create(&mut tpm, label).expect("create vault");
    let rec1 = v.seal_record(&mut tpm, &secret_records).unwrap();
    let rec2 = v.seal_record(&mut tpm, &secret_records).unwrap();
    assert_ne!(rec1, rec2, "a fresh nonce per record");
    assert!(
        !rec1.windows(16).any(|w| w == &secret_records[..16]),
        "no plaintext in the record"
    );

    let (v2, plain) = Vault::open(&mut tpm, label, &rec1).expect("open");
    assert_eq!(plain, secret_records);
    let rec3 = v2.seal_record(&mut tpm, b"updated").unwrap();
    assert_eq!(Vault::open(&mut tpm, label, &rec3).unwrap().1, b"updated");

    // Any flipped bit is detected (header, nonce or ciphertext), as is the wrong label.
    for i in 0..rec1.len() {
        let mut bad = rec1.clone();
        bad[i] ^= 0x01;
        assert!(
            Vault::open(&mut tpm, label, &bad).is_err(),
            "bit flip at {i} was accepted"
        );
    }
    assert_eq!(
        Vault::open(&mut tpm, b"OTHER", &rec1).err(),
        Some(VaultError::Corrupt)
    );
    assert!(
        Vault::open(&mut tpm, label, &rec1[..rec1.len() - 1]).is_err(),
        "truncated"
    );
    assert!(Vault::open(&mut tpm, label, b"").is_err());

    // After the platform state changes (PCR 7), the vault stays closed.
    extend(&mut tpm, 7, [0x11; 32]);
    assert!(matches!(
        Vault::open(&mut tpm, label, &rec1),
        Err(VaultError::Tpm(_))
    ));
    let _ = std::fs::remove_dir_all(dir);
}
