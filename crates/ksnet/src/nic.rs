// SPDX-License-Identifier: Apache-2.0
//! The driver-facing NIC trait and its adapter into smoltcp's `Device`.

use crate::filter::{self, Config, RateLimit, Reason, Verdict};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;

pub type Mac = [u8; 6];

/// Largest Ethernet frame (without FCS) the stack will hand to a driver or accept from one.
pub const MAX_FRAME: usize = 1514;

/// What a network driver provides. Frames are plain Ethernet II without the FCS.
pub trait Nic {
    fn mac(&self) -> Mac;
    /// Whether the physical link is currently up.
    fn link_up(&mut self) -> bool;
    /// Copies the next received frame into `buf` and returns its length, or `None` if none is pending.
    /// Frames longer than `buf` must be dropped by the driver, not truncated.
    fn receive(&mut self, buf: &mut [u8]) -> Option<usize>;
    /// Queues `frame` for transmission. `false` means the transmit ring is full (the frame is lost
    /// and TCP/DHCP will retransmit).
    fn transmit(&mut self, frame: &[u8]) -> bool;
}

/// Adapts a [`Nic`] to smoltcp.
pub struct NicDevice<N: Nic> {
    pub nic: N,
    rx: [u8; MAX_FRAME],
    /// Ingress filter configuration (kept current by the stack).
    pub filter: Config,
    /// Frames the stack may still take in this poll (bounds the work one pass can do).
    pub budget: u32,
    pub now_ms: i64,
    echo_limit: RateLimit,
    pub stats: Stats,
}

/// Counters for the ingress filter, for diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub passed: u64,
    pub dropped: u64,
    pub echo_replies: u64,
    pub last_drop: Option<Reason>,
}

impl<N: Nic> NicDevice<N> {
    pub fn new(nic: N) -> Self {
        Self {
            nic,
            rx: [0; MAX_FRAME],
            filter: Config::default(),
            budget: u32::MAX,
            now_ms: 0,
            echo_limit: RateLimit::new(10),
            stats: Stats::default(),
        }
    }
}

pub struct Rx<'a> {
    frame: &'a [u8],
}

pub struct Tx<'a, N: Nic> {
    nic: &'a mut N,
}

impl RxToken for Rx<'_> {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(self.frame)
    }
}

impl<N: Nic> TxToken for Tx<'_, N> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut buf = [0u8; MAX_FRAME];
        let len = len.min(MAX_FRAME);
        let frame = buf.get_mut(..len).unwrap_or(&mut []);
        let r = f(frame);
        // A full ring simply drops the frame; the protocols above retransmit.
        let _ = self.nic.transmit(buf.get(..len).unwrap_or(&[]));
        r
    }
}

impl<N: Nic> Device for NicDevice<N> {
    type RxToken<'a>
        = Rx<'a>
    where
        Self: 'a;
    type TxToken<'a>
        = Tx<'a, N>
    where
        Self: 'a;

    fn receive(&mut self, _ts: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        loop {
            if self.budget == 0 {
                return None; // the caller polls again soon; the UI must not starve
            }
            let n = self.nic.receive(&mut self.rx)?;
            self.budget = self.budget.saturating_sub(1);
            let frame = self.rx.get(..n)?;
            match filter::ingress(frame, &self.filter) {
                Verdict::Pass => {
                    self.stats.passed = self.stats.passed.saturating_add(1);
                    let Self { nic, rx, .. } = self;
                    return Some((
                        Rx {
                            frame: rx.get(..n)?,
                        },
                        Tx { nic },
                    ));
                }
                Verdict::Drop(why) => {
                    self.stats.dropped = self.stats.dropped.saturating_add(1);
                    self.stats.last_drop = Some(why);
                }
                Verdict::EchoRequest => {
                    if self.echo_limit.allow(self.now_ms) {
                        if let Some(reply) = filter::echo_reply(frame) {
                            let _ = self.nic.transmit(&reply);
                            self.stats.echo_replies = self.stats.echo_replies.saturating_add(1);
                        }
                    }
                }
            }
        }
    }

    fn transmit(&mut self, _ts: Instant) -> Option<Self::TxToken<'_>> {
        Some(Tx { nic: &mut self.nic })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut c = DeviceCapabilities::default();
        c.medium = Medium::Ethernet;
        c.max_transmission_unit = MAX_FRAME;
        c.max_burst_size = Some(1);
        c
    }
}
