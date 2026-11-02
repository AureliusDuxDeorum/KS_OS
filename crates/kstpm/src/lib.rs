// SPDX-License-Identifier: Apache-2.0
//! A small TPM 2.0 client (TCG TPM 2.0 Library spec, part 3 commands, part 2 structures).
//!
//! It does exactly what the OS needs: random numbers, reading a PCR, and **sealing** a secret to the
//! platform state (PCR 7 = Secure Boot configuration) with a policy session, so the secret can only be
//! unsealed when the platform is in the same state. Everything is written against [`Transport`], so it runs
//! unchanged against `swtpm` in host tests and against the hardware (CRB/TIS) in the kernel.
//!
//! Sessions used are unsalted and unbound policy sessions with empty authorisation values; the secret travels
//! unencrypted on the TPM bus, which is acceptable for firmware TPMs (PTT/fTPM) and noted as a limit for
//! discrete TPMs (bus sniffing needs physical access).
#![no_std]
#![forbid(unsafe_code)]
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]
#![cfg_attr(
    test,
    allow(
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic
    )
)]

extern crate alloc;

pub mod harness;
pub mod vault;
pub mod wire;

use alloc::vec::Vec;
use ksparse::bytes::{array_at, slice_at};
use wire::Writer;
use zeroize::Zeroize;

// ---- constants (TCG TPM 2.0 Library, part 2) ----
const ST_NO_SESSIONS: u16 = 0x8001;
const ST_SESSIONS: u16 = 0x8002;
const CC_STARTUP: u32 = 0x144;
const CC_CREATE_PRIMARY: u32 = 0x131;
const CC_CREATE: u32 = 0x153;
const CC_LOAD: u32 = 0x157;
const CC_UNSEAL: u32 = 0x15E;
const CC_FLUSH_CONTEXT: u32 = 0x165;
const CC_GET_RANDOM: u32 = 0x17B;
const CC_START_AUTH_SESSION: u32 = 0x176;
const CC_POLICY_PCR: u32 = 0x17F;
const CC_POLICY_GET_DIGEST: u32 = 0x189;
const CC_PCR_READ: u32 = 0x17E;
const RH_OWNER: u32 = 0x4000_0001;
const RH_NULL: u32 = 0x4000_0007;
const RS_PW: u32 = 0x4000_0009;
const ALG_SHA256: u16 = 0x000B;
const ALG_ECC: u16 = 0x0023;
const ALG_KEYEDHASH: u16 = 0x0008;
const ALG_NULL: u16 = 0x0010;
const ALG_AES: u16 = 0x0006;
const ALG_CFB: u16 = 0x0043;
const ECC_NIST_P256: u16 = 0x0003;
const SE_POLICY: u8 = 1;
const SE_TRIAL: u8 = 3;
const RC_INITIALIZE: u32 = 0x100;
/// Largest secret that can be sealed (the sealed-data limit is 128 bytes for a SHA-256 object).
pub const MAX_SECRET: usize = 64;

/// A channel to a TPM: send one command, receive its response.
pub trait Transport {
    /// Sends `command` and writes the response into `response`, returning its length.
    fn transmit(&mut self, command: &[u8], response: &mut [u8]) -> Result<usize, Error>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The transport failed (timeout, bus error, no device).
    Transport,
    /// The TPM answered with this non-zero response code.
    Tpm(u32),
    /// The response was malformed or too short.
    Malformed,
    /// An argument was out of range (secret too large, too many bytes requested).
    Argument,
}

/// A PCR selection: SHA-256 bank, up to 24 PCRs as a bitmap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PcrSelection(pub u32);

impl PcrSelection {
    pub const fn pcr(n: u8) -> Self {
        Self(1u32.wrapping_shl(n as u32))
    }
}

/// The result of sealing: opaque blobs to store, plus nothing secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedBlob {
    pub private: Vec<u8>,
    pub public: Vec<u8>,
    pub pcrs: PcrSelection,
}

impl SealedBlob {
    /// Serialises as `pcrs(4) | len(2) private | len(2) public`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(self.pcrs.0);
        w.tpm2b(&self.private);
        w.tpm2b(&self.public);
        w.finish()
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        let pcrs = PcrSelection(u32_be(b, 0)?);
        let pl = usize::from(u16_at_be(b, 4)?);
        let private = slice_at(b, 6, pl)?.to_vec();
        let off = 6usize.checked_add(pl)?;
        let ql = usize::from(u16_at_be(b, off)?);
        let public = slice_at(b, off.checked_add(2)?, ql)?.to_vec();
        (off.checked_add(2)?.checked_add(ql)? == b.len()).then_some(Self {
            private,
            public,
            pcrs,
        })
    }
}

pub struct Tpm<T: Transport> {
    t: T,
    buf: Vec<u8>,
}

/// Parsed response: the bytes after the 10-byte header, split into handle area and the rest.
struct Response {
    body: Vec<u8>,
}

impl<T: Transport> Tpm<T> {
    pub fn new(t: T) -> Self {
        Self {
            t,
            buf: alloc::vec![0u8; 4096],
        }
    }

    pub fn transport(&mut self) -> &mut T {
        &mut self.t
    }

    fn run(&mut self, command: &[u8]) -> Result<Response, Error> {
        let n = self.t.transmit(command, &mut self.buf)?;
        let r = self.buf.get(..n).ok_or(Error::Malformed)?;
        let size = u32_be(r, 2).ok_or(Error::Malformed)? as usize;
        let rc = u32_be(r, 6).ok_or(Error::Malformed)?;
        if size != n {
            return Err(Error::Malformed);
        }
        if rc != 0 {
            return Err(Error::Tpm(rc));
        }
        Ok(Response {
            body: r.get(10..).ok_or(Error::Malformed)?.to_vec(),
        })
    }

    fn command(tag: u16, cc: u32, handles: &[u32], auth: Option<&[u8]>, params: &[u8]) -> Vec<u8> {
        let mut w = Writer::new();
        w.u16(tag);
        w.u32(0); // size, patched below
        w.u32(cc);
        for h in handles {
            w.u32(*h);
        }
        if let Some(a) = auth {
            w.u32(u32::try_from(a.len()).unwrap_or(0));
            w.bytes(a);
        }
        w.bytes(params);
        w.finish_with_size()
    }

    /// Password session with an empty authorisation value (used for the owner hierarchy and parent keys).
    fn pw_auth() -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(RS_PW);
        w.u16(0); // nonceCaller
        w.u8(1); // continueSession
        w.u16(0); // hmac
        w.finish()
    }

    /// Policy-session authorisation (no HMAC needed: the policy is satisfied by PolicyPCR).
    fn policy_auth(session: u32) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(session);
        w.u16(0);
        w.u8(0); // session is not continued: the TPM flushes it after the command
        w.u16(0);
        w.finish()
    }

    /// TPM2_Startup(CLEAR). Harmless if firmware already started the TPM.
    pub fn startup(&mut self) -> Result<(), Error> {
        let c = Self::command(ST_NO_SESSIONS, CC_STARTUP, &[], None, &0u16.to_be_bytes());
        match self.run(&c) {
            Ok(_) | Err(Error::Tpm(RC_INITIALIZE)) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// `n` bytes from the TPM's random number generator (at most 48 per call are returned; loops).
    pub fn random(&mut self, n: usize) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        while out.len() < n {
            let want = n.saturating_sub(out.len()).min(32);
            let c = Self::command(
                ST_NO_SESSIONS,
                CC_GET_RANDOM,
                &[],
                None,
                &(want as u16).to_be_bytes(),
            );
            let r = self.run(&c)?;
            let len = usize::from(u16_at_be(&r.body, 0).ok_or(Error::Malformed)?);
            let got = slice_at(&r.body, 2, len).ok_or(Error::Malformed)?;
            if got.is_empty() {
                return Err(Error::Malformed);
            }
            out.extend_from_slice(got);
        }
        out.truncate(n);
        Ok(out)
    }

    /// Current SHA-256 value of one PCR.
    pub fn pcr_read(&mut self, pcr: u8) -> Result<[u8; 32], Error> {
        let mut p = Writer::new();
        Self::pcr_selection(&mut p, PcrSelection::pcr(pcr));
        let c = Self::command(ST_NO_SESSIONS, CC_PCR_READ, &[], None, &p.finish());
        let r = self.run(&c)?;
        // updateCounter(4) | TPML_PCR_SELECTION out (4 + 2 + 1 + 3) | TPML_DIGEST (4, then TPM2B)
        let digests_at = 4usize
            .saturating_add(4)
            .saturating_add(2)
            .saturating_add(1)
            .saturating_add(3);
        let count = u32_be(&r.body, digests_at).ok_or(Error::Malformed)?;
        if count != 1 {
            return Err(Error::Malformed);
        }
        let len =
            usize::from(u16_at_be(&r.body, digests_at.saturating_add(4)).ok_or(Error::Malformed)?);
        let v = slice_at(&r.body, digests_at.saturating_add(6), len).ok_or(Error::Malformed)?;
        v.try_into().map_err(|_| Error::Malformed)
    }

    fn pcr_selection(w: &mut Writer, sel: PcrSelection) {
        w.u32(1); // count
        w.u16(ALG_SHA256);
        w.u8(3); // sizeofSelect
        let b = sel.0.to_le_bytes();
        w.bytes(&b[..3]);
    }

    fn flush(&mut self, handle: u32) {
        let c = Self::command(
            ST_NO_SESSIONS,
            CC_FLUSH_CONTEXT,
            &[],
            None,
            &handle.to_be_bytes(),
        );
        let _ = self.run(&c);
    }

    /// The deterministic storage primary key under the owner hierarchy (same template => same key).
    fn create_primary(&mut self) -> Result<u32, Error> {
        let mut p = Writer::new();
        // inSensitive: TPM2B_SENSITIVE_CREATE { userAuth TPM2B(0), data TPM2B(0) }
        p.u16(4);
        p.u16(0);
        p.u16(0);
        // inPublic: TPM2B_PUBLIC { ECC P-256 restricted decryption key, AES-128-CFB }
        let mut t = Writer::new();
        t.u16(ALG_ECC);
        t.u16(ALG_SHA256);
        t.u32(0x0003_0472); // fixedTPM|fixedParent|sensitiveDataOrigin|userWithAuth|noDA|restricted|decrypt
        t.u16(0); // authPolicy
        t.u16(ALG_AES);
        t.u16(128);
        t.u16(ALG_CFB);
        t.u16(ALG_NULL); // scheme
        t.u16(ECC_NIST_P256);
        t.u16(ALG_NULL); // kdf
        t.u16(0); // unique.x
        t.u16(0); // unique.y
        p.tpm2b(&t.finish());
        p.u16(0); // outsideInfo
        p.u32(0); // creationPCR count
        let c = Self::command(
            ST_SESSIONS,
            CC_CREATE_PRIMARY,
            &[RH_OWNER],
            Some(&Self::pw_auth()),
            &p.finish(),
        );
        let r = self.run(&c)?;
        u32_be(&r.body, 0).ok_or(Error::Malformed)
    }

    fn start_session(&mut self, kind: u8, nonce_seed: &[u8]) -> Result<u32, Error> {
        let mut p = Writer::new();
        let mut nonce = [0u8; 16];
        for (d, s) in nonce.iter_mut().zip(nonce_seed.iter().cycle()) {
            *d = *s;
        }
        p.tpm2b(&nonce);
        p.u16(0); // encryptedSalt
        p.u8(kind);
        p.u16(ALG_NULL); // symmetric
        p.u16(ALG_SHA256);
        let c = Self::command(
            ST_NO_SESSIONS,
            CC_START_AUTH_SESSION,
            &[RH_NULL, RH_NULL],
            None,
            &p.finish(),
        );
        let r = self.run(&c)?;
        u32_be(&r.body, 0).ok_or(Error::Malformed)
    }

    fn policy_pcr(&mut self, session: u32, sel: PcrSelection) -> Result<(), Error> {
        let mut p = Writer::new();
        p.u16(0); // pcrDigest: empty = use the current values
        Self::pcr_selection(&mut p, sel);
        let c = Self::command(ST_NO_SESSIONS, CC_POLICY_PCR, &[session], None, &p.finish());
        self.run(&c).map(|_| ())
    }

    /// Policy digest that "PCR selection has its current value" produces (computed by the TPM itself).
    fn current_policy(&mut self, sel: PcrSelection, nonce_seed: &[u8]) -> Result<Vec<u8>, Error> {
        let s = self.start_session(SE_TRIAL, nonce_seed)?;
        let result = (|| {
            self.policy_pcr(s, sel)?;
            let c = Self::command(ST_NO_SESSIONS, CC_POLICY_GET_DIGEST, &[s], None, &[]);
            let r = self.run(&c)?;
            // parameterSize? no sessions: body is TPM2B_DIGEST directly
            let len = usize::from(u16_at_be(&r.body, 0).ok_or(Error::Malformed)?);
            Ok(slice_at(&r.body, 2, len).ok_or(Error::Malformed)?.to_vec())
        })();
        self.flush(s);
        result
    }

    /// Seals `secret` (at most [`MAX_SECRET`] bytes) so that only the same state of `pcrs` can release it.
    /// `entropy` seeds session nonces (anything; the TPM supplies its own).
    pub fn seal(
        &mut self,
        secret: &[u8],
        pcrs: PcrSelection,
        entropy: &[u8],
    ) -> Result<SealedBlob, Error> {
        if secret.is_empty() || secret.len() > MAX_SECRET {
            return Err(Error::Argument);
        }
        let policy = self.current_policy(pcrs, entropy)?;
        let parent = self.create_primary()?;
        let result = (|| {
            let mut p = Writer::new();
            // inSensitive { userAuth(0), data(secret) }
            let mut s = Writer::new();
            s.u16(0);
            s.tpm2b(secret);
            p.tpm2b(&s.finish());
            // inPublic: keyed-hash data object, policy-only authorisation.
            let mut t = Writer::new();
            t.u16(ALG_KEYEDHASH);
            t.u16(ALG_SHA256);
            t.u32(0x0000_0012); // fixedTPM | fixedParent (no userWithAuth: only the policy opens it)
            t.tpm2b(&policy);
            t.u16(ALG_NULL); // scheme
            t.u16(0); // unique
            p.tpm2b(&t.finish());
            p.u16(0); // outsideInfo
            p.u32(0); // creationPCR
            let c = Self::command(
                ST_SESSIONS,
                CC_CREATE,
                &[parent],
                Some(&Self::pw_auth()),
                &p.finish(),
            );
            let r = self.run(&c)?;
            // paramSize(4) | outPrivate TPM2B | outPublic TPM2B | ...
            let at = 4usize;
            let pl = usize::from(u16_at_be(&r.body, at).ok_or(Error::Malformed)?);
            let private = slice_at(&r.body, at.saturating_add(2), pl)
                .ok_or(Error::Malformed)?
                .to_vec();
            let at2 = at.saturating_add(2).saturating_add(pl);
            let ql = usize::from(u16_at_be(&r.body, at2).ok_or(Error::Malformed)?);
            let public = slice_at(&r.body, at2.saturating_add(2), ql)
                .ok_or(Error::Malformed)?
                .to_vec();
            Ok(SealedBlob {
                private,
                public,
                pcrs,
            })
        })();
        self.flush(parent);
        result
    }

    /// Releases a sealed secret. Fails with `Error::Tpm(..)` (policy failure) if the PCRs changed.
    pub fn unseal(&mut self, blob: &SealedBlob, entropy: &[u8]) -> Result<Vec<u8>, Error> {
        let parent = self.create_primary()?;
        let loaded = (|| {
            let mut p = Writer::new();
            p.tpm2b(&blob.private);
            p.tpm2b(&blob.public);
            let c = Self::command(
                ST_SESSIONS,
                CC_LOAD,
                &[parent],
                Some(&Self::pw_auth()),
                &p.finish(),
            );
            let r = self.run(&c)?;
            u32_be(&r.body, 0).ok_or(Error::Malformed)
        })();
        self.flush(parent);
        let object = loaded?;
        let result = (|| {
            let session = self.start_session(SE_POLICY, entropy)?;
            if let Err(e) = self.policy_pcr(session, blob.pcrs) {
                self.flush(session);
                return Err(e);
            }
            let c = Self::command(
                ST_SESSIONS,
                CC_UNSEAL,
                &[object],
                Some(&Self::policy_auth(session)),
                &[],
            );
            let r = self.run(&c);
            // A session that is not continued is flushed by the TPM on success; flush defensively on error.
            if r.is_err() {
                self.flush(session);
            }
            let r = r?;
            // paramSize(4) | TPM2B_SENSITIVE_DATA | auth response
            let len = usize::from(u16_at_be(&r.body, 4).ok_or(Error::Malformed)?);
            Ok(slice_at(&r.body, 6, len).ok_or(Error::Malformed)?.to_vec())
        })();
        self.flush(object);
        result
    }
}

fn u16_at_be(b: &[u8], off: usize) -> Option<u16> {
    array_at::<2>(b, off).map(u16::from_be_bytes)
}

fn u32_be(b: &[u8], off: usize) -> Option<u32> {
    array_at::<4>(b, off).map(u32::from_be_bytes)
}

/// Overwrites a secret buffer before it is dropped.
pub fn wipe(v: &mut Vec<u8>) {
    v.zeroize();
}
