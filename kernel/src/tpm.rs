// SPDX-License-Identifier: Apache-2.0
//! TPM 2.0 transport: the CRB (command-response buffer; Intel PTT, many fTPMs, QEMU `tpm-crb`) and the
//! TIS/FIFO (discrete TPMs, QEMU `tpm-tis`) memory-mapped interfaces described by the ACPI `TPM2` table.
//! The protocol on top lives in the `kstpm` crate. Start methods that need ACPI AML (2, 8, 11, 12) are not
//! supported because the kernel has no AML interpreter (ADR-0023).

use crate::boot::BootInfo;
use crate::mmio::Mmio;
use crate::sync::IrqMutex;
use crate::time;
use alloc::string::String;
use kstpm::{Error, Tpm, Transport};

const DEFAULT_BASE: u64 = 0xFED4_0000;
const MAP_LEN: u64 = 0x5000;

// CRB register offsets from the locality base (TCG PC Client Platform TPM Profile, CRB interface).
const CRB_LOC_STATE: usize = 0x00;
const CRB_LOC_CTRL: usize = 0x08;
const CRB_CTRL_REQ: usize = 0x40;
const CRB_CTRL_START: usize = 0x4C;
const CRB_CMD_SIZE: usize = 0x58;
const CRB_CMD_LADDR: usize = 0x5C;
const CRB_RSP_SIZE: usize = 0x64;
const CRB_RSP_ADDR: usize = 0x68;

// TIS register offsets (locality 0).
const TIS_ACCESS: usize = 0x00;
const TIS_STS: usize = 0x18;
const TIS_FIFO: usize = 0x24;

enum Kind {
    Crb {
        cmd: Mmio,
        cmd_len: usize,
        rsp: Mmio,
        rsp_len: usize,
    },
    Tis,
}

pub struct Device {
    regs: Mmio,
    kind: Kind,
}

static TPM: IrqMutex<Option<Tpm<Device>>> = IrqMutex::new(None);

fn wait(timeout_ms: u64, mut cond: impl FnMut() -> bool) -> bool {
    let end = time::now_ns().saturating_add(timeout_ms.saturating_mul(1_000_000));
    loop {
        if cond() {
            return true;
        }
        if time::now_ns() > end {
            return false;
        }
        core::hint::spin_loop();
    }
}

impl Device {
    fn crb(
        &self,
        cmd: &Mmio,
        cmd_len: usize,
        rsp: &Mmio,
        rsp_len: usize,
        command: &[u8],
        response: &mut [u8],
    ) -> Result<usize, Error> {
        let r = self.regs;
        if command.len() > cmd_len {
            return Err(Error::Argument);
        }
        // Take the locality, then wake the TPM.
        if r.r8(CRB_LOC_STATE) & 0x02 == 0 {
            r.w8(CRB_LOC_CTRL, 1);
            if !wait(2000, || r.r8(CRB_LOC_STATE) & 0x02 != 0) {
                return Err(Error::Transport);
            }
        }
        r.w32(CRB_CTRL_REQ, 1); // cmdReady
        if !wait(1000, || r.r32(CRB_CTRL_REQ) & 1 == 0) {
            return Err(Error::Transport);
        }
        for (i, b) in command.iter().enumerate() {
            cmd.w8(i, *b);
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        r.w32(CRB_CTRL_START, 1);
        if !wait(30_000, || r.r32(CRB_CTRL_START) & 1 == 0) {
            return Err(Error::Transport);
        }
        let mut header = [0u8; 10];
        for (i, b) in header.iter_mut().enumerate() {
            *b = rsp.r8(i);
        }
        let size = u32::from_be_bytes([header[2], header[3], header[4], header[5]]) as usize;
        if size < 10 || size > rsp_len || size > response.len() {
            return Err(Error::Malformed);
        }
        for (i, d) in response.iter_mut().take(size).enumerate() {
            *d = rsp.r8(i);
        }
        r.w32(CRB_CTRL_REQ, 2); // goIdle
        r.w8(CRB_LOC_CTRL, 2); // relinquish the locality
        Ok(size)
    }

    fn tis(&self, command: &[u8], response: &mut [u8]) -> Result<usize, Error> {
        let r = self.regs;
        let sts = || r.r32(TIS_STS);
        // Locality 0.
        if r.r8(TIS_ACCESS) & 0x20 == 0 {
            r.w8(TIS_ACCESS, 0x02);
            if !wait(2000, || r.r8(TIS_ACCESS) & 0x20 != 0) {
                return Err(Error::Transport);
            }
        }
        if sts() & 0x40 == 0 {
            r.w32(TIS_STS, 0x40); // commandReady
            if !wait(2000, || sts() & 0x40 != 0) {
                return Err(Error::Transport);
            }
        }
        let burst = || ((sts() >> 8) & 0xFFFF) as usize;
        let mut sent = 0;
        while sent < command.len() {
            if !wait(2000, || burst() > 0) {
                return Err(Error::Transport);
            }
            let n = burst().min(command.len().saturating_sub(sent).saturating_sub(1).max(1));
            for b in command.iter().skip(sent).take(n) {
                r.w8(TIS_FIFO, *b);
            }
            sent = sent.saturating_add(n);
            let expect = sts() & 0x08 != 0;
            if sent < command.len() && !expect {
                return Err(Error::Transport);
            }
        }
        if sts() & 0x08 != 0 {
            return Err(Error::Transport); // the TPM still expects data
        }
        r.w32(TIS_STS, 0x20); // tpmGo
        if !wait(30_000, || sts() & 0x90 == 0x90) {
            return Err(Error::Transport);
        }
        let mut got = 0usize;
        let mut total = 10usize;
        while got < total {
            if !wait(2000, || sts() & 0x10 != 0) {
                return Err(Error::Transport);
            }
            let b = r.r8(TIS_FIFO);
            if let Some(d) = response.get_mut(got) {
                *d = b;
            } else {
                return Err(Error::Malformed);
            }
            got = got.saturating_add(1);
            if got == 6 {
                total = u32::from_be_bytes([response[2], response[3], response[4], response[5]])
                    as usize;
                if total < 10 || total > response.len() {
                    return Err(Error::Malformed);
                }
            }
        }
        r.w32(TIS_STS, 0x40); // back to idle
        r.w8(TIS_ACCESS, 0x20); // release the locality
        Ok(total)
    }
}

impl Transport for Device {
    fn transmit(&mut self, command: &[u8], response: &mut [u8]) -> Result<usize, Error> {
        match &self.kind {
            Kind::Crb {
                cmd,
                cmd_len,
                rsp,
                rsp_len,
            } => {
                let (cmd, cl, rsp, rl) = (*cmd, *cmd_len, *rsp, *rsp_len);
                self.crb(&cmd, cl, &rsp, rl, command, response)
            }
            Kind::Tis => self.tis(command, response),
        }
    }
}

fn map(boot: &BootInfo, phys: u64, len: u64) -> Result<Mmio, String> {
    let page = phys & !0xFFF;
    let span = (phys - page).saturating_add(len).next_multiple_of(4096);
    let virt = crate::vmm::map_mmio(boot, page, span)
        .map_err(|e| alloc::format!("map {phys:#x}: {e:?}"))?;
    Ok(Mmio::new(virt + (phys - page), len))
}

/// Finds the TPM described by the ACPI `TPM2` table and starts talking to it.
pub fn init(boot: &BootInfo) {
    let table = match crate::acpi::find_table(boot, b"TPM2") {
        Ok(t) => t,
        Err(_) => {
            kprintln!("tpm: no TPM2 ACPI table (no TPM, or disabled in firmware)");
            return;
        }
    };
    let ctrl = ksparse::bytes::u64_at(table, 40).unwrap_or(0);
    let method = ksparse::bytes::u32_at(table, 48).unwrap_or(0);
    let result: Result<Device, String> = (|| {
        match method {
            7 => {
                // CRB: the control area sits 0x40 above the locality registers.
                let base = ctrl.checked_sub(0x40).ok_or("bad control area address")?;
                let regs = map(boot, base, MAP_LEN.min(0x1000))?;
                let cmd_addr = u64::from(regs.r32(CRB_CMD_LADDR))
                    | (u64::from(regs.r32(CRB_CMD_LADDR + 4)) << 32);
                let cmd_len = regs.r32(CRB_CMD_SIZE) as usize;
                let rsp_addr = regs.r64(CRB_RSP_ADDR);
                let rsp_len = regs.r32(CRB_RSP_SIZE) as usize;
                if cmd_addr == 0
                    || rsp_addr == 0
                    || cmd_len == 0
                    || rsp_len == 0
                    || cmd_len > 0x4000
                    || rsp_len > 0x4000
                {
                    return Err("CRB buffers are not described".into());
                }
                let cmd = map(boot, cmd_addr, cmd_len as u64)?;
                let rsp = map(boot, rsp_addr, rsp_len as u64)?;
                Ok(Device {
                    regs,
                    kind: Kind::Crb {
                        cmd,
                        cmd_len,
                        rsp,
                        rsp_len,
                    },
                })
            }
            6 => {
                let base = if ctrl != 0 { ctrl } else { DEFAULT_BASE };
                Ok(Device {
                    regs: map(boot, base, 0x1000)?,
                    kind: Kind::Tis,
                })
            }
            m => Err(alloc::format!(
                "start method {m} needs ACPI AML (unsupported)"
            )),
        }
    })();
    match result {
        Ok(dev) => {
            let mut tpm = Tpm::new(dev);
            match tpm.startup().and_then(|()| tpm.random(8)) {
                Ok(_) => {
                    kprintln!(
                        "tpm: TPM 2.0 ready ({} interface)",
                        if method == 7 { "CRB" } else { "TIS" }
                    );
                    *TPM.lock() = Some(tpm);
                }
                Err(e) => kprintln!("tpm: found a TPM but it does not answer ({e:?})"),
            }
        }
        Err(e) => kprintln!("tpm: not usable: {e}"),
    }
}

/// Whether a working TPM was found.
pub fn present() -> bool {
    TPM.lock().is_some()
}

/// Runs `f` with exclusive access to the TPM.
pub fn with<R>(f: impl FnOnce(&mut Tpm<Device>) -> R) -> Option<R> {
    let mut g = TPM.lock();
    g.as_mut().map(f)
}
