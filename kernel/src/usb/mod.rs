// SPDX-License-Identifier: Apache-2.0
//! USB core: device enumeration, the device registry and class-driver binding.
//!
//! The xHCI driver ([`xhci`]) moves data; this module walks the topology, reads descriptors
//! (parsed by the fuzzed `ksparse::usb`), and hands interfaces to class drivers ([`hid`], [`hub`]).
//! Other drivers (Bluetooth, storage) find their device through [`find_device`].

pub mod hid;
pub mod hub;
pub mod xhci;

use crate::boot::BootInfo;
use crate::sync::IrqMutex;
use crate::{pci, time};
use alloc::vec::Vec;
use ksparse::usb::{self as fmt, ConfigDescriptor, DeviceDescriptor, Setup};

/// Identifies an addressed device: controller index and slot id.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DeviceRef {
    pub ctrl: u8,
    pub slot: u8,
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum UsbError {
    Timeout,
    Stall,
    Transfer(u8),
    Command(u8),
    NoSlot,
    NoDevice,
    Busy,
    OutOfMemory,
    Unsupported(&'static str),
    Controller(&'static str),
    Parse(&'static str),
}

/// An enumerated device and everything its descriptors told us.
#[derive(Clone)]
pub struct Device {
    pub dev: DeviceRef,
    pub speed: u8,
    pub descriptor: DeviceDescriptor,
    pub config: ConfigDescriptor,
    /// `(hub, port)` this device hangs off; `None` for root-port devices.
    pub parent: Option<(DeviceRef, u8)>,
}

static DEVICES: IrqMutex<Vec<Device>> = IrqMutex::new(Vec::new());

/// Snapshot of every enumerated device.
pub fn devices() -> Vec<Device> {
    DEVICES.lock().clone()
}

/// Finds the first device with the given vendor/product ID (used by the Bluetooth driver).
#[allow(dead_code)]
pub fn find_device(vendor: u16, product: u16) -> Option<Device> {
    DEVICES
        .lock()
        .iter()
        .find(|d| d.descriptor.vendor == vendor && d.descriptor.product == product)
        .cloned()
}

/// Finds the first device exposing an interface of the given class/subclass/protocol.
pub fn find_interface(class: u8, subclass: u8, protocol: u8) -> Option<(Device, fmt::Interface)> {
    DEVICES.lock().iter().find_map(|d| {
        d.config
            .interfaces
            .iter()
            .find(|i| {
                i.alt == 0 && i.class == class && i.subclass == subclass && i.protocol == protocol
            })
            .map(|i| (d.clone(), i.clone()))
    })
}

fn speed_name(speed: u8) -> &'static str {
    match speed {
        fmt::SPEED_LOW => "low-speed",
        fmt::SPEED_FULL => "full-speed",
        fmt::SPEED_HIGH => "high-speed",
        fmt::SPEED_SUPER => "SuperSpeed",
        _ => "unknown-speed",
    }
}

/// Reads a descriptor into a fresh buffer.
fn get_descriptor(dev: DeviceRef, kind: u8, index: u8, len: usize) -> Result<Vec<u8>, UsbError> {
    let mut buf = alloc::vec![0u8; len];
    let n = xhci::control(
        dev,
        Setup::get_descriptor(kind, index, len as u16),
        &mut buf,
    )?;
    buf.truncate(n);
    Ok(buf)
}

/// Brings up a device that has just been addressed: descriptors, configuration, class drivers.
fn setup_device(
    at: xhci::Attach,
    dev: DeviceRef,
    parent: Option<(DeviceRef, u8)>,
) -> Result<(), UsbError> {
    // The first 8 bytes tell us the real EP0 packet size.
    let head = get_descriptor(dev, fmt::DT_DEVICE, 0, 8)?;
    let mps0 = head
        .get(7)
        .and_then(|&f| fmt::ep0_packet_size(at.speed, f))
        .ok_or(UsbError::Parse("bad bMaxPacketSize0"))?;
    if mps0 != fmt::default_ep0_packet_size(at.speed) {
        xhci::set_ep0_packet_size(dev, mps0)?;
    }
    let descriptor = fmt::parse_device(&get_descriptor(dev, fmt::DT_DEVICE, 0, 18)?)
        .ok_or(UsbError::Parse("device descriptor"))?;
    let cfg_head = get_descriptor(dev, fmt::DT_CONFIG, 0, 9)?;
    let total = usize::from(u16::from_le_bytes([
        *cfg_head.get(2).ok_or(UsbError::Parse("config header"))?,
        *cfg_head.get(3).ok_or(UsbError::Parse("config header"))?,
    ]));
    let config = fmt::parse_config(&get_descriptor(
        dev,
        fmt::DT_CONFIG,
        0,
        total.clamp(9, 4096),
    )?)
    .ok_or(UsbError::Parse("configuration descriptor"))?;
    xhci::control(dev, Setup::set_configuration(config.value), &mut [])?;
    kprintln!(
        "usb: xhci{} slot {} {} {:04x}:{:04x} class {:02x} ({} interface(s)){}",
        dev.ctrl,
        dev.slot,
        speed_name(at.speed),
        descriptor.vendor,
        descriptor.product,
        descriptor.class,
        config.interfaces.iter().filter(|i| i.alt == 0).count(),
        parent.map_or(alloc::string::String::new(), |(h, p)| alloc::format!(
            " behind hub slot {} port {p}",
            h.slot
        ))
    );
    let device = Device {
        dev,
        speed: at.speed,
        descriptor,
        config,
        parent,
    };
    DEVICES.lock().push(device.clone());
    bind_drivers(&device);
    Ok(())
}

fn bind_drivers(d: &Device) {
    let is_hub = d.descriptor.class == fmt::CLASS_HUB
        || d.config
            .interfaces
            .iter()
            .any(|i| i.class == fmt::CLASS_HUB);
    if is_hub {
        if let Err(e) = hub::attach(d) {
            kprintln!("usb: hub on slot {} failed: {e:?}", d.dev.slot);
        }
        return;
    }
    for iface in d
        .config
        .interfaces
        .iter()
        .filter(|i| i.alt == 0 && i.class == fmt::CLASS_HID)
    {
        if let Err(e) = hid::attach(d, iface) {
            kprintln!(
                "usb: HID interface {} on slot {} not bound: {e:?}",
                iface.number,
                d.dev.slot
            );
        }
    }
}

/// Enumerates a device that appeared on an already-reset port.
pub(crate) fn enumerate(
    ctrl: usize,
    at: xhci::Attach,
    parent: Option<(DeviceRef, u8)>,
) -> Result<DeviceRef, UsbError> {
    let dev = xhci::address_device(ctrl, at)?;
    if let Err(e) = setup_device(at, dev, parent) {
        kprintln!("usb: enumeration of slot {} failed: {e:?}", dev.slot);
        return Err(e);
    }
    Ok(dev)
}

/// Enumerates every device attached to the root ports of controller `ctrl`.
fn scan_root(ctrl: usize) {
    let ports = xhci::port_count(ctrl);
    for p in 1..=ports {
        xhci::power_port(ctrl, p);
    }
    time::sleep_ms(150); // power-good plus connect debounce
    for p in 1..=ports {
        let v = xhci::port_status(ctrl, p);
        xhci::clear_port_changes(ctrl, p);
        if !xhci::portsc_connected(v) {
            continue;
        }
        let v = if xhci::portsc_enabled(v) {
            v
        } else {
            match xhci::reset_port(ctrl, p) {
                Ok(v) => v,
                Err(e) => {
                    kprintln!("usb: xhci{ctrl} port {p}: reset failed: {e:?}");
                    continue;
                }
            }
        };
        let speed = xhci::portsc_speed(v);
        let at = xhci::Attach {
            speed,
            root_port: p,
            route: 0,
            depth: 0,
            tt: None,
        };
        let _ = enumerate(ctrl, at, None); // errors are logged by `enumerate`
    }
}

/// Initialises every xHCI controller found on the PCI bus and enumerates attached devices.
pub fn init(boot: &BootInfo) {
    for dev in pci::devices()
        .iter()
        .filter(|d| d.header.class == 0x0C && d.header.subclass == 0x03 && d.header.prog_if == 0x30)
    {
        match xhci::init(boot, dev) {
            Ok(ctrl) => scan_root(ctrl),
            Err(e) => kprintln!("usb: xHCI at {} failed to initialise: {e:?}", dev.addr),
        }
    }
    kprintln!("usb: {} device(s) enumerated", DEVICES.lock().len());
}
