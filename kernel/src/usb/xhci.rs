//! xHCI (USB 3.x) host controller driver.
//!
//! Owns controller reset (including BIOS hand-off), the command/event/transfer rings, device
//! slots and contexts, port reset, and synchronous control/bulk/interrupt transfers. Events
//! arrive by MSI-X/MSI; every wait also polls the event ring, so progress never depends on the
//! interrupt actually being delivered.
//!
//! Locking: one `IrqMutex` guards all controller state and is *never* held while waiting.
//! Completions travel from the event handler to waiters through lock-free mailboxes.

use super::{DeviceRef, UsbError};
use crate::boot::BootInfo;
use crate::dma::Dma;
use crate::sync::IrqMutex;
use crate::{arch, irq, pci, time};
use alloc::vec::Vec;
use core::sync::atomic::{fence, AtomicU64, AtomicUsize, Ordering};
use ksparse::usb::{self as fmt, Setup};

pub const MAX_CONTROLLERS: usize = 4;
/// Highest slot id we use (keeps the completion tables small).
const SLOT_LIMIT: usize = 63;
const MAILBOX_LEN: usize = MAX_CONTROLLERS * (SLOT_LIMIT + 1) * 32;
const VALID: u64 = 1 << 63;

// Operational register offsets (from the operational base).
const USBCMD: u64 = 0x00;
const USBSTS: u64 = 0x04;
const PAGESIZE: u64 = 0x08;
const CRCR: u64 = 0x18;
const DCBAAP: u64 = 0x30;
const CONFIG: u64 = 0x38;
const PORT_BASE: u64 = 0x400;

const CMD_RUN: u32 = 1 << 0;
const CMD_RESET: u32 = 1 << 1;
const CMD_INTE: u32 = 1 << 2;
const STS_HALTED: u32 = 1 << 0;
const STS_EINT: u32 = 1 << 3;
const STS_NOT_READY: u32 = 1 << 11;

// PORTSC bits.
const PORT_CCS: u32 = 1 << 0;
const PORT_PED: u32 = 1 << 1;
const PORT_RESET: u32 = 1 << 4;
const PORT_POWER: u32 = 1 << 9;
const PORT_CHANGE_BITS: u32 = (0x7F) << 17; // CSC PEC WRC OCC PRC PLC CEC (all RW1C)
const PORT_PRESERVE: u32 = PORT_POWER | (3 << 14) | (7 << 25);
const PORT_PRC: u32 = 1 << 21;

// TRB types.
const TRB_NORMAL: u32 = 1;
const TRB_SETUP: u32 = 2;
const TRB_DATA: u32 = 3;
const TRB_STATUS: u32 = 4;
const TRB_LINK: u32 = 6;
const CMD_ENABLE_SLOT: u32 = 9;
const CMD_DISABLE_SLOT: u32 = 10;
const CMD_ADDRESS_DEVICE: u32 = 11;
const CMD_CONFIGURE_ENDPOINT: u32 = 12;
const CMD_EVALUATE_CONTEXT: u32 = 13;
const CMD_RESET_ENDPOINT: u32 = 14;
const CMD_SET_TR_DEQUEUE: u32 = 16;
const CMD_NOOP: u32 = 23;
const EVT_TRANSFER: u32 = 32;
const EVT_COMMAND: u32 = 33;
const EVT_PORT_STATUS: u32 = 34;

pub const CC_SUCCESS: u8 = 1;
pub const CC_STALL: u8 = 6;
pub const CC_SHORT_PACKET: u8 = 13;

const RING_TRBS: usize = 256;

static MAILBOX: [AtomicU64; MAILBOX_LEN] = [const { AtomicU64::new(0) }; MAILBOX_LEN];
static HANDLERS: [AtomicUsize; MAILBOX_LEN] = [const { AtomicUsize::new(0) }; MAILBOX_LEN];
static CMD_RESULT: [AtomicU64; MAX_CONTROLLERS] = [const { AtomicU64::new(0) }; MAX_CONTROLLERS];
/// Control-transfer rendezvous (one control transfer in flight per controller).
static CTRL_EXPECT: [AtomicU64; MAX_CONTROLLERS] = [const { AtomicU64::new(0) }; MAX_CONTROLLERS];
static CTRL_DATA_RESIDUAL: [AtomicU64; MAX_CONTROLLERS] =
    [const { AtomicU64::new(0) }; MAX_CONTROLLERS];
static CTRL_RESULT: [AtomicU64; MAX_CONTROLLERS] = [const { AtomicU64::new(0) }; MAX_CONTROLLERS];

fn mbox(ctrl: usize, slot: u8, dci: u8) -> usize {
    (ctrl * (SLOT_LIMIT + 1) + usize::from(slot)) * 32 + usize::from(dci)
}

/// Handler invoked from the event path when a transfer on a streaming endpoint completes.
pub type EndpointHandler =
    fn(DeviceRef, u8 /* dci */, u8 /* completion code */, u32 /* residual */);

struct Ring {
    dma: Dma,
    enq: usize,
    pcs: bool,
}

impl Ring {
    fn new(hhdm: u64, below_4g: bool) -> Option<Self> {
        Some(Self {
            dma: Dma::alloc(hhdm, RING_TRBS * 16, below_4g)?,
            enq: 0,
            pcs: true,
        })
    }

    fn write_trb(&self, index: usize, param: u64, status: u32, control: u32) {
        let off = index * 16;
        self.dma.write64(off, param);
        self.dma.write32(off + 8, status);
        fence(Ordering::Release);
        self.dma.write32(off + 12, control); // cycle bit last: hands the TRB to the controller
    }

    /// Appends a TRB (cycle bit set from the producer cycle state) and returns its physical address.
    fn push(&mut self, param: u64, status: u32, control: u32) -> u64 {
        self.write_trb(
            self.enq,
            param,
            status,
            (control & !1) | u32::from(self.pcs),
        );
        let phys = self.dma.phys + (self.enq * 16) as u64;
        self.enq += 1;
        if self.enq == RING_TRBS - 1 {
            // Link back to the start, toggling the cycle bit.
            self.write_trb(
                RING_TRBS - 1,
                self.dma.phys,
                0,
                (TRB_LINK << 10) | (1 << 1) | u32::from(self.pcs),
            );
            self.pcs = !self.pcs;
            self.enq = 0;
        }
        phys
    }

    /// Physical address where the next TRB will be written.
    fn enqueue_phys(&self) -> u64 {
        self.dma.phys + (self.enq * 16) as u64
    }
}

#[derive(Clone, Copy)]
enum Event {
    None,
    Command {
        code: u8,
        slot: u8,
    },
    Transfer {
        slot: u8,
        dci: u8,
        code: u8,
        residual: u32,
        ptr: u64,
    },
    PortChange {
        port: u8,
    },
}

pub(super) struct SlotState {
    dev_ctx: Dma,
    input_ctx: Dma,
    rings: [Option<Ring>; 32],
    pub speed: u8,
    pub root_port: u8,
    pub route: u32,
    pub depth: u8,
    pub tt: Option<(u8, u8)>,
}

pub(super) struct Controller {
    op: u64,
    rt: u64,
    db: u64,
    hhdm: u64,
    below_4g: bool,
    csz: usize,
    pub max_ports: u8,
    dcbaa: Dma,
    cmd: Ring,
    ev_ring: Dma,
    ev_deq: usize,
    ev_ccs: bool,
    _erst: Dma,
    _scratch: Vec<Dma>,
    _scratch_pages: Vec<Dma>,
    pub(super) slots: Vec<Option<SlotState>>,
    control_buf: Dma,
    control_busy: bool,
}

static CONTROLLERS: IrqMutex<Vec<Controller>> = IrqMutex::new(Vec::new());

fn mmio_read32(addr: u64) -> u32 {
    // SAFETY: `addr` lies inside the controller's uncached MMIO mapping (computed from BAR + offsets).
    unsafe { (addr as *const u32).read_volatile() }
}

fn mmio_write32(addr: u64, v: u32) {
    // SAFETY: as in `mmio_read32`.
    unsafe { (addr as *mut u32).write_volatile(v) };
}

fn mmio_write64(addr: u64, v: u64) {
    mmio_write32(addr, v as u32);
    mmio_write32(addr + 4, (v >> 32) as u32);
}

fn wait_for(timeout_ms: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = time::now_ns() + timeout_ms * 1_000_000;
    loop {
        if cond() {
            return true;
        }
        if time::now_ns() >= deadline {
            return false;
        }
        core::hint::spin_loop();
    }
}

impl Controller {
    fn port_reg(&self, port: u8) -> u64 {
        self.op + PORT_BASE + 0x10 * u64::from(port - 1)
    }

    pub(super) fn portsc(&self, port: u8) -> u32 {
        mmio_read32(self.port_reg(port))
    }

    fn portsc_write(&self, port: u8, v: u32) {
        mmio_write32(self.port_reg(port), v);
    }

    fn ring_doorbell(&self, target: u8, value: u32) {
        fence(Ordering::SeqCst);
        mmio_write32(self.db + 4 * u64::from(target), value);
    }

    /// Drains the event ring into `out`; returns how many events were stored.
    fn drain(&mut self, out: &mut [Event; 64]) -> usize {
        // Acknowledge: EINT in USBSTS and IP in IMAN (both write-1-to-clear).
        mmio_write32(self.op + USBSTS, STS_EINT);
        mmio_write32(self.rt + 0x20, mmio_read32(self.rt + 0x20) | 1);
        let mut n = 0;
        while n < out.len() {
            let off = self.ev_deq * 16;
            let control = self.ev_ring.read32(off + 12);
            if (control & 1 != 0) != self.ev_ccs {
                break;
            }
            fence(Ordering::Acquire);
            let param = u64::from(self.ev_ring.read32(off))
                | (u64::from(self.ev_ring.read32(off + 4)) << 32);
            let status = self.ev_ring.read32(off + 8);
            let code = (status >> 24) as u8;
            let slot = (control >> 24) as u8;
            out[n] = match (control >> 10) & 0x3F {
                EVT_COMMAND => Event::Command { code, slot },
                EVT_TRANSFER => Event::Transfer {
                    slot,
                    dci: ((control >> 16) & 0x1F) as u8,
                    code,
                    residual: status & 0xFF_FFFF,
                    ptr: param,
                },
                EVT_PORT_STATUS => Event::PortChange {
                    port: (param >> 24) as u8,
                },
                _ => Event::None,
            };
            n += 1;
            self.ev_deq += 1;
            if self.ev_deq == RING_TRBS {
                self.ev_deq = 0;
                self.ev_ccs = !self.ev_ccs;
            }
        }
        // Tell the controller how far we got (EHB, bit 3, write-1-to-clear).
        mmio_write64(
            self.rt + 0x38,
            (self.ev_ring.phys + (self.ev_deq * 16) as u64) | 8,
        );
        n
    }
}

/// Handles pending events for controller `ctrl` (interrupt handler and wait loops).
pub fn process_events(ctrl: usize) {
    let mut events = [Event::None; 64];
    let n = {
        let mut g = CONTROLLERS.lock();
        match g.get_mut(ctrl) {
            Some(c) => c.drain(&mut events),
            None => return,
        }
    };
    for ev in &events[..n] {
        match *ev {
            Event::Command { code, slot } => CMD_RESULT[ctrl].store(
                VALID | (u64::from(slot) << 8) | u64::from(code),
                Ordering::SeqCst,
            ),
            Event::PortChange { port } => kprintln!(
                "usb: xhci{ctrl} port {port} status changed (hot-plug is not handled yet)"
            ),
            Event::Transfer {
                slot,
                dci,
                code,
                residual,
                ptr,
            } => {
                if usize::from(slot) > SLOT_LIMIT || dci == 0 {
                    continue;
                }
                if dci == 1 {
                    // Control endpoint: rendezvous with the transfer waiting in `control`.
                    if ptr == CTRL_EXPECT[ctrl].load(Ordering::SeqCst) {
                        CTRL_RESULT[ctrl].store(
                            VALID | (u64::from(code) << 32) | u64::from(residual),
                            Ordering::SeqCst,
                        );
                    } else if code == CC_SUCCESS || code == CC_SHORT_PACKET {
                        CTRL_DATA_RESIDUAL[ctrl]
                            .store(VALID | u64::from(residual), Ordering::SeqCst);
                    } else {
                        CTRL_RESULT[ctrl].store(
                            VALID | (u64::from(code) << 32) | u64::from(residual),
                            Ordering::SeqCst,
                        );
                    }
                    continue;
                }
                let idx = mbox(ctrl, slot, dci);
                let h = HANDLERS[idx].load(Ordering::Acquire);
                if h != 0 {
                    // SAFETY: only `set_endpoint_handler` stores here, and it stores valid `EndpointHandler`s.
                    let f: EndpointHandler =
                        unsafe { core::mem::transmute::<usize, EndpointHandler>(h) };
                    f(
                        DeviceRef {
                            ctrl: ctrl as u8,
                            slot,
                        },
                        dci,
                        code,
                        residual,
                    );
                } else {
                    MAILBOX[idx].store(
                        VALID | (u64::from(code) << 32) | u64::from(residual),
                        Ordering::SeqCst,
                    );
                }
            }
            Event::None => {}
        }
    }
}

fn irq_handler(_: &mut crate::idt::TrapFrame) {
    for i in 0..MAX_CONTROLLERS {
        process_events(i);
    }
}

/// Waits for `slot` of the atomic to become valid, polling the event ring meanwhile.
fn wait_atomic(ctrl: usize, a: &AtomicU64, timeout_ms: u64) -> Option<u64> {
    let deadline = time::now_ns() + timeout_ms * 1_000_000;
    loop {
        let v = a.swap(0, Ordering::SeqCst);
        if v & VALID != 0 {
            return Some(v & !VALID);
        }
        process_events(ctrl);
        let v = a.swap(0, Ordering::SeqCst);
        if v & VALID != 0 {
            return Some(v & !VALID);
        }
        if time::now_ns() >= deadline {
            return None;
        }
        arch::enable_and_halt();
    }
}

fn read_ext_caps(base: u64, hcc1: u32, mut f: impl FnMut(u32, u64)) {
    let mut off = u64::from(hcc1 >> 16) * 4;
    for _ in 0..64 {
        if off == 0 {
            break;
        }
        let dw0 = mmio_read32(base + off);
        f(dw0 & 0xFF, base + off);
        let next = u64::from((dw0 >> 8) & 0xFF) * 4;
        if next == 0 {
            break;
        }
        off += next;
    }
}

/// Initialises one controller, returning its index.
pub fn init(boot: &BootInfo, dev: &pci::Device) -> Result<usize, UsbError> {
    let index = CONTROLLERS.lock().len();
    if index >= MAX_CONTROLLERS {
        return Err(UsbError::Unsupported("too many xHCI controllers"));
    }
    pci::enable_bus_master(dev);
    let base = pci::map_bar(boot, dev, 0).map_err(|_| UsbError::Controller("cannot map BAR0"))?;
    let (cap_len, hcs1, hcs2, hcc1) = (
        u64::from(mmio_read32(base) & 0xFF),
        mmio_read32(base + 4),
        mmio_read32(base + 8),
        mmio_read32(base + 0x10),
    );
    let (db, rt) = (
        base + u64::from(mmio_read32(base + 0x14) & !3),
        base + u64::from(mmio_read32(base + 0x18) & !0x1F),
    );
    let op = base + cap_len;
    let ac64 = hcc1 & 1 != 0;
    let below_4g = !ac64;

    // BIOS -> OS ownership hand-off and SMI disable (USB Legacy Support capability).
    read_ext_caps(base, hcc1, |id, addr| {
        if id != 1 {
            return;
        }
        let legsup = mmio_read32(addr);
        if legsup & (1 << 16) != 0 {
            mmio_write32(addr, legsup | (1 << 24));
            if !wait_for(1000, || mmio_read32(addr) & (1 << 16) == 0) {
                kprintln!("xhci{index}: BIOS did not release ownership; taking over");
            }
        }
        let ctl = addr + 4;
        mmio_write32(
            ctl,
            (mmio_read32(ctl) & !((0x3 << 1) | (0xFF << 5) | (0x7 << 17))) | (0x7 << 29),
        );
    });

    // Stop and reset.
    if !wait_for(1000, || mmio_read32(op + USBSTS) & STS_NOT_READY == 0) {
        return Err(UsbError::Controller("controller never became ready"));
    }
    if mmio_read32(op + USBSTS) & STS_HALTED == 0 {
        mmio_write32(op + USBCMD, mmio_read32(op + USBCMD) & !CMD_RUN);
        if !wait_for(1000, || mmio_read32(op + USBSTS) & STS_HALTED != 0) {
            return Err(UsbError::Controller("controller did not halt"));
        }
    }
    mmio_write32(op + USBCMD, CMD_RESET);
    if !wait_for(1000, || {
        mmio_read32(op + USBCMD) & CMD_RESET == 0 && mmio_read32(op + USBSTS) & STS_NOT_READY == 0
    }) {
        return Err(UsbError::Controller("controller reset timed out"));
    }
    if mmio_read32(op + PAGESIZE) & 1 == 0 {
        return Err(UsbError::Unsupported("controller lacks 4 KiB page support"));
    }

    let max_slots = ((hcs1 & 0xFF) as usize).min(SLOT_LIMIT);
    let max_ports = (hcs1 >> 24) as u8;
    mmio_write32(op + CONFIG, max_slots as u32);

    let hhdm = boot.hhdm;
    let oom = || UsbError::OutOfMemory;
    let dcbaa = Dma::alloc(hhdm, 4096, below_4g).ok_or_else(oom)?;
    // Scratchpad buffers the controller wants for its own use.
    let n_scratch = (((hcs2 >> 21) & 0x1F) << 5 | ((hcs2 >> 27) & 0x1F)) as usize;
    let mut scratch_pages = Vec::new();
    let mut scratch_array = Vec::new();
    if n_scratch > 0 {
        let array = Dma::alloc(hhdm, n_scratch * 8, below_4g).ok_or_else(oom)?;
        for i in 0..n_scratch {
            let page = Dma::alloc(hhdm, 4096, below_4g).ok_or_else(oom)?;
            array.write64(i * 8, page.phys);
            scratch_pages.push(page);
        }
        dcbaa.write64(0, array.phys);
        scratch_array.push(array);
    }
    mmio_write64(op + DCBAAP, dcbaa.phys);

    let cmd = Ring::new(hhdm, below_4g).ok_or_else(oom)?;
    mmio_write64(op + CRCR, cmd.dma.phys | 1);

    let ev_ring = Dma::alloc(hhdm, RING_TRBS * 16, below_4g).ok_or_else(oom)?;
    let erst = Dma::alloc(hhdm, 4096, below_4g).ok_or_else(oom)?;
    erst.write64(0, ev_ring.phys);
    erst.write32(8, RING_TRBS as u32);
    mmio_write32(rt + 0x28, 1); // ERSTSZ
    mmio_write64(rt + 0x38, ev_ring.phys); // ERDP
    mmio_write64(rt + 0x30, erst.phys); // ERSTBA (arms the ring)
    mmio_write32(rt + 0x20, 2 | 1); // IMAN: interrupt enable, clear pending
    mmio_write32(rt + 0x24, 0); // IMOD: no moderation

    let control_buf = Dma::alloc(hhdm, 4096, below_4g).ok_or_else(oom)?;
    let mut slots = Vec::new();
    slots.resize_with(SLOT_LIMIT + 1, || None);
    let ctrl = Controller {
        op,
        rt,
        db,
        hhdm,
        below_4g,
        csz: if hcc1 & (1 << 2) != 0 { 64 } else { 32 },
        max_ports,
        dcbaa,
        cmd,
        ev_ring,
        ev_deq: 0,
        ev_ccs: true,
        _erst: erst,
        _scratch: scratch_array,
        _scratch_pages: scratch_pages,
        slots,
        control_buf,
        control_busy: false,
    };
    CONTROLLERS.lock().push(ctrl);

    // Interrupts: MSI-X/MSI to our handler; polling covers the case where that fails.
    let vector = irq::alloc_vector().ok_or(UsbError::Controller("no interrupt vector"))?;
    irq::register(vector, irq_handler);
    match pci::enable_msi(boot, dev, vector, crate::apic::id()) {
        Ok(kind) => kprintln!("xhci{index}: {kind} on vector {vector:#x}"),
        Err(e) => kprintln!("xhci{index}: no MSI/MSI-X ({e:?}); polling the event ring"),
    }

    mmio_write32(op + USBCMD, CMD_RUN | CMD_INTE);
    if !wait_for(1000, || mmio_read32(op + USBSTS) & STS_HALTED == 0) {
        return Err(UsbError::Controller("controller did not start"));
    }
    command(index, 0, 0, CMD_NOOP << 10, 0)?;
    kprintln!(
        "xhci{index}: v{:x}.{:02x}, {max_ports} ports, {max_slots} slots, {}-byte contexts, {}-bit DMA, {n_scratch} scratchpad page(s)",
        mmio_read32(base) >> 24,
        (mmio_read32(base) >> 16) & 0xFF,
        if hcc1 & (1 << 2) != 0 { 64 } else { 32 },
        if ac64 { 64 } else { 32 },
    );
    Ok(index)
}

/// Issues a command and waits for its completion; returns `(slot id, completion code)`.
fn command(
    ctrl: usize,
    param: u64,
    status: u32,
    control: u32,
    slot: u8,
) -> Result<(u8, u8), UsbError> {
    CMD_RESULT[ctrl].store(0, Ordering::SeqCst);
    {
        let mut g = CONTROLLERS.lock();
        let c = g.get_mut(ctrl).ok_or(UsbError::NoDevice)?;
        c.cmd.push(param, status, control | (u32::from(slot) << 24));
        c.ring_doorbell(0, 0);
    }
    let v = wait_atomic(ctrl, &CMD_RESULT[ctrl], 2000).ok_or(UsbError::Timeout)?;
    let (code, slot) = ((v & 0xFF) as u8, ((v >> 8) & 0xFF) as u8);
    if code == CC_SUCCESS {
        Ok((slot, code))
    } else {
        Err(UsbError::Command(code))
    }
}

pub fn port_count(ctrl: usize) -> u8 {
    CONTROLLERS.lock().get(ctrl).map_or(0, |c| c.max_ports)
}

/// Raw PORTSC of `port` (1-based).
pub fn port_status(ctrl: usize, port: u8) -> u32 {
    CONTROLLERS.lock().get(ctrl).map_or(0, |c| c.portsc(port))
}

/// Powers a port if needed and returns once it has had time to detect a device.
pub fn power_port(ctrl: usize, port: u8) {
    let g = CONTROLLERS.lock();
    if let Some(c) = g.get(ctrl) {
        let v = c.portsc(port);
        if v & PORT_POWER == 0 {
            c.portsc_write(port, (v & PORT_PRESERVE) | PORT_POWER);
        }
    }
}

/// Clears all change bits of a port.
pub fn clear_port_changes(ctrl: usize, port: u8) {
    let g = CONTROLLERS.lock();
    if let Some(c) = g.get(ctrl) {
        let v = c.portsc(port);
        c.portsc_write(port, (v & PORT_PRESERVE) | (v & PORT_CHANGE_BITS));
    }
}

/// Resets a root port and returns the resulting `PORTSC`, or an error if it does not come up enabled.
pub fn reset_port(ctrl: usize, port: u8) -> Result<u32, UsbError> {
    {
        let g = CONTROLLERS.lock();
        let c = g.get(ctrl).ok_or(UsbError::NoDevice)?;
        let v = c.portsc(port);
        c.portsc_write(
            port,
            (v & PORT_PRESERVE) | (v & PORT_CHANGE_BITS) | PORT_RESET,
        );
    }
    let deadline = time::now_ns() + 500_000_000;
    loop {
        let v = port_status(ctrl, port);
        if v & PORT_PRC != 0 {
            break;
        }
        if time::now_ns() > deadline {
            return Err(UsbError::Timeout);
        }
        time::sleep_ms(2);
    }
    clear_port_changes(ctrl, port);
    time::sleep_ms(10); // recovery time
    let v = port_status(ctrl, port);
    if v & PORT_PED == 0 || v & PORT_CCS == 0 {
        return Err(UsbError::Controller("port did not enable after reset"));
    }
    Ok(v)
}

/// Decodes the speed ID from a `PORTSC` value.
pub fn portsc_speed(v: u32) -> u8 {
    ((v >> 10) & 0xF) as u8
}

pub fn portsc_connected(v: u32) -> bool {
    v & PORT_CCS != 0
}

pub fn portsc_enabled(v: u32) -> bool {
    v & PORT_PED != 0
}

// ---- contexts ---------------------------------------------------------------------------

impl Controller {
    fn dw(&self, d: &Dma, ctx: usize, dword: usize, v: u32) {
        d.write32(ctx * self.csz + dword * 4, v);
    }

    fn slot_mut(&mut self, slot: u8) -> Result<&mut SlotState, UsbError> {
        self.slots
            .get_mut(usize::from(slot))
            .and_then(Option::as_mut)
            .ok_or(UsbError::NoDevice)
    }
}

/// Parameters describing where a new device sits in the topology.
#[derive(Clone, Copy)]
pub struct Attach {
    pub speed: u8,
    pub root_port: u8,
    pub route: u32,
    pub depth: u8,
    pub tt: Option<(u8, u8)>,
}

/// Enables a slot and addresses the device (steps 1-3 of enumeration).
pub fn address_device(ctrl: usize, at: Attach) -> Result<DeviceRef, UsbError> {
    let (slot, _) = command(ctrl, 0, 0, CMD_ENABLE_SLOT << 10, 0)?;
    if usize::from(slot) > SLOT_LIMIT || slot == 0 {
        return Err(UsbError::NoSlot);
    }
    let result = (|| {
        let input;
        {
            let mut g = CONTROLLERS.lock();
            let c = g.get_mut(ctrl).ok_or(UsbError::NoDevice)?;
            let (hhdm, below) = (c.hhdm, c.below_4g);
            let dev_ctx = Dma::alloc(hhdm, 4096, below).ok_or(UsbError::OutOfMemory)?;
            let input_ctx = Dma::alloc(hhdm, 4096, below).ok_or(UsbError::OutOfMemory)?;
            let ep0 = Ring::new(hhdm, below).ok_or(UsbError::OutOfMemory)?;
            c.dcbaa.write64(usize::from(slot) * 8, dev_ctx.phys);
            let mps = u32::from(fmt::default_ep0_packet_size(at.speed));
            // Input control context: add slot and EP0.
            c.dw(&input_ctx, 0, 1, 0b11);
            // Slot context.
            c.dw(
                &input_ctx,
                1,
                0,
                at.route | (u32::from(at.speed) << 20) | (1 << 27),
            );
            c.dw(&input_ctx, 1, 1, u32::from(at.root_port) << 16);
            if let Some((hub_slot, hub_port)) = at.tt {
                c.dw(
                    &input_ctx,
                    1,
                    2,
                    u32::from(hub_slot) | (u32::from(hub_port) << 8),
                );
            }
            // EP0 context: control endpoint, 3 error retries.
            c.dw(&input_ctx, 2, 1, (3 << 1) | (4 << 3) | (mps << 16));
            let ptr = ep0.dma.phys | 1; // DCS = 1
            c.dw(&input_ctx, 2, 2, ptr as u32);
            c.dw(&input_ctx, 2, 3, (ptr >> 32) as u32);
            c.dw(&input_ctx, 2, 4, 8);
            input = input_ctx.phys;
            let mut rings: [Option<Ring>; 32] = [const { None }; 32];
            rings[1] = Some(ep0);
            c.slots[usize::from(slot)] = Some(SlotState {
                dev_ctx,
                input_ctx,
                rings,
                speed: at.speed,
                root_port: at.root_port,
                route: at.route,
                depth: at.depth,
                tt: at.tt,
            });
        }
        command(ctrl, input, 0, CMD_ADDRESS_DEVICE << 10, slot)?;
        Ok(DeviceRef {
            ctrl: ctrl as u8,
            slot,
        })
    })();
    if result.is_err() {
        let _ = command(ctrl, 0, 0, CMD_DISABLE_SLOT << 10, slot);
        if let Some(c) = CONTROLLERS.lock().get_mut(ctrl) {
            c.slots[usize::from(slot)] = None;
        }
    }
    result
}

/// Topology facts about an addressed device.
pub fn attach_info(dev: DeviceRef) -> Option<Attach> {
    let g = CONTROLLERS.lock();
    let s = g
        .get(usize::from(dev.ctrl))?
        .slots
        .get(usize::from(dev.slot))?
        .as_ref()?;
    Some(Attach {
        speed: s.speed,
        root_port: s.root_port,
        route: s.route,
        depth: s.depth,
        tt: s.tt,
    })
}

/// Updates the EP0 max packet size (full-speed devices report it in their descriptor).
pub fn set_ep0_packet_size(dev: DeviceRef, mps: u16) -> Result<(), UsbError> {
    let input;
    {
        let mut g = CONTROLLERS.lock();
        let c = g.get_mut(usize::from(dev.ctrl)).ok_or(UsbError::NoDevice)?;
        let csz = c.csz;
        let s = c.slot_mut(dev.slot)?;
        // Input context: evaluate EP0 only; copy the current EP0 context from the device context.
        for i in 0..8 {
            let v = s.dev_ctx.read32(csz + i * 4);
            s.input_ctx.write32(2 * csz + i * 4, v);
        }
        s.input_ctx.write32(0, 0);
        s.input_ctx.write32(4, 0b10);
        let dw1 = s.input_ctx.read32(2 * csz + 4);
        s.input_ctx
            .write32(2 * csz + 4, (dw1 & 0xFFFF) | (u32::from(mps) << 16));
        input = s.input_ctx.phys;
    }
    command(
        usize::from(dev.ctrl),
        input,
        0,
        CMD_EVALUATE_CONTEXT << 10,
        dev.slot,
    )
    .map(|_| ())
}

/// Description of an endpoint to bring up.
#[derive(Clone, Copy)]
pub struct EndpointConfig {
    pub dci: u8,
    pub kind: fmt::TransferKind,
    pub is_in: bool,
    pub max_packet: u16,
    pub max_burst: u8,
    pub interval: u8,
}

/// Hub parameters for the slot context.
#[derive(Clone, Copy)]
pub struct HubConfig {
    pub ports: u8,
    pub think_time: u8,
}

/// Adds endpoints (and optionally marks the slot as a hub) with a Configure Endpoint command.
pub fn configure_endpoints(
    dev: DeviceRef,
    eps: &[EndpointConfig],
    hub: Option<HubConfig>,
) -> Result<(), UsbError> {
    let input;
    {
        let mut g = CONTROLLERS.lock();
        let c = g.get_mut(usize::from(dev.ctrl)).ok_or(UsbError::NoDevice)?;
        let (csz, hhdm, below) = (c.csz, c.hhdm, c.below_4g);
        let s = c.slot_mut(dev.slot)?;
        let speed = s.speed;
        for i in 0..32 * csz / 4 {
            s.input_ctx.write32(i * 4, 0);
        }
        // Start from the current slot context.
        let mut slot_dw = [0u32; 4];
        for (i, d) in slot_dw.iter_mut().enumerate() {
            *d = s.dev_ctx.read32(i * 4);
        }
        let max_dci = eps
            .iter()
            .map(|e| e.dci)
            .max()
            .unwrap_or(1)
            .max(((slot_dw[0] >> 27) & 0x1F) as u8);
        slot_dw[0] = (slot_dw[0] & !(0x1F << 27)) | (u32::from(max_dci) << 27);
        if let Some(h) = hub {
            slot_dw[0] |= 1 << 26;
            slot_dw[1] = (slot_dw[1] & 0x00FF_FFFF) | (u32::from(h.ports) << 24);
            slot_dw[2] = (slot_dw[2] & !(3 << 16)) | (u32::from(h.think_time & 3) << 16);
        }
        let mut add = 1u32; // slot context
        for (i, d) in slot_dw.iter().enumerate() {
            s.input_ctx.write32(csz + i * 4, *d);
        }
        for e in eps {
            let ring = Ring::new(hhdm, below).ok_or(UsbError::OutOfMemory)?;
            let ep_type: u32 = match (e.kind, e.is_in) {
                (fmt::TransferKind::Isochronous, false) => 1,
                (fmt::TransferKind::Bulk, false) => 2,
                (fmt::TransferKind::Interrupt, false) => 3,
                (fmt::TransferKind::Control, _) => 4,
                (fmt::TransferKind::Isochronous, true) => 5,
                (fmt::TransferKind::Bulk, true) => 6,
                (fmt::TransferKind::Interrupt, true) => 7,
            };
            let base = (1 + usize::from(e.dci)) * csz;
            let interval = if matches!(
                e.kind,
                fmt::TransferKind::Interrupt | fmt::TransferKind::Isochronous
            ) {
                u32::from(fmt::interval_exponent(speed, e.interval))
            } else {
                0
            };
            let esit = u32::from(e.max_packet) * (u32::from(e.max_burst) + 1);
            s.input_ctx.write32(base, interval << 16);
            s.input_ctx.write32(
                base + 4,
                (3 << 1)
                    | (ep_type << 3)
                    | (u32::from(e.max_burst) << 8)
                    | (u32::from(e.max_packet) << 16),
            );
            let ptr = ring.dma.phys | 1;
            s.input_ctx.write32(base + 8, ptr as u32);
            s.input_ctx.write32(base + 12, (ptr >> 32) as u32);
            s.input_ctx.write32(
                base + 16,
                u32::from(e.max_packet).min(0xFFFF) | (esit.min(0xFFFF) << 16),
            );
            s.rings[usize::from(e.dci)] = Some(ring);
            add |= 1 << e.dci;
        }
        s.input_ctx.write32(4, add);
        input = s.input_ctx.phys;
    }
    command(
        usize::from(dev.ctrl),
        input,
        0,
        CMD_CONFIGURE_ENDPOINT << 10,
        dev.slot,
    )
    .map(|_| ())
}

// ---- transfers --------------------------------------------------------------------------

fn code_to_error(code: u8) -> UsbError {
    if code == CC_STALL {
        UsbError::Stall
    } else {
        UsbError::Transfer(code)
    }
}

/// Resets a halted endpoint and moves its dequeue pointer past the failed TD.
fn recover_endpoint(dev: DeviceRef, dci: u8) {
    let ctrl = usize::from(dev.ctrl);
    let _ = command(
        ctrl,
        0,
        0,
        (CMD_RESET_ENDPOINT << 10) | (u32::from(dci) << 16),
        dev.slot,
    );
    let target = {
        let mut g = CONTROLLERS.lock();
        g.get_mut(ctrl)
            .and_then(|c| c.slot_mut(dev.slot).ok())
            .and_then(|s| {
                s.rings[usize::from(dci)]
                    .as_ref()
                    .map(|r| r.enqueue_phys() | u64::from(r.pcs))
            })
    };
    if let Some(ptr) = target {
        let _ = command(
            ctrl,
            ptr,
            0,
            (CMD_SET_TR_DEQUEUE << 10) | (u32::from(dci) << 16),
            dev.slot,
        );
    }
}

/// A synchronous control transfer on endpoint 0. For IN requests the data lands in `data`;
/// for OUT requests `data` is sent. Returns the number of bytes transferred.
pub fn control(dev: DeviceRef, setup: Setup, data: &mut [u8]) -> Result<usize, UsbError> {
    let ctrl = usize::from(dev.ctrl);
    let len = if setup.is_in() {
        usize::from(setup.length).min(data.len())
    } else {
        data.len()
    };
    if len > 4096 || (setup.is_in() && usize::from(setup.length) > 4096) {
        return Err(UsbError::Unsupported("control transfer larger than 4 KiB"));
    }
    CTRL_RESULT[ctrl].store(0, Ordering::SeqCst);
    CTRL_DATA_RESIDUAL[ctrl].store(0, Ordering::SeqCst);
    let status_trb;
    {
        let mut g = CONTROLLERS.lock();
        let c = g.get_mut(ctrl).ok_or(UsbError::NoDevice)?;
        if c.control_busy {
            return Err(UsbError::Busy);
        }
        let buf = c.control_buf;
        if !setup.is_in() {
            // SAFETY: `control_buf` is a page we own; `len <= 4096` was checked above.
            unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), buf.virt, len) };
        }
        let ring = c.slot_mut(dev.slot)?.rings[1]
            .as_mut()
            .ok_or(UsbError::NoDevice)?;
        let trt = match (len > 0, setup.is_in()) {
            (false, _) => 0u32,
            (true, false) => 2,
            (true, true) => 3,
        };
        ring.push(
            setup.to_u64(),
            8,
            (TRB_SETUP << 10) | (1 << 6) | (trt << 16),
        ); // IDT: setup packet inline
        if len > 0 {
            let dir_in = u32::from(setup.is_in());
            ring.push(
                buf.phys,
                len as u32,
                (TRB_DATA << 10) | (dir_in << 16) | (1 << 2),
            ); // ISP
        }
        let status_dir = u32::from(!(len > 0 && setup.is_in())); // IN status unless data was IN
        status_trb = ring.push(0, 0, (TRB_STATUS << 10) | (1 << 5) | (status_dir << 16)); // IOC
        CTRL_EXPECT[ctrl].store(status_trb, Ordering::SeqCst);
        c.control_busy = true;
        c.ring_doorbell(dev.slot, 1);
    }
    let r = wait_atomic(ctrl, &CTRL_RESULT[ctrl], 2000);
    let (copy, outcome) = {
        let mut g = CONTROLLERS.lock();
        let c = g.get_mut(ctrl).ok_or(UsbError::NoDevice)?;
        c.control_busy = false;
        let buf = c.control_buf;
        (buf, r)
    };
    let Some(v) = outcome else {
        return Err(UsbError::Timeout);
    };
    let code = ((v >> 32) & 0xFF) as u8;
    if code != CC_SUCCESS && code != CC_SHORT_PACKET {
        if code == CC_STALL {
            recover_endpoint(dev, 1);
        }
        return Err(code_to_error(code));
    }
    let residual = {
        let d = CTRL_DATA_RESIDUAL[ctrl].swap(0, Ordering::SeqCst);
        if d & VALID != 0 {
            (d & 0xFF_FFFF) as usize
        } else {
            0
        }
    };
    let got = len.saturating_sub(residual);
    if setup.is_in() {
        // SAFETY: `control_buf` holds at least `got <= len` bytes just written by the device.
        unsafe {
            core::ptr::copy_nonoverlapping(copy.virt, data.as_mut_ptr(), got.min(data.len()))
        };
    }
    Ok(got)
}

/// Queues one transfer (Normal TRB) on a bulk/interrupt endpoint. `ioc` requests an event.
pub fn queue(dev: DeviceRef, dci: u8, phys: u64, len: u32) -> Result<(), UsbError> {
    if !(2..=31).contains(&dci) {
        return Err(UsbError::Unsupported("bad endpoint"));
    }
    let ctrl = usize::from(dev.ctrl);
    let mut g = CONTROLLERS.lock();
    let c = g.get_mut(ctrl).ok_or(UsbError::NoDevice)?;
    let ring = c.slot_mut(dev.slot)?.rings[usize::from(dci)]
        .as_mut()
        .ok_or(UsbError::NoDevice)?;
    ring.push(
        phys,
        len & 0x1_FFFF,
        (TRB_NORMAL << 10) | (1 << 5) | (1 << 2),
    ); // IOC | ISP
    c.ring_doorbell(dev.slot, u32::from(dci));
    Ok(())
}

/// Synchronous bulk/interrupt transfer: queues the buffer and waits; returns bytes transferred.
pub fn transfer(
    dev: DeviceRef,
    dci: u8,
    phys: u64,
    len: u32,
    timeout_ms: u64,
) -> Result<usize, UsbError> {
    let ctrl = usize::from(dev.ctrl);
    let idx = mbox(ctrl, dev.slot, dci);
    MAILBOX[idx].store(0, Ordering::SeqCst);
    queue(dev, dci, phys, len)?;
    let v = wait_atomic(ctrl, &MAILBOX[idx], timeout_ms).ok_or(UsbError::Timeout)?;
    let code = ((v >> 32) & 0xFF) as u8;
    if code != CC_SUCCESS && code != CC_SHORT_PACKET {
        if code == CC_STALL {
            recover_endpoint(dev, dci);
        }
        return Err(code_to_error(code));
    }
    Ok((len as usize).saturating_sub((v & 0xFF_FFFF) as usize))
}

/// Routes completions of a streaming endpoint to `handler` instead of the mailbox.
pub fn set_endpoint_handler(dev: DeviceRef, dci: u8, handler: EndpointHandler) {
    HANDLERS[mbox(usize::from(dev.ctrl), dev.slot, dci)].store(handler as usize, Ordering::Release);
}

/// Allocates a DMA buffer for the controller's addressing capability.
pub fn alloc_buffer(dev: DeviceRef, len: usize) -> Option<Dma> {
    let g = CONTROLLERS.lock();
    let c = g.get(usize::from(dev.ctrl))?;
    Dma::alloc(c.hhdm, len, c.below_4g)
}
