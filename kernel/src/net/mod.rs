// SPDX-License-Identifier: Apache-2.0
//! Networking: NIC drivers ([`igc`], [`virtio`]) feeding the `ksnet` IP stack.
//!
//! Drivers are polled from the UI idle loop (1 kHz tick); the receive rings are sized for that
//! (512 frames ≈ 2.5 ms of 2.5 GbE line rate for full-size frames). Interrupt-driven receive is a
//! Gate-G2 follow-up (docs/hardening/stage-1.md).

pub mod igc;
pub mod virtio;

use crate::boot::BootInfo;
use crate::dma::Dma;
use crate::sync::IrqMutex;
use crate::{rng, time};
use alloc::format;
use alloc::string::String;
use core::sync::atomic::{AtomicU32, Ordering};
use ksnet::dma::DmaMem;
use ksnet::stack::PingResult;
use ksnet::{Mac, Nic, Stack};

/// Interrupts delivered by the NIC (handlers only count; `service()` does the work).
pub static NET_IRQS: AtomicU32 = AtomicU32::new(0);

/// Volatile access to a DMA buffer for the ring logic in `ksnet`.
impl DmaMem for Dma {
    fn len(&self) -> usize {
        self.len
    }
    fn read32(&self, off: usize) -> u32 {
        Dma::read32(self, off)
    }
    fn write32(&self, off: usize, v: u32) {
        Dma::write32(self, off, v)
    }
    fn read_bytes(&self, off: usize, out: &mut [u8]) {
        assert!(off + out.len() <= self.len);
        for (i, b) in out.iter_mut().enumerate() {
            // SAFETY: bounds checked above; volatile because the device writes this memory.
            *b = unsafe { self.virt.add(off + i).read_volatile() };
        }
    }
    fn write_bytes(&self, off: usize, data: &[u8]) {
        assert!(off + data.len() <= self.len);
        for (i, b) in data.iter().enumerate() {
            // SAFETY: bounds checked above; volatile because the device reads this memory.
            unsafe { self.virt.add(off + i).write_volatile(*b) };
        }
    }
}

pub use crate::mmio::Mmio;

/// The NIC drivers behind one type, so the stack stays monomorphic.
pub enum Driver {
    Igc(igc::Igc),
    Virtio(alloc::boxed::Box<virtio::VirtioNet>),
}

impl Driver {
    pub fn name(&self) -> &'static str {
        match self {
            Driver::Igc(d) => d.name(),
            Driver::Virtio(_) => "virtio-net",
        }
    }
    pub fn describe_link(&mut self) -> String {
        match self {
            Driver::Igc(d) => d.describe_link(),
            Driver::Virtio(d) => d.describe_link(),
        }
    }
    pub fn irq_enabled(&self) -> bool {
        match self {
            Driver::Igc(d) => d.irq_enabled(),
            Driver::Virtio(d) => d.irq_enabled(),
        }
    }
    pub fn rearm_irq(&mut self) {
        match self {
            Driver::Igc(d) => d.rearm_irq(),
            Driver::Virtio(d) => d.rearm_irq(),
        }
    }
}

impl Nic for Driver {
    fn mac(&self) -> Mac {
        match self {
            Driver::Igc(d) => d.mac(),
            Driver::Virtio(d) => d.mac(),
        }
    }
    fn link_up(&mut self) -> bool {
        match self {
            Driver::Igc(d) => d.link_up(),
            Driver::Virtio(d) => d.link_up(),
        }
    }
    fn receive(&mut self, buf: &mut [u8]) -> Option<usize> {
        match self {
            Driver::Igc(d) => d.receive(buf),
            Driver::Virtio(d) => d.receive(buf),
        }
    }
    fn transmit(&mut self, frame: &[u8]) -> bool {
        match self {
            Driver::Igc(d) => d.transmit(frame),
            Driver::Virtio(d) => d.transmit(frame),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Waiting for link and an address.
    Configuring,
    /// Sending echo requests to the gateway (local connectivity check).
    Checking {
        sent: u8,
        next_at: u64,
    },
    Done,
}

struct Net {
    stack: Stack<Driver>,
    name: &'static str,
    last_link: Option<bool>,
    phase: Phase,
    ping_sent_at: u64,
    started: u64,
    last_poll: i64,
    last_irqs: u32,
    more: bool,
    v6_reported: bool,
}

static NET: IrqMutex<Option<Net>> = IrqMutex::new(None);
static STATUS: IrqMutex<(String, bool)> = IrqMutex::new((String::new(), false));

fn set_status(s: impl Into<String>) {
    let s = s.into();
    kprintln!("net: {s}");
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

/// Probes PCI for a supported NIC and starts the stack on the first one found.
pub fn init(boot: &BootInfo) {
    let mut driver = None;
    for dev in crate::pci::devices() {
        if igc::supports(dev) {
            match igc::Igc::new(boot, dev) {
                Ok(mut d) => {
                    d.enable_interrupts(boot, dev);
                    driver = Some(Driver::Igc(d));
                    break;
                }
                Err(e) => kprintln!("net: {} {}: init failed: {e}", dev.addr, "igc/igb"),
            }
        } else if virtio::supports(dev) {
            match virtio::VirtioNet::new(boot, dev) {
                Ok(d) => {
                    driver = Some(Driver::Virtio(alloc::boxed::Box::new(d)));
                    break;
                }
                Err(e) => kprintln!("net: {} virtio-net: init failed: {e}", dev.addr),
            }
        }
    }
    let Some(mut driver) = driver else {
        kprintln!("net: no supported network adapter found");
        return;
    };
    let name = driver.name();
    let mac = driver.mac();
    kprintln!(
        "net: {name} ready, mac {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}; {}",
        mac[0],
        mac[1],
        mac[2],
        mac[3],
        mac[4],
        mac[5],
        driver.describe_link()
    );
    let now = now_ms();
    let mac_word = mac.iter().fold(0u64, |a, b| (a << 8) | u64::from(*b));
    let (seed, hw_rng) = rng::network_seed(mac_word);
    if !hw_rng {
        kprintln!("net: no hardware RNG; TCP sequence numbers are seeded from the TSC (weaker)");
    }
    let stack = Stack::new(driver, seed, now);
    *NET.lock() = Some(Net {
        stack,
        name,
        last_link: None,
        phase: Phase::Configuring,
        ping_sent_at: 0,
        started: now as u64,
        last_poll: now,
        last_irqs: 0,
        more: false,
        v6_reported: false,
    });
}

fn now_ms() -> i64 {
    (time::now_ns() / 1_000_000) as i64
}

/// Advances the network stack. Call from the idle loop (at least every few milliseconds).
pub fn service() {
    let Some(mut g) = NET.try_lock() else { return };
    let Some(n) = g.as_mut() else { return };
    let now = now_ms();
    // Interrupt-driven receive: poll when the NIC signalled, when the previous pass ran out of
    // budget, or at least every 10 ms (protocol timers, and a safety net if an interrupt is lost).
    let irqs = NET_IRQS.load(Ordering::Acquire);
    let signalled = irqs != n.last_irqs;
    let irq_mode = n.stack.nic().irq_enabled();
    if !(signalled || n.more || !irq_mode || now - n.last_poll >= 10) {
        return;
    }
    n.last_irqs = irqs;
    n.last_poll = now;
    n.more = n.stack.poll(now);
    n.stack.nic().rearm_irq();
    if n.more {
        return; // the budget ran out: yield to the UI now, continue on the next pass
    }

    let link = n.stack.nic().link_up();
    if n.last_link != Some(link) {
        n.last_link = Some(link);
        let desc = n.stack.nic().describe_link();
        if link {
            set_status(format!(
                "{}: link up ({desc}); requesting an address",
                n.name
            ));
            n.phase = Phase::Configuring;
        } else {
            set_status(format!("{}: no link (cable unplugged?)", n.name));
            n.phase = Phase::Configuring;
        }
    }

    if !n.v6_reported {
        if let Some(a) = n.stack.global_ipv6() {
            n.v6_reported = true;
            set_status(format!("{}: IPv6 address {a}", n.name));
        }
    }
    let (name, started) = (n.name, n.started);
    match n.phase {
        Phase::Configuring => {
            if let Some(l) = n.stack.lease().cloned() {
                let router = l.router;
                let dns = l.dns.first().copied();
                set_status(format!(
                    "{name}: {} gateway {} dns {}",
                    l.address,
                    router.map_or("none".into(), |r| format!("{r}")),
                    dns.map_or("none".into(), |d| format!("{d}"))
                ));
                n.phase = Phase::Checking {
                    sent: 0,
                    next_at: now as u64,
                };
            } else if now as u64 - started > 20_000 && now % 5_000 == 0 && link {
                kprintln!(
                    "net: still waiting for a DHCP lease ({} s)",
                    (now as u64 - started) / 1000
                );
            }
        }
        Phase::Checking { sent, next_at } => {
            if let PingResult::Reply { from, .. } = n.stack.ping_result() {
                let rtt = (time::now_ns() / 1_000_000).saturating_sub(n.ping_sent_at);
                set_status(format!("{name}: gateway {from} reachable ({rtt} ms)"));
                n.phase = Phase::Done;
            } else if now as u64 >= next_at {
                let gw = n.stack.lease().and_then(|l| l.router);
                match gw {
                    Some(gw) if sent < 5 => {
                        n.ping_sent_at = time::now_ns() / 1_000_000;
                        if let Err(e) = n.stack.ping(gw, u16::from(sent)) {
                            kprintln!("net: ping {gw} could not be queued: {e:?}");
                        }
                        n.phase = Phase::Checking {
                            sent: sent + 1,
                            next_at: now as u64 + 1_000,
                        };
                    }
                    Some(gw) => {
                        let st = n.stack.filter_stats();
                        kprintln!(
                            "net: ingress filter: {} passed, {} dropped (last drop: {:?}), {} echo replies sent",
                            st.passed, st.dropped, st.last_drop, st.echo_replies
                        );
                        set_status(format!("{name}: gateway {gw} does not answer ping"));
                        n.phase = Phase::Done;
                    }
                    None => n.phase = Phase::Done,
                }
            }
        }
        Phase::Done => {
            if n.stack.lease().is_none() {
                n.phase = Phase::Configuring;
            }
        }
    }
}

/// Runs `f` with the stack, if a NIC was found. Drivers and apps use this API.
pub fn with<R>(f: impl FnOnce(&mut Stack<Driver>, i64) -> R) -> Option<R> {
    let mut g = NET.lock();
    let n = g.as_mut()?;
    Some(f(&mut n.stack, now_ms()))
}

/// Number of NIC interrupts delivered so far, and whether the NIC runs interrupt-driven.
pub fn irq_status() -> (u32, bool) {
    let mut g = NET.lock();
    let mode = g.as_mut().is_some_and(|n| n.stack.nic().irq_enabled());
    (NET_IRQS.load(Ordering::Acquire), mode)
}

/// Whether a NIC driver is running.
pub fn present() -> bool {
    NET.lock().is_some()
}
