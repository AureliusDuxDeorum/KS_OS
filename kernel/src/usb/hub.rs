//! USB hub class driver (high-speed and below; SuperSpeed hubs are reported as unsupported).

use super::{enumerate, xhci, Device, UsbError};
use crate::time;
use ksparse::usb::{self as fmt, HubPortStatus, Setup};

const REQ_TYPE_HUB_IN: u8 = 0xA0; // device-to-host, class, device
const REQ_TYPE_PORT_IN: u8 = 0xA3; // device-to-host, class, other (port)
const REQ_TYPE_PORT_OUT: u8 = 0x23; // host-to-device, class, other (port)

fn port_status(d: &Device, port: u8) -> Result<HubPortStatus, UsbError> {
    let mut b = [0u8; 4];
    xhci::control(
        d.dev,
        Setup {
            request_type: REQ_TYPE_PORT_IN,
            request: fmt::REQ_GET_STATUS,
            value: 0,
            index: u16::from(port),
            length: 4,
        },
        &mut b,
    )?;
    Ok(HubPortStatus::from_raw(u32::from_le_bytes(b)))
}

fn port_feature(d: &Device, request: u8, feature: u16, port: u8) -> Result<(), UsbError> {
    xhci::control(
        d.dev,
        Setup {
            request_type: REQ_TYPE_PORT_OUT,
            request,
            value: feature,
            index: u16::from(port),
            length: 0,
        },
        &mut [],
    )
    .map(|_| ())
}

/// Configures `d` as a hub, powers its ports and enumerates whatever is plugged in.
pub fn attach(d: &Device) -> Result<(), UsbError> {
    if d.speed >= fmt::SPEED_SUPER {
        return Err(UsbError::Unsupported("SuperSpeed hubs"));
    }
    let mut raw = [0u8; 16];
    let n = xhci::control(
        d.dev,
        Setup {
            request_type: REQ_TYPE_HUB_IN,
            request: fmt::REQ_GET_DESCRIPTOR,
            value: u16::from(fmt::DT_HUB) << 8,
            index: 0,
            length: 16,
        },
        &mut raw,
    )?;
    let hub = fmt::parse_hub(&raw[..n]).ok_or(UsbError::Parse("hub descriptor"))?;
    let think_time = ((hub.characteristics >> 5) & 3) as u8;
    xhci::configure_endpoints(
        d.dev,
        &[],
        Some(xhci::HubConfig {
            ports: hub.ports,
            think_time,
        }),
    )?;
    kprintln!("usb: hub on slot {} with {} port(s)", d.dev.slot, hub.ports);

    for p in 1..=hub.ports {
        port_feature(d, fmt::REQ_SET_FEATURE, fmt::FEATURE_PORT_POWER, p)?;
    }
    time::sleep_ms(u64::from(hub.power_on_good) * 2 + 100);

    let parent = xhci::attach_info(d.dev).ok_or(UsbError::NoDevice)?;
    for p in 1..=hub.ports {
        let st = port_status(d, p)?;
        if !st.connected {
            continue;
        }
        port_feature(d, fmt::REQ_SET_FEATURE, fmt::FEATURE_PORT_RESET, p)?;
        let mut st = st;
        for _ in 0..100 {
            time::sleep_ms(10);
            st = port_status(d, p)?;
            if st.reset_changed && !st.reset {
                break;
            }
        }
        let _ = port_feature(d, fmt::REQ_CLEAR_FEATURE, fmt::FEATURE_C_PORT_RESET, p);
        let _ = port_feature(d, fmt::REQ_CLEAR_FEATURE, fmt::FEATURE_C_PORT_CONNECTION, p);
        if !st.enabled {
            kprintln!(
                "usb: hub slot {} port {p}: did not enable after reset",
                d.dev.slot
            );
            continue;
        }
        let speed = if st.low_speed {
            fmt::SPEED_LOW
        } else if st.high_speed {
            fmt::SPEED_HIGH
        } else {
            fmt::SPEED_FULL
        };
        // Route string: one nibble per hub tier. Full/low-speed devices behind a high-speed
        // hub are reached through its transaction translator.
        let route = parent.route | (u32::from(p.min(15)) << (4 * u32::from(parent.depth)));
        let tt = if speed == fmt::SPEED_HIGH || speed >= fmt::SPEED_SUPER {
            None
        } else if d.speed == fmt::SPEED_HIGH {
            Some((d.dev.slot, p))
        } else {
            parent.tt
        };
        let at = xhci::Attach {
            speed,
            root_port: parent.root_port,
            route,
            depth: parent.depth + 1,
            tt,
        };
        let _ = enumerate(usize::from(d.dev.ctrl), at, Some((d.dev, p)));
    }
    Ok(())
}
