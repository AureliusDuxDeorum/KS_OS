// SPDX-License-Identifier: Apache-2.0
//! HID over GATT Profile (HOGP) client: finds the HID service, reads the report map, maps
//! report characteristics to report IDs, subscribes to input reports and decodes notifications.

use crate::att::uuid;
use crate::gatt::{self, AttLink, Characteristic, Error, PROP_NOTIFY};
use alloc::vec::Vec;
use ksparse::hid::{self, Input, ReportDescriptor};

const REPORT_TYPE_INPUT: u8 = 1;

/// One report characteristic of the HID service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    pub value_handle: u16,
    pub ccc_handle: Option<u16>,
    pub id: u8,
    pub kind: u8,
    pub can_notify: bool,
}

#[derive(Debug, Clone)]
pub struct HogpDevice {
    pub descriptor: ReportDescriptor,
    pub reports: Vec<Report>,
}

fn find_char(chars: &[Characteristic], u: u16) -> Option<&Characteristic> {
    chars.iter().find(|c| c.uuid.short() == Some(u))
}

impl HogpDevice {
    /// Discovers the HID service on a connected, *encrypted* link and subscribes to input reports.
    pub fn setup(link: &mut impl AttLink, mtu: u16) -> Result<Self, Error> {
        let services = gatt::discover_services(link)?;
        let svc = services
            .iter()
            .find(|s| s.uuid.short() == Some(uuid::HID_SERVICE))
            .ok_or(Error::Att(0x0A))?;
        let chars = gatt::discover_characteristics(link, svc)?;

        let map_char = find_char(&chars, uuid::REPORT_MAP).ok_or(Error::Malformed)?;
        let map = gatt::read_long(link, map_char.value_handle, mtu, 4096)?;
        let descriptor = hid::parse(&map);

        // Report mode is the default, but make it explicit if the device has a protocol-mode characteristic.
        if let Some(pm) = find_char(&chars, uuid::PROTOCOL_MODE) {
            let _ = link.request(&crate::att::write_request(pm.value_handle, &[1]));
            // Write Without Response is not required
        }

        let mut reports = Vec::new();
        for (i, c) in chars
            .iter()
            .enumerate()
            .filter(|(_, c)| c.uuid.short() == Some(uuid::REPORT))
        {
            // Descriptors live between this value and the next characteristic declaration.
            let end = chars
                .get(i + 1)
                .map_or(svc.end, |n| n.decl_handle.saturating_sub(1));
            let descs = gatt::discover_descriptors(link, c.value_handle.saturating_add(1), end)
                .unwrap_or_default();
            let mut report = Report {
                value_handle: c.value_handle,
                ccc_handle: None,
                id: 0,
                kind: REPORT_TYPE_INPUT,
                can_notify: c.properties & PROP_NOTIFY != 0,
            };
            for d in descs {
                match d.uuid.short() {
                    Some(uuid::CLIENT_CHARACTERISTIC_CONFIG) => report.ccc_handle = Some(d.handle),
                    Some(uuid::REPORT_REFERENCE) => {
                        let v = gatt::read_long(link, d.handle, mtu, 2)?;
                        if let [id, kind] = v[..] {
                            report.id = id;
                            report.kind = kind;
                        }
                    }
                    _ => {}
                }
            }
            reports.push(report);
        }
        // Subscribe to every input report that can notify.
        for r in reports
            .iter()
            .filter(|r| r.kind == REPORT_TYPE_INPUT && r.can_notify)
        {
            if let Some(ccc) = r.ccc_handle {
                gatt::write(link, ccc, &[0x01, 0x00])?;
            }
        }
        Ok(Self {
            descriptor,
            reports,
        })
    }

    /// Decodes an ATT notification `(handle, value)` into input, if it belongs to an input report.
    pub fn decode(&self, handle: u16, value: &[u8]) -> Option<Input> {
        let r = self
            .reports
            .iter()
            .find(|r| r.value_handle == handle && r.kind == REPORT_TYPE_INPUT)?;
        // GATT report values carry no report-ID byte; the descriptor-based decoder expects one when IDs are in use.
        let mut report = Vec::with_capacity(value.len() + 1);
        if self.descriptor.uses_report_ids {
            report.push(r.id);
        }
        report.extend_from_slice(value);
        Some(hid::interpret(&self.descriptor, &report))
    }

    pub fn is_keyboard(&self) -> bool {
        hid::is_keyboard(&self.descriptor)
    }

    pub fn is_pointer(&self) -> bool {
        hid::is_pointer(&self.descriptor)
    }
}
