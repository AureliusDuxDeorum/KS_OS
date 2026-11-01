//! USB HID class driver: keyboards and relative pointing devices feed the unified input queue.
//!
//! Report layouts are never assumed: the HID report descriptor is fetched, parsed by the fuzzed
//! `ksparse::hid`, and every incoming report is decoded against it.

use super::{xhci, Device, DeviceRef, UsbError};
use crate::dma::Dma;
use crate::input;
use crate::sync::IrqMutex;
use alloc::vec::Vec;
use ksparse::hid::{self, ReportDescriptor};
use ksparse::usb::{self as fmt, Interface, Setup};

struct Bound {
    dev: DeviceRef,
    dci: u8,
    buf: Dma,
    report_len: usize,
    desc: ReportDescriptor,
    keyboard: bool,
    pointer: bool,
    tracker: input::Tracker,
    errors: u32,
}

static BOUND: IrqMutex<Vec<Bound>> = IrqMutex::new(Vec::new());

/// Number of bound USB HID interfaces (keyboards and pointers).
pub fn bound_count() -> usize {
    BOUND.lock().len()
}

/// True if a USB pointing device is bound.
pub fn has_pointer() -> bool {
    BOUND.lock().iter().any(|b| b.pointer)
}

/// Count of HID reports decoded (diagnostics and tests).
pub fn reports_seen() -> usize {
    REPORTS.load(core::sync::atomic::Ordering::Relaxed)
}

static REPORTS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Binds a HID interface if it is a keyboard or a relative pointer.
pub fn attach(d: &Device, iface: &Interface) -> Result<(), UsbError> {
    let ep = iface
        .endpoints
        .iter()
        .find(|e| e.is_in() && e.kind() == fmt::TransferKind::Interrupt)
        .copied()
        .ok_or(UsbError::Unsupported(
            "HID interface without an interrupt IN endpoint",
        ))?;
    let len = usize::from(iface.hid_report_len.unwrap_or(0));
    if len == 0 || len > 4096 {
        return Err(UsbError::Parse("HID report descriptor length"));
    }
    let mut raw = alloc::vec![0u8; len];
    let n = xhci::control(
        d.dev,
        Setup {
            request_type: 0x81,
            request: fmt::REQ_GET_DESCRIPTOR,
            value: u16::from(fmt::DT_REPORT) << 8,
            index: u16::from(iface.number),
            length: len as u16,
        },
        &mut raw,
    )?;
    let desc = hid::parse(&raw[..n]);
    let (keyboard, pointer) = (hid::is_keyboard(&desc), hid::is_pointer(&desc));
    let relative = desc.fields.iter().any(|f| {
        f.usage_page == hid::PAGE_GENERIC_DESKTOP && f.usages.contains(&hid::USAGE_X) && f.relative
    });
    if !keyboard && !(pointer && relative) {
        return Err(UsbError::Unsupported(if pointer {
            "absolute pointer (tablet)"
        } else {
            "HID device that is neither keyboard nor relative pointer"
        }));
    }
    let report_len = (hid::input_report_len(&desc, 0).max(hid::input_report_len(&desc, 1))
        + usize::from(desc.uses_report_ids))
    .clamp(1, 64);

    xhci::configure_endpoints(
        d.dev,
        &[xhci::EndpointConfig {
            dci: ep.dci(),
            kind: ep.kind(),
            is_in: true,
            max_packet: ep.packet_size(),
            max_burst: ep.max_burst,
            interval: ep.interval,
        }],
        None,
    )?;
    // SET_IDLE(0): report only on change. Many devices stall this; that is harmless.
    let _ = xhci::control(
        d.dev,
        Setup {
            request_type: 0x21,
            request: fmt::REQ_HID_SET_IDLE,
            value: 0,
            index: u16::from(iface.number),
            length: 0,
        },
        &mut [],
    );

    let buf = xhci::alloc_buffer(d.dev, 4096).ok_or(UsbError::OutOfMemory)?;
    xhci::set_endpoint_handler(d.dev, ep.dci(), on_report);
    BOUND.lock().push(Bound {
        dev: d.dev,
        dci: ep.dci(),
        buf,
        report_len,
        desc,
        keyboard,
        pointer,
        tracker: input::Tracker::default(),
        errors: 0,
    });
    kprintln!(
        "usb: HID {} bound on slot {} interface {} (endpoint {:#04x}, {report_len}-byte reports)",
        match (keyboard, pointer) {
            (true, true) => "keyboard+pointer",
            (true, false) => "keyboard",
            _ => "pointer",
        },
        d.dev.slot,
        iface.number,
        ep.address
    );
    xhci::queue(d.dev, ep.dci(), buf.phys, report_len as u32)
}

/// Endpoint completion handler (runs in interrupt context): decode, publish events, requeue.
fn on_report(dev: DeviceRef, dci: u8, code: u8, residual: u32) {
    let (phys, len, requeue) = {
        let mut g = BOUND.lock();
        let Some(b) = g.iter_mut().find(|b| b.dev == dev && b.dci == dci) else {
            return;
        };
        if code == xhci::CC_SUCCESS || code == xhci::CC_SHORT_PACKET {
            b.errors = 0;
            let got = b.report_len.saturating_sub(residual as usize);
            let report = &b.buf.as_slice()[..got.min(64)];
            let decoded = hid::interpret(&b.desc, report);
            REPORTS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            b.tracker.publish(&decoded, b.keyboard, b.pointer);
        } else {
            b.errors += 1;
        }
        // Give up after repeated errors (e.g. the device was unplugged) rather than storm.
        (b.buf.phys, b.report_len as u32, b.errors < 8)
    };
    if requeue {
        let _ = xhci::queue(dev, dci, phys, len);
    }
}
