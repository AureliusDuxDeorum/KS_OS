// SPDX-License-Identifier: Apache-2.0
//! Bluetooth: controller bring-up, scanning, pairing, bonded reconnection and HID input.
//!
//! [`init`] finds a controller among the enumerated USB devices and brings it up; [`service`] is
//! then called from the UI's idle loop and advances the connection state machine in short slices
//! (scanning in 250 ms windows; connecting and pairing block for up to a few seconds).

pub mod mediatek;
pub mod transport;

use crate::boot::{BootInfo, Module};
use crate::efi::{self, Efi};
use crate::input;
use crate::rng::{self, HwRng};
use crate::sync::IrqMutex;
use crate::time;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use ksbt::bond::{Bond, BondStore};
use ksbt::gatt;
use ksbt::hci::{self, AdvReport};
use ksbt::hogp::HogpDevice;
use ksbt::host::Host;
use ksbt::policy::{Candidate, Policy};
use ksbt::smp::{self, io, PairingConfig, PairingUi};
use ksbt::Address;
use spin::Mutex;
use transport::UsbTransport;

const SCAN_WINDOW_MS: u64 = 250;
const SCAN_TOTAL_MS: u64 = 8000;
const RESCAN_GAP_MS: u64 = 20_000;
/// How long pairing stays open when no other input device exists (bootstrap) or after `B` (S-01).
pub const BOOTSTRAP_WINDOW_MS: u64 = 120_000;
pub const USER_WINDOW_MS: u64 = 60_000;
const BONDS_NAME: [u16; 14] = efi::utf16("KSOS-BT-BONDS");

/// Pairing with a *new* device is allowed only while this deadline (ms since boot) lies ahead.
/// Without it, anyone in radio range could pair a fake keyboard (ADR-0030 S-01).
static PAIR_UNTIL_MS: AtomicU64 = AtomicU64::new(0);

/// Owner override (key `J`): accept Just Works from keyboards during the current window.
static JUST_WORKS_OK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Allows (or forbids) unauthenticated pairing of keyboards for the current pairing window.
pub fn allow_just_works(allow: bool) {
    JUST_WORKS_OK.store(allow, Ordering::Relaxed);
    kprintln!(
        "bt: Just Works for keyboards {}",
        if allow {
            "ALLOWED by the user for this pairing window (no MITM protection)"
        } else {
            "refused"
        }
    );
}

fn now_ms() -> u64 {
    time::now_ns() / 1_000_000
}

/// Opens (or extends) the window in which unknown HID devices may be paired.
pub fn open_pairing_window(ms: u64) {
    PAIR_UNTIL_MS.store(now_ms() + ms, Ordering::Relaxed);
    // Scan again right away so the user does not wait for the next round.
    if let Some(m) = MANAGER.lock().as_mut() {
        m.next_scan_at = 0;
    }
    kprintln!("bt: pairing window open for {} s", ms / 1000);
}

pub fn pairing_window_open() -> bool {
    now_ms() < PAIR_UNTIL_MS.load(Ordering::Relaxed)
}

/// Whether a Bluetooth controller is up (so the pairing key is meaningful).
pub fn available() -> bool {
    MANAGER.lock().is_some()
}

struct Connected {
    handle: u16,
    address: Address,
    dev: HogpDevice,
    tracker: input::Tracker,
}

struct Manager {
    host: Host<UsbTransport>,
    bonds: BondStore,
    efi: Option<Efi>,
    connected: Vec<Connected>,
    candidates: Vec<AdvReport>,
    scan_started: Option<u64>,
    next_scan_at: u64,
    local: Address,
    policy: Policy,
}

// SAFETY: the manager is only ever used from the main (UI) context; the raw pointer inside `Efi`
// points at firmware tables valid for the kernel's lifetime.
unsafe impl Send for Manager {}

static MANAGER: Mutex<Option<Manager>> = Mutex::new(None);
static STATUS: IrqMutex<(String, bool)> = IrqMutex::new((String::new(), false));

fn set_status(s: impl Into<String>) {
    let s = s.into();
    kprintln!("bt: {s}");
    *STATUS.lock() = (s, true);
}

/// Like [`set_status`] but never written to the serial log or log ring.
fn set_status_private(s: String) {
    *STATUS.lock() = (s, true);
}

/// Latest status line if it changed since the last call.
pub fn take_status() -> Option<String> {
    let mut g = STATUS.lock();
    if g.1 {
        g.1 = false;
        Some(g.0.clone())
    } else {
        None
    }
}

struct KernelUi;

impl PairingUi for KernelUi {
    fn display_passkey(&mut self, passkey: u32) {
        let msg = alloc::format!("Pairing: type {passkey:06} on the keyboard, then press Enter");
        // The passkey goes to the screen only, never to the serial log or log ring (S-06).
        kprintln!("bt: pairing passkey displayed on screen");
        set_status_private(msg.clone());
        crate::fb::show_notice(&msg);
    }
    fn confirm_numeric(&mut self, _value: u32) -> bool {
        false // never offered: our IO capability is DisplayOnly
    }
    fn enter_passkey(&mut self) -> Option<u32> {
        None
    }
}

fn load_bonds(efi: Option<&Efi>) -> BondStore {
    let Some(efi) = efi else {
        return BondStore::default();
    };
    let mut buf = alloc::vec![0u8; 2048];
    match efi.read_variable(&BONDS_NAME, &efi::KS_VENDOR, &mut buf) {
        Ok(n) => {
            let store = BondStore::from_bytes(&buf[..n]);
            kprintln!(
                "bt: loaded {} bond(s) from firmware storage",
                store.bonds.len()
            );
            store
        }
        Err(_) => BondStore::default(),
    }
}

fn save_bonds(m: &Manager) {
    if let Some(efi) = &m.efi {
        match efi.write_variable(&BONDS_NAME, &efi::KS_VENDOR, &m.bonds.to_bytes()) {
            Ok(()) => kprintln!("bt: bonds saved ({} entr(ies))", m.bonds.bonds.len()),
            Err(s) => kprintln!("bt: could not save bonds (EFI status {s:#x}); you will need to pair again after a reboot"),
        }
    }
}

/// Brings up the first usable Bluetooth controller, if any.
pub fn init(boot: &BootInfo, efi: Option<&Efi>) {
    let controllers = transport::controllers();
    if controllers.is_empty() {
        kprintln!("bt: no Bluetooth controller found on USB");
        return;
    }
    let modules: Vec<Module> = boot.modules().collect();
    kprintln!(
        "bt: {} controller(s), {} firmware module(s)",
        controllers.len(),
        modules.len()
    );
    if !rng::available() {
        set_status("Bluetooth disabled: this CPU has no hardware random number generator");
        return;
    }
    for d in controllers {
        let (vid, pid) = (d.descriptor.vendor, d.descriptor.product);
        kprintln!("bt: trying {vid:04x}:{pid:04x} on slot {}", d.dev.slot);
        let mut t = match UsbTransport::open(&d) {
            Ok(t) => t,
            Err(e) => {
                kprintln!("bt: cannot open transport: {e:?}");
                continue;
            }
        };
        match (vid, pid) {
            (0x0e8d, 0x0608) | (0x0e8d, 0x7961) | (0x0489, 0xe0d8) | (0x13d3, 0x3563) => {
                if let Err(e) = mediatek::setup(&mut t, &modules) {
                    set_status(match e {
                        mediatek::MtkError::FirmwareMissing(ref n) => alloc::format!(
                            "Bluetooth needs firmware {n} (build the image with --firmware-dir)"
                        ),
                        _ => {
                            alloc::format!("MediaTek bring-up failed: {e:?} (press L for the log)")
                        }
                    });
                    continue;
                }
            }
            _ => {
                kprintln!("bt: no firmware loader for {vid:04x}:{pid:04x} yet; skipping");
                continue;
            }
        }
        let mut host = Host::new(t);
        match host.init() {
            Ok(info) => {
                kprintln!(
                    "bt: controller ready: HCI {} rev {:#06x}, LMP {} subver {:#06x}, manufacturer {:#06x}, address {:02x?}, ACL {} x {}",
                    info.version.hci_version,
                    info.version.hci_revision,
                    info.version.lmp_version,
                    info.version.lmp_subversion,
                    info.version.manufacturer,
                    info.address.bytes,
                    info.acl_len,
                    info.acl_buffers
                );
                let m = Manager {
                    host,
                    bonds: load_bonds(efi),
                    efi: efi.copied(),
                    connected: Vec::new(),
                    candidates: Vec::new(),
                    scan_started: None,
                    next_scan_at: time::now_ns() / 1_000_000 + 500,
                    local: info.address,
                    policy: Policy::new(),
                };
                *MANAGER.lock() = Some(m);
                set_status("Bluetooth ready");
                return;
            }
            Err(e) => set_status(alloc::format!(
                "Bluetooth controller init failed: {e:?} (press L for the log)"
            )),
        }
    }
}

/// Number of connected Bluetooth input devices.
pub fn connected_count() -> usize {
    MANAGER.lock().as_ref().map_or(0, |m| m.connected.len())
}

/// Advances the Bluetooth state machine by one short slice. Call from the idle loop.
pub fn service() {
    let mut g = MANAGER.lock();
    let Some(m) = g.as_mut() else { return };
    m.slice();
}

impl Manager {
    fn now(&mut self) -> u64 {
        time::now_ns() / 1_000_000
    }

    fn slice(&mut self) {
        // Drain whatever the controller has sent.
        for _ in 0..16 {
            if !self.host.pump(0) {
                break;
            }
        }
        while let Some((handle, att_handle, value)) = self.host.next_notification() {
            if let Some(c) = self.connected.iter_mut().find(|c| c.handle == handle) {
                if let Some(input) = c.dev.decode(att_handle, &value) {
                    c.tracker
                        .publish(&input, c.dev.is_keyboard(), c.dev.is_pointer());
                }
            }
        }
        let host = &self.host;
        let before = self.connected.len();
        self.connected.retain(|c| host.is_connected(c.handle));
        if self.connected.len() < before {
            set_status("Bluetooth device disconnected; scanning again");
            self.next_scan_at = 0;
        }

        let now = self.now();
        match self.scan_started {
            Some(start) if now - start < SCAN_TOTAL_MS => {
                let mut found = Vec::new();
                let _ = self.host.scan(SCAN_WINDOW_MS, |r| found.push(r.clone()));
                for r in found {
                    if !self.candidates.iter().any(|c| c.address == r.address) {
                        self.candidates.push(r);
                    }
                }
            }
            Some(_) => {
                self.scan_started = None;
                self.connect_candidates();
                let now = self.now();
                self.next_scan_at = now + RESCAN_GAP_MS;
            }
            None if now >= self.next_scan_at => {
                self.candidates.clear();
                self.scan_started = Some(now);
                set_status("Scanning for Bluetooth keyboards and mice (put yours in pairing mode)");
            }
            None => {}
        }
    }

    fn connect_candidates(&mut self) {
        let candidates = core::mem::take(&mut self.candidates);
        let now = self.now();
        let window = pairing_window_open();
        if !window {
            JUST_WORKS_OK.store(false, Ordering::Relaxed); // the override never outlives its window
        }
        let info: Vec<(Candidate, hci::AdvInfo)> = candidates
            .iter()
            .map(|r| {
                let adv = hci::parse_adv_data(&r.data);
                let c = Candidate {
                    address: r.address,
                    bonded: self.bonds.find_for_advertiser(&r.address).is_some(),
                    pairable_hid: adv.is_hid() && adv.discoverable(),
                    already_connected: self.connected.iter().any(|c| c.address == r.address),
                };
                (c, adv)
            })
            .collect();
        let plain: Vec<Candidate> = info.iter().map(|(c, _)| *c).collect();
        for i in self.policy.plan(now, window, &plain) {
            let r = &candidates[i];
            let adv = &info[i].1;
            let bonded = self
                .bonds
                .find_for_advertiser(&r.address)
                .map(|b| (b.address, b.ltk));
            let label = if adv.name.is_some() {
                adv.name_lossy()
            } else {
                alloc::format!("{:02x?}", r.address.bytes)
            };
            let was_bonded = bonded.is_some();
            set_status(alloc::format!("Connecting to {label}..."));
            match self.bring_up(r, bonded.map(|(_, ltk)| ltk), adv) {
                Ok(()) => {
                    set_status(alloc::format!("Connected: {label}"));
                    if !was_bonded {
                        // Leave room for a second device (keyboard + mouse), then close.
                        PAIR_UNTIL_MS.store(self.now() + USER_WINDOW_MS, Ordering::Relaxed);
                    }
                }
                Err(e) => {
                    let hint = if e.contains("only offers Just Works") {
                        " - press J to allow it for this pairing window (not MITM-protected)"
                    } else {
                        ""
                    };
                    set_status(alloc::format!("{label}: {e}{hint}"));
                    let t = self.now();
                    self.policy.failed(r.address, t);
                }
            }
        }
        if self.connected.is_empty() {
            if window {
                set_status("No Bluetooth input device connected (pairing is open)");
            } else {
                set_status("No Bluetooth input device connected (press B to allow pairing)");
            }
        }
    }

    fn bring_up(
        &mut self,
        r: &AdvReport,
        stored_ltk: Option<[u8; 16]>,
        adv: &hci::AdvInfo,
    ) -> Result<(), String> {
        let handle = self
            .host
            .connect(r.address, 4000)
            .map_err(|e| alloc::format!("connect failed ({e:?})"))?;
        let result = (|| -> Result<Connected, String> {
            let mut identity = r.address;
            match stored_ltk {
                Some(ltk) => {
                    self.host.start_encryption(handle, ltk).map_err(|e| alloc::format!("stored key rejected ({e:?}); put the device in pairing mode to pair again"))?;
                }
                None => {
                    let cfg = PairingConfig {
                        io_capability: io::DISPLAY_ONLY,
                        require_mitm: ksbt::policy::requires_mitm(adv.appearance)
                            && !JUST_WORKS_OK.load(Ordering::Relaxed),
                        bonding: true,
                        local: self.local,
                        peer: r.address,
                    };
                    let mut keys = {
                        let mut link = self.host.smp(handle);
                        smp::pair(&mut link, &mut KernelUi, &mut HwRng, &cfg).map_err(
                            |e| match e {
                                smp::PairError::Aborted(_, why) => {
                                    alloc::format!("pairing refused: {why}")
                                }
                                other => alloc::format!("pairing failed ({other:?})"),
                            },
                        )?
                    };
                    self.host
                        .start_encryption(handle, keys.ltk)
                        .map_err(|e| alloc::format!("encryption failed ({e:?})"))?;
                    smp::receive_identity(&mut self.host.smp(handle), &mut keys, 300);
                    if let Some(id) = keys.peer_identity {
                        identity = id;
                    }
                    let kind = u8::from(adv.appearance.is_some_and(|a| a == 0x03C2)) + 1; // 1 keyboard, 2 mouse
                    self.bonds.upsert(Bond {
                        address: identity,
                        ltk: keys.ltk,
                        authenticated: keys.authenticated,
                        irk: keys.peer_irk,
                        kind,
                    });
                    save_bonds(self);
                    if !keys.authenticated {
                        kprintln!("bt: paired with Just Works (not protected against an active man-in-the-middle)");
                    }
                }
            }
            let mtu = gatt::exchange_mtu(&mut self.host.att(handle), 247);
            let dev = HogpDevice::setup(&mut self.host.att(handle), mtu)
                .map_err(|e| alloc::format!("HID service setup failed ({e:?})"))?;
            kprintln!(
                "bt: HID over GATT ready ({} input report(s), keyboard={}, pointer={})",
                dev.reports.len(),
                dev.is_keyboard(),
                dev.is_pointer()
            );
            Ok(Connected {
                handle,
                address: r.address,
                dev,
                tracker: input::Tracker::default(),
            })
        })();
        match result {
            Ok(c) => {
                self.connected.push(c);
                Ok(())
            }
            Err(e) => {
                self.host.disconnect(handle);
                Err(e)
            }
        }
    }
}
