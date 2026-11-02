// SPDX-License-Identifier: Apache-2.0
//! HCI over USB: commands on the control endpoint, events on the interrupt IN endpoint, ACL data
//! on the bulk endpoints (Bluetooth USB transport, Core Spec Vol 4, Part B).
//!
//! Incoming transfers are delivered by the xHCI event path in interrupt context; they are
//! re-assembled into whole HCI packets (a packet may span several USB transfers) and queued for
//! [`Transport::recv`].

use crate::dma::Dma;
use crate::sync::IrqMutex;
use crate::usb::{self, xhci, Device, DeviceRef, UsbError};
use crate::{arch, time};
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use ksbt::framing::{Assembler, Kind};
use ksbt::host::{HostError, Packet, Transport};
use ksparse::usb::{self as fmt, Setup};

struct Rx {
    events: VecDeque<Vec<u8>>,
    acl: VecDeque<Vec<u8>>,
    event_asm: Assembler,
    acl_asm: Assembler,
}

struct Endpoints {
    dev: DeviceRef,
    event_dci: u8,
    event_buf: Dma,
    acl_buf: Dma,
    event_len: u32,
    acl_len: u32,
}

static RX: IrqMutex<Option<Rx>> = IrqMutex::new(None);
static EP: IrqMutex<Option<Endpoints>> = IrqMutex::new(None);

const QUEUE_LIMIT: usize = 256;

/// Interrupt-context completion handler for both IN endpoints.
fn on_transfer(dev: DeviceRef, dci: u8, code: u8, residual: u32) {
    let Some((phys, len, is_event, got)) = ({
        let g = EP.lock();
        g.as_ref().filter(|e| e.dev == dev).map(|e| {
            let is_event = dci == e.event_dci;
            let (buf, len) = if is_event {
                (&e.event_buf, e.event_len)
            } else {
                (&e.acl_buf, e.acl_len)
            };
            let got = (len as usize).saturating_sub(residual as usize);
            let data = buf.as_slice()[..got.min(buf.len)].to_vec();
            (buf.phys, len, is_event, data)
        })
    }) else {
        return;
    };
    if code == xhci::CC_SUCCESS || code == xhci::CC_SHORT_PACKET {
        let mut g = RX.lock();
        if let Some(rx) = g.as_mut() {
            let packets = if is_event {
                rx.event_asm.push(&got)
            } else {
                rx.acl_asm.push(&got)
            };
            for p in packets {
                let q = if is_event {
                    &mut rx.events
                } else {
                    &mut rx.acl
                };
                if q.len() < QUEUE_LIMIT {
                    q.push_back(p);
                }
            }
        }
    }
    let _ = xhci::queue(dev, dci, phys, len);
}

pub struct UsbTransport {
    pub dev: DeviceRef,
    interface: u8,
    out_dci: u8,
    out_buf: Dma,
}

impl UsbTransport {
    /// Opens the Bluetooth interface (class E0/01/01) of `d`: configures its endpoints and starts
    /// receiving events and ACL data.
    pub fn open(d: &Device) -> Result<Self, UsbError> {
        let iface = d
            .config
            .interfaces
            .iter()
            .find(|i| {
                i.alt == 0 && i.class == fmt::CLASS_WIRELESS && i.subclass == 1 && i.protocol == 1
            })
            .ok_or(UsbError::Unsupported("no Bluetooth HCI interface"))?;
        let ep = |kind: fmt::TransferKind, inward: bool| {
            iface
                .endpoints
                .iter()
                .copied()
                .find(|e| e.kind() == kind && e.is_in() == inward)
                .ok_or(UsbError::Unsupported(
                    "Bluetooth interface lacks an endpoint",
                ))
        };
        let (ev, acl_in, acl_out) = (
            ep(fmt::TransferKind::Interrupt, true)?,
            ep(fmt::TransferKind::Bulk, true)?,
            ep(fmt::TransferKind::Bulk, false)?,
        );
        let cfg = |e: fmt::Endpoint| xhci::EndpointConfig {
            dci: e.dci(),
            kind: e.kind(),
            is_in: e.is_in(),
            max_packet: e.packet_size(),
            max_burst: e.max_burst,
            interval: e.interval,
        };
        xhci::configure_endpoints(d.dev, &[cfg(ev), cfg(acl_in), cfg(acl_out)], None)?;
        let event_buf = xhci::alloc_buffer(d.dev, 4096).ok_or(UsbError::OutOfMemory)?;
        let acl_buf = xhci::alloc_buffer(d.dev, 4096).ok_or(UsbError::OutOfMemory)?;
        let out_buf = xhci::alloc_buffer(d.dev, 4096).ok_or(UsbError::OutOfMemory)?;
        let (event_len, acl_len) = (
            u32::from(ev.packet_size().max(16)),
            u32::from(acl_in.packet_size().max(64)),
        );
        *RX.lock() = Some(Rx {
            events: VecDeque::new(),
            acl: VecDeque::new(),
            event_asm: Assembler::new(Kind::Event),
            acl_asm: Assembler::new(Kind::Acl),
        });
        *EP.lock() = Some(Endpoints {
            dev: d.dev,
            event_dci: ev.dci(),
            event_buf,
            acl_buf,
            event_len,
            acl_len,
        });
        xhci::set_endpoint_handler(d.dev, ev.dci(), on_transfer);
        xhci::set_endpoint_handler(d.dev, acl_in.dci(), on_transfer);
        xhci::queue(d.dev, ev.dci(), event_buf.phys, event_len)?;
        xhci::queue(d.dev, acl_in.dci(), acl_buf.phys, acl_len)?;
        Ok(Self {
            dev: d.dev,
            interface: iface.number,
            out_dci: acl_out.dci(),
            out_buf,
        })
    }

    /// Vendor register read (MediaTek "UHW" access).
    pub fn vendor_read(
        &self,
        request: u8,
        request_type: u8,
        value: u16,
        index: u16,
        buf: &mut [u8],
    ) -> Result<usize, UsbError> {
        xhci::control(
            self.dev,
            Setup {
                request_type,
                request,
                value,
                index,
                length: buf.len() as u16,
            },
            buf,
        )
    }

    /// Vendor register write.
    pub fn vendor_write(
        &self,
        request: u8,
        request_type: u8,
        value: u16,
        index: u16,
        data: &mut [u8],
    ) -> Result<usize, UsbError> {
        xhci::control(
            self.dev,
            Setup {
                request_type,
                request,
                value,
                index,
                length: data.len() as u16,
            },
            data,
        )
    }
}

impl Transport for UsbTransport {
    fn send_command(&mut self, packet: &[u8]) -> Result<(), HostError> {
        let mut data = packet.to_vec();
        let setup = Setup {
            request_type: 0x20,
            request: 0,
            value: 0,
            index: u16::from(self.interface),
            length: data.len() as u16,
        };
        xhci::control(self.dev, setup, &mut data)
            .map(|_| ())
            .map_err(|_| HostError::Transport)
    }

    fn send_acl(&mut self, packet: &[u8]) -> Result<(), HostError> {
        if packet.len() > 4096 {
            return Err(HostError::Malformed);
        }
        // SAFETY: `out_buf` is a 4 KiB DMA page owned by this transport; `packet.len() <= 4096`.
        unsafe { core::ptr::copy_nonoverlapping(packet.as_ptr(), self.out_buf.virt, packet.len()) };
        xhci::transfer(
            self.dev,
            self.out_dci,
            self.out_buf.phys,
            packet.len() as u32,
            1000,
        )
        .map(|_| ())
        .map_err(|_| HostError::Transport)
    }

    fn recv(&mut self, timeout_ms: u64) -> Option<Packet> {
        let deadline = time::now_ns() + timeout_ms * 1_000_000;
        loop {
            {
                let mut g = RX.lock();
                if let Some(rx) = g.as_mut() {
                    if let Some(e) = rx.events.pop_front() {
                        return Some(Packet::Event(e));
                    }
                    if let Some(a) = rx.acl.pop_front() {
                        return Some(Packet::Acl(a));
                    }
                }
            }
            if time::now_ns() >= deadline {
                return None;
            }
            xhci::process_events(usize::from(self.dev.ctrl));
            arch::enable_and_halt();
        }
    }

    fn now_ms(&mut self) -> u64 {
        time::now_ns() / 1_000_000
    }
}

/// Finds Bluetooth controllers among the enumerated USB devices.
pub fn controllers() -> Vec<Device> {
    usb::devices()
        .into_iter()
        .filter(|d| {
            d.config.interfaces.iter().any(|i| {
                i.alt == 0 && i.class == fmt::CLASS_WIRELESS && i.subclass == 1 && i.protocol == 1
            })
        })
        .collect()
}
