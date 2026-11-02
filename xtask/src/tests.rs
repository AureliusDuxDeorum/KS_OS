// SPDX-License-Identifier: Apache-2.0
//! End-to-end tests: boot the real image in QEMU/OVMF and drive it.

use crate::qemu::{self, Session};
use crate::{Ctx, Result};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

fn serial(s: &Session) -> String {
    std::fs::read(&s.serial_log)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default()
}

fn wait_for(s: &Session, needle: &str, timeout: Duration) -> Result<()> {
    let t0 = Instant::now();
    while t0.elapsed() < timeout {
        if serial(s).contains(needle) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!(
        "timed out waiting for `{needle}` on serial; log so far:\n{}",
        serial(s)
    )
    .into())
}

fn monitor(s: &Session, cmd: &str) -> Result<()> {
    let mut sock = UnixStream::connect(&s.monitor_sock)?;
    sock.write_all(cmd.as_bytes())?;
    sock.write_all(b"\n")?;
    std::thread::sleep(Duration::from_millis(250));
    Ok(())
}

fn wait_exit(s: &mut Session, timeout: Duration) -> Result<bool> {
    let t0 = Instant::now();
    while t0.elapsed() < timeout {
        if s.child.try_wait()?.is_some() {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(false)
}

fn boot(ctx: &Ctx, image: &Path, tag: &str) -> Result<Session> {
    let s = qemu::spawn_test(ctx, image, tag)?;
    wait_for(&s, "OS operating", Duration::from_secs(60))?;
    wait_for(&s, "ui: ready", Duration::from_secs(30))?;
    Ok(s)
}

fn parse_rect(log: &str, label: &str) -> Option<(i32, i32, i32, i32)> {
    let line = log
        .lines()
        .find(|l| l.contains(&format!("ui: button {label} ")))?;
    let r = line.split("rect=").nth(1)?.split_whitespace().next()?;
    let v: Vec<i32> = r.split(',').filter_map(|p| p.trim().parse().ok()).collect();
    (v.len() == 4).then(|| (v[0], v[1], v[2], v[3]))
}

fn parse_cursor(log: &str) -> Option<(i32, i32)> {
    let line = log.lines().find(|l| l.contains("ui: cursor start "))?;
    let r = line.split("start ").nth(1)?;
    let v: Vec<i32> = r.split(',').filter_map(|p| p.trim().parse().ok()).collect();
    (v.len() == 2).then(|| (v[0], v[1]))
}

fn kill(s: Session) {
    drop(s); // Drop kills and reaps QEMU
}

fn test_boot_banner(ctx: &Ctx, image: &Path) -> Result<()> {
    let s = boot(ctx, image, "t-boot")?;
    let log = serial(&s);
    kill(s);
    for needle in [
        &format!("KS_OS {} booting", env!("CARGO_PKG_VERSION")),
        "OS operating",
        "0 W^X violations",
        "pmm:",
    ] {
        if !log.contains(needle) {
            return Err(format!("missing `{needle}`").into());
        }
    }
    println!("ok   boot prints \"OS operating\"");
    Ok(())
}

fn test_power_off_key(ctx: &Ctx, image: &Path) -> Result<()> {
    let mut s = boot(ctx, image, "t-poweroff-key")?;
    monitor(&s, "sendkey p")?;
    wait_for(&s, "action: power off", Duration::from_secs(10))?;
    if !wait_exit(&mut s, Duration::from_secs(20))? {
        let log = serial(&s);
        kill(s);
        return Err(format!("QEMU did not power off after the Power off action:\n{log}").into());
    }
    println!("ok   power off (keyboard) shuts the machine down");
    Ok(())
}

fn test_power_off_mouse(ctx: &Ctx, image: &Path) -> Result<()> {
    let mut s = boot(ctx, image, "t-poweroff-mouse")?;
    let log = serial(&s);
    let (x, y, w, h) = parse_rect(&log, "PowerOff").ok_or("no power-off button rect on serial")?;
    let (cx, cy) = parse_cursor(&log).ok_or("no cursor start on serial")?;
    let (tx, ty) = (x + w / 2, y + h / 2);
    // Relative PS/2 motion in modest steps, then a click.
    let (mut dx, mut dy) = (tx - cx, ty - cy);
    while dx != 0 || dy != 0 {
        let sx = dx.clamp(-100, 100);
        let sy = dy.clamp(-100, 100);
        monitor(&s, &format!("mouse_move {sx} {sy}"))?;
        dx -= sx;
        dy -= sy;
    }
    monitor(&s, "mouse_button 1")?;
    monitor(&s, "mouse_button 0")?;
    wait_for(&s, "action: power off", Duration::from_secs(10))?;
    if !wait_exit(&mut s, Duration::from_secs(20))? {
        kill(s);
        return Err("QEMU did not power off after clicking the Power off button".into());
    }
    println!("ok   power off (mouse click) shuts the machine down");
    Ok(())
}

fn test_uefi_setup(ctx: &Ctx, image: &Path) -> Result<()> {
    let mut s = boot(ctx, image, "t-uefi")?;
    monitor(&s, "sendkey u")?;
    wait_for(
        &s,
        "action: reboot into UEFI firmware setup",
        Duration::from_secs(10),
    )?;
    // After the reset the firmware must stop in setup instead of booting the kernel again.
    std::thread::sleep(Duration::from_secs(12));
    if s.child.try_wait()?.is_some() {
        let log = serial(&s);
        return Err(format!("QEMU exited instead of entering firmware setup:\n{log}").into());
    }
    let log = serial(&s);
    if log.matches("ui: ready").count() != 1 {
        kill(s);
        return Err("kernel booted again; firmware setup was not entered".into());
    }
    if log.contains("failed") {
        kill(s);
        return Err(format!("firmware reported a failure:\n{log}").into());
    }
    let shot = ctx.target_dir().join("qemu").join("uefi-setup.ppm");
    monitor(&s, &format!("screendump {}", shot.display()))?;
    std::thread::sleep(Duration::from_millis(500));
    kill(s);
    println!(
        "ok   Boot into UEFI enters firmware setup (screenshot: {})",
        shot.display()
    );
    Ok(())
}

/// Boots a self-test image and returns the serial log once `needle` appears.
fn selftest_log(ctx: &Ctx, name: &str, needle: &str) -> Result<String> {
    selftest_log_with(ctx, name, needle, &[])
}

fn selftest_log_with(ctx: &Ctx, name: &str, needle: &str, extra: &[&str]) -> Result<String> {
    selftest_log_full(ctx, name, "", needle, extra)
}

fn selftest_log_full(
    ctx: &Ctx,
    name: &str,
    cmdline_extra: &str,
    needle: &str,
    extra: &[&str],
) -> Result<String> {
    let img = ctx.target_dir().join(format!("ks_os-selftest-{name}.img"));
    crate::write_image(ctx, &format!("selftest={name} {cmdline_extra}"), &img)?;
    let s = qemu::spawn_test_with(ctx, &img, &format!("t-selftest-{name}"), extra)?;
    let r = wait_for(&s, needle, Duration::from_secs(60));
    let log = serial(&s);
    drop(s);
    let _ = std::fs::remove_file(&img);
    r.map_err(|_| format!("selftest `{name}`: `{needle}` never appeared; log:\n{log}"))?;
    Ok(log)
}

fn expect_all(log: &str, name: &str, needles: &[&str]) -> Result<()> {
    for n in needles {
        if !log.contains(n) {
            return Err(format!("selftest `{name}`: missing `{n}` in log:\n{log}").into());
        }
    }
    Ok(())
}

fn test_exception_breakpoint(ctx: &Ctx, _image: &Path) -> Result<()> {
    let log = selftest_log(ctx, "breakpoint", "selftest: breakpoint returned")?;
    expect_all(
        &log,
        "breakpoint",
        &[
            "exception: #BP breakpoint",
            "cpu: GDT/TSS/IDT loaded",
            "stack guard pages installed",
        ],
    )?;
    println!("ok   #BP is handled and execution resumes");
    Ok(())
}

fn test_fatal_exceptions(ctx: &Ctx, _image: &Path) -> Result<()> {
    let cases: [(&str, &[&str]); 4] = [
        ("divide", &["KERNEL FAULT: #DE divide error"]),
        ("invalid-opcode", &["KERNEL FAULT: #UD invalid opcode"]),
        (
            "general-protection",
            &["KERNEL FAULT: #GP general protection fault"],
        ),
        (
            "page-fault",
            &[
                "KERNEL FAULT: #PF page fault",
                "cr2=0x10",
                "read of non-present page",
            ],
        ),
    ];
    for (name, needles) in cases {
        let log = selftest_log(ctx, name, "rax=")?;
        expect_all(&log, name, needles)?;
    }
    println!("ok   #DE #UD #GP #PF produce a diagnosable fatal fault");
    Ok(())
}

fn test_stack_overflow(ctx: &Ctx, _image: &Path) -> Result<()> {
    let log = selftest_log(ctx, "stack-overflow", "rax=")?;
    expect_all(
        &log,
        "stack-overflow",
        &[
            "KERNEL FAULT: #DF double fault",
            "stack overflow",
            "overflowed: kernel stack",
        ],
    )?;
    println!("ok   kernel stack overflow is caught by the guard page and reported via the IST");
    Ok(())
}

fn test_nmi(ctx: &Ctx, _image: &Path) -> Result<()> {
    // Real NMIs are masked at the LAPIC by the bootloader until the APIC phase configures LINT1,
    // so this exercises the NMI IDT gate and its IST stack through a software `int 2`.
    let log = selftest_log(ctx, "nmi", "selftest: nmi returned")?;
    expect_all(&log, "nmi", &["exception: NMI"])?;
    println!("ok   NMI gate runs on its IST stack and execution resumes");
    Ok(())
}

fn test_memory_selftests(ctx: &Ctx, _image: &Path) -> Result<()> {
    for (name, needle) in [
        ("pmm", "selftest: pmm ok"),
        ("heap", "selftest: heap ok"),
        ("mmio-map", "selftest: mmio-map ok"),
        ("vmm-layout", "selftest: vmm-layout ok"),
    ] {
        let log = selftest_log(ctx, name, needle)?;
        if log.contains("selftest FAILED") {
            return Err(format!("selftest `{name}` reported a failure:\n{log}").into());
        }
    }
    println!("ok   PMM, heap, MMIO mapping and kernel W^X layout self-tests");
    Ok(())
}

fn test_memory_violations(ctx: &Ctx, _image: &Path) -> Result<()> {
    let cases: [(&str, &[&str]); 3] = [
        (
            "write-text",
            &["KERNEL FAULT: #PF", "write protection violation"],
        ),
        (
            "exec-data",
            &[
                "KERNEL FAULT: #PF",
                "instruction fetch from non-executable page",
            ],
        ),
        ("pmm-double-free", &["KERNEL PANIC", "double free"]),
    ];
    for (name, needles) in cases {
        let log = selftest_log(ctx, name, "PANIC").or_else(|_| selftest_log(ctx, name, "rax="))?;
        expect_all(&log, name, needles)?;
    }
    println!("ok   writing kernel code, executing data and double-freeing a frame are all caught");
    Ok(())
}

fn test_interrupts(ctx: &Ctx, _image: &Path) -> Result<()> {
    for (name, needle) in [
        ("irq", "selftest: irq ok"),
        ("timer", "selftest: timer ok"),
        ("msi", "selftest: msi ok"),
        ("madt-parser", "selftest: madt-parser ok"),
    ] {
        let log = selftest_log(ctx, name, needle)?;
        expect_all(&log, name, &["apic: xAPIC enabled", "ioapic:", "time: TSC"])?;
        if log.contains("selftest FAILED") {
            return Err(format!("selftest `{name}` reported a failure:\n{log}").into());
        }
    }
    println!("ok   xAPIC, calibrated timer, IRQ-safe allocation under interrupts, MSI composition, MADT parser");
    Ok(())
}

const PCI_DEVICES: &[&str] = &[
    "-device",
    "edu",
    "-device",
    "qemu-xhci",
    "-device",
    "virtio-net-pci,disable-legacy=on",
];

/// USB topology under test: keyboard, mouse, a hub with a keyboard behind it (no PS/2 controller,
/// so injected input can only reach the USB devices).
const USB_DEVICES: &[&str] = &[
    "-machine",
    "q35,i8042=off",
    "-device",
    "qemu-xhci,id=xhci",
    "-device",
    "usb-kbd,bus=xhci.0",
    "-device",
    "usb-mouse,bus=xhci.0",
    "-device",
    "usb-hub,bus=xhci.0,port=3",
    "-device",
    "usb-kbd,bus=xhci.0,port=3.1",
];

fn usb_disk(ctx: &Ctx) -> Result<std::path::PathBuf> {
    let path = ctx.target_dir().join("usb-test-disk.img");
    let mut disk = vec![0u8; 1 << 20];
    disk[..17].copy_from_slice(b"KSOS-USB-BOT-TEST");
    for (i, b) in disk.iter_mut().enumerate().take(512).skip(17) {
        *b = (i as u8).wrapping_mul(3);
    }
    std::fs::write(&path, disk)?;
    Ok(path)
}

fn test_usb_enumeration(ctx: &Ctx, _image: &Path) -> Result<()> {
    let log = selftest_log_with(ctx, "usb", "selftest: usb ok", USB_DEVICES)?;
    expect_all(
        &log,
        "usb",
        &[
            "xhci0: MSI-X on vector",
            "ps2: no controller or devices present",
            "usb: HID keyboard bound",
            "usb: HID pointer bound",
            "usb: hub on slot",
            "behind hub slot",
            "4 device(s) enumerated",
            "bt: no Bluetooth controller found on USB",
        ],
    )?;
    println!("ok   xHCI: MSI-X, enumeration, hub with a device behind it, HID binding (and a machine with no PS/2)");
    Ok(())
}

fn test_usb_hid(ctx: &Ctx, _image: &Path) -> Result<()> {
    let img = ctx.target_dir().join("ks_os-selftest-usb-hid.img");
    crate::write_image(ctx, "selftest=usb-hid", &img)?;
    let s = qemu::spawn_test_with(ctx, &img, "t-selftest-usb-hid", USB_DEVICES)?;
    wait_for(&s, "selftest: usb-hid ready", Duration::from_secs(60))?;
    monitor(&s, "sendkey p")?;
    monitor(&s, "mouse_move 12 7")?;
    let r = wait_for(&s, "selftest: usb-hid ok", Duration::from_secs(30));
    let log = serial(&s);
    drop(s);
    let _ = std::fs::remove_file(&img);
    r.map_err(|_| format!("USB HID input never arrived; log:\n{log}"))?;
    println!("ok   USB keyboard and mouse input travels interrupt endpoint -> report parser -> input queue");
    Ok(())
}

fn boot_usb_ui(ctx: &Ctx, image: &Path, tag: &str) -> Result<Session> {
    let s = qemu::spawn_test_with(ctx, image, tag, USB_DEVICES)?;
    wait_for(&s, "ui: ready", Duration::from_secs(60))?;
    Ok(s)
}

fn test_usb_ui(ctx: &Ctx, image: &Path) -> Result<()> {
    // Keyboard: the `P` accelerator powers the machine off.
    let mut s = boot_usb_ui(ctx, image, "t-usb-ui-key")?;
    monitor(&s, "sendkey p")?;
    wait_for(&s, "action: power off", Duration::from_secs(10))?;
    if !wait_exit(&mut s, Duration::from_secs(20))? {
        return Err("QEMU did not power off after a USB keyboard press".into());
    }
    // Mouse: move onto the Power off button and click.
    let mut s = boot_usb_ui(ctx, image, "t-usb-ui-mouse")?;
    let log = serial(&s);
    let (x, y, w, h) = parse_rect(&log, "PowerOff").ok_or("no power-off button rect on serial")?;
    let (cx, cy) = parse_cursor(&log).ok_or("no cursor start on serial")?;
    let (mut dx, mut dy) = (x + w / 2 - cx, y + h / 2 - cy);
    while dx != 0 || dy != 0 {
        let (sx, sy) = (dx.clamp(-100, 100), dy.clamp(-100, 100));
        monitor(&s, &format!("mouse_move {sx} {sy}"))?;
        dx -= sx;
        dy -= sy;
    }
    monitor(&s, "mouse_button 1")?;
    monitor(&s, "mouse_button 0")?;
    wait_for(&s, "action: power off", Duration::from_secs(10))?;
    if !wait_exit(&mut s, Duration::from_secs(20))? {
        return Err("QEMU did not power off after a USB mouse click".into());
    }
    println!(
        "ok   the real UI is operable from a USB keyboard and mouse on a machine with no PS/2"
    );
    Ok(())
}

fn test_usb_bulk(ctx: &Ctx, _image: &Path) -> Result<()> {
    let disk = usb_disk(ctx)?;
    let drive = format!("if=none,id=usbdisk,file={},format=raw", disk.display());
    let extra: Vec<&str> = vec![
        "-device",
        "qemu-xhci,id=xhci",
        "-drive",
        &drive,
        "-device",
        "usb-storage,bus=xhci.0,drive=usbdisk",
    ];
    let log = selftest_log_with(ctx, "usb-bot", "selftest: usb-bot ok", &extra)?;
    if log.contains("selftest FAILED") {
        return Err(format!("usb-bot reported a failure:\n{log}").into());
    }
    println!("ok   USB bulk-only mass storage: CBW out, 512-byte data stage in, CSW in (bulk endpoints work)");
    Ok(())
}

fn test_firmware_modules(ctx: &Ctx, _image: &Path) -> Result<()> {
    // A synthetic firmware blob travels: host file -> ESP -> Limine module -> kernel boot info.
    let blob = crate::firmware::Blob {
        name: "TEST_FIRMWARE_0.bin".into(),
        data: (0..1500u32).map(|i| (i * 7) as u8).collect(),
    };
    let fw_ctx = Ctx {
        root: ctx.root.clone(),
        release: ctx.release,
        firmware: vec![blob],
    };
    let img = ctx.target_dir().join("ks_os-selftest-firmware.img");
    crate::write_image(&fw_ctx, "", &img)?;
    let s = qemu::spawn_test(ctx, &img, "t-firmware")?;
    let r = wait_for(&s, "ui: ready", Duration::from_secs(60));
    let log = serial(&s);
    drop(s);
    let _ = std::fs::remove_file(&img);
    r.map_err(|_| format!("boot with a firmware module failed:\n{log}"))?;
    expect_all(
        &log,
        "firmware",
        &["boot: module TEST_FIRMWARE_0.bin (1500 bytes)"],
    )?;
    println!("ok   firmware blobs reach the kernel as Limine boot modules");
    Ok(())
}

fn test_diagnostic_mode(ctx: &Ctx, image: &Path) -> Result<()> {
    // No PS/2 controller and no USB devices at all: the UI must show the live log by itself.
    let s = qemu::spawn_test_with(ctx, image, "t-diagnostic", &["-machine", "q35,i8042=off"])?;
    wait_for(
        &s,
        "ui: no input device; diagnostic log view active",
        Duration::from_secs(60),
    )?;
    let shot = ctx.target_dir().join("qemu").join("diagnostic.ppm");
    monitor(&s, &format!("screendump {}", shot.display()))?;
    std::thread::sleep(Duration::from_millis(500));
    drop(s);
    let ppm = std::fs::read(&shot)?;
    // The header bar of the diagnostic screen is white: a plain black/white screenshot check.
    let white = ppm
        .windows(3)
        .skip(20)
        .step_by(3)
        .take(2000)
        .filter(|p| p == &[255, 255, 255])
        .count();
    if white < 100 {
        return Err("diagnostic screen was not drawn".into());
    }
    println!("ok   with no working input device the UI shows a live diagnostic log");
    Ok(())
}

fn test_rng(ctx: &Ctx, _image: &Path) -> Result<()> {
    let log = selftest_log_with(ctx, "rng", "selftest: rng ok", &["-cpu", "max"])?;
    if log.contains("selftest FAILED") {
        return Err(format!("rng reported a failure:\n{log}").into());
    }
    println!("ok   hardware RNG (RDSEED/RDRAND) is available and well behaved");
    Ok(())
}

fn test_ksparse_host(ctx: &Ctx, _image: &Path) -> Result<()> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = std::process::Command::new(cargo)
        .current_dir(&ctx.root)
        .args(["test", "--quiet", "-p", "ksparse", "-p", "ksbt"])
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "ksparse host tests failed:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
        .into());
    }
    println!("ok   ksparse parser unit, mutation and noise tests (host)");
    Ok(())
}

fn test_pci(ctx: &Ctx, _image: &Path) -> Result<()> {
    let log = selftest_log_with(ctx, "pci", "selftest: pci ok", PCI_DEVICES)?;
    expect_all(
        &log,
        "pci",
        &[
            "pci: configuration via ECAM (MCFG)",
            "1234:11e8 class 00.ff.00 msi",
            "1b36:000d class 0c.03.30 msi-x",
            "1af4:1041 class 02.00.00 msi-x",
            "bar0 Mem64",
            "size=0x4000",
        ],
    )?;
    println!("ok   PCI enumeration finds edu, xHCI and virtio-net with correct classes, BAR sizes and capabilities");
    Ok(())
}

fn test_pci_msi(ctx: &Ctx, _image: &Path) -> Result<()> {
    let log = selftest_log_with(ctx, "pci-edu", "selftest: pci-edu ok", PCI_DEVICES)?;
    expect_all(&log, "pci-edu", &["using MSI", "device computed 5! = 120"])?;
    let log = selftest_log_with(ctx, "pci-msix", "selftest: pci-msix ok", PCI_DEVICES)?;
    expect_all(&log, "pci-msix", &["MSI-X is enabled"])?;
    println!("ok   device MSI delivery, DMA through the PMM's DMA allocator, and MSI-X table programming");
    Ok(())
}

fn test_x2apic(ctx: &Ctx, _image: &Path) -> Result<()> {
    // QEMU's software emulation (TCG) has no x2APIC; this needs hardware virtualisation.
    if !Path::new("/dev/kvm").exists() {
        println!("skip x2APIC mode test (needs /dev/kvm; the x2APIC path is untested in this environment)");
        return Ok(());
    }
    let log = selftest_log_full(
        ctx,
        "irq",
        "apic=x2",
        "selftest: irq ok",
        &["-enable-kvm", "-cpu", "host"],
    )?;
    expect_all(&log, "irq(x2apic)", &["apic: x2APIC enabled"])?;
    println!("ok   x2APIC mode works the same (interrupt delivery, EOI, self-IPI)");
    Ok(())
}

fn test_nmi_real(ctx: &Ctx, image: &Path) -> Result<()> {
    let s = boot(ctx, image, "t-nmi-real")?;
    monitor(&s, "nmi")?;
    wait_for(&s, "exception: NMI", Duration::from_secs(10))?;
    // The kernel keeps running and still takes input interrupts afterwards.
    monitor(&s, "sendkey p")?;
    wait_for(&s, "action: power off", Duration::from_secs(10))?;
    println!("ok   a real NMI (QEMU `nmi`) is delivered through the LAPIC and survived");
    Ok(())
}

/// Boots the `net` self-test with the given emulated NIC against QEMU user-mode networking and a
/// one-shot TCP server on the host (reachable from the guest as the gateway, 10.0.2.2).
fn net_test(ctx: &Ctx, nic: &str, tag: &str) -> Result<String> {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let server = std::thread::spawn(move || -> std::io::Result<Vec<u8>> {
        let (mut c, _) = listener.accept()?;
        c.set_read_timeout(Some(Duration::from_secs(30)))?;
        let mut req = Vec::new();
        let mut b = [0u8; 64];
        while !req.ends_with(b"\n") {
            let n = c.read(&mut b)?;
            if n == 0 {
                break;
            }
            req.extend_from_slice(&b[..n]);
        }
        c.write_all(b"KS-NET-PONG\n")?;
        Ok(req)
    });
    let dev = format!("{nic},netdev=n0");
    let log = selftest_log_full(
        ctx,
        "net",
        &format!(
            "nettest.port={port} nettest.dns=localhost nettest.ping6=fec0::2 nettest.require_irq=1"
        ),
        "selftest: net ok",
        &["-cpu", "max", "-netdev", "user,id=n0", "-device", &dev],
    )
    .map_err(|e| format!("{tag}: {e}"))?;
    let req = server
        .join()
        .map_err(|_| "tcp server thread panicked")?
        .map_err(|e| format!("{tag}: host TCP server: {e}"))?;
    if req != b"KS-NET-PING\n" {
        return Err(format!("{tag}: server received {req:?}").into());
    }
    Ok(log)
}

fn test_cpu_protections(ctx: &Ctx, _image: &Path) -> Result<()> {
    let log = selftest_log_with(
        ctx,
        "cpu-protections",
        "selftest: cpu-protections ok",
        &["-cpu", "max"],
    )?;
    expect_all(
        &log,
        "cpu-protections",
        &["cpu: protections SMEP=true SMAP=true UMIP=true WP=true"],
    )?;
    println!("ok   SMEP, SMAP, UMIP and CR0.WP are enabled");
    Ok(())
}

fn test_kaslr(ctx: &Ctx, _image: &Path) -> Result<()> {
    let base = |tag: &str| -> Result<String> {
        let log = selftest_log_with(ctx, "kaslr", "selftest: kaslr ok", &["-cpu", "max"])
            .map_err(|e| format!("{tag}: {e}"))?;
        log.lines()
            .find_map(|l| l.strip_prefix("selftest: kaslr base "))
            .map(str::to_owned)
            .ok_or_else(|| format!("{tag}: no base in log").into())
    };
    let bases = [base("boot 1")?, base("boot 2")?, base("boot 3")?];
    let distinct = bases
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    if distinct < 2 {
        return Err(format!("kernel base never changed across boots: {bases:?}").into());
    }
    println!(
        "ok   KASLR: the kernel base differs between boots ({} distinct of 3)",
        distinct
    );
    Ok(())
}

fn test_net_virtio(ctx: &Ctx, _image: &Path) -> Result<()> {
    let log = net_test(ctx, "virtio-net-pci,disable-legacy=on", "virtio-net")?;
    expect_all(
        &log,
        "net(virtio)",
        &[
            "net: virtio-net ready",
            "selftest: net lease 10.0.2.15/24 gateway 10.0.2.2",
            "selftest: net icmp ok",
            "selftest: net icmp6 ok",
            "selftest: net tcp ok",
            "selftest: net interrupts=",
        ],
    )?;
    println!("ok   virtio-net: MSI-X interrupts, DHCP, SLAAC IPv6, ICMP/ICMPv6 echo, TCP round trip, ingress filter");
    Ok(())
}

fn test_net_igb(ctx: &Ctx, _image: &Path) -> Result<()> {
    let log = net_test(ctx, "igb", "igb")?;
    expect_all(
        &log,
        "net(igb)",
        &[
            "Intel 82576 (igb) ready",
            "selftest: net lease 10.0.2.15/24 gateway 10.0.2.2",
            "selftest: net icmp ok",
            "selftest: net icmp6 ok",
            "selftest: net tcp ok",
            "selftest: net interrupts=",
        ],
    )?;
    println!(
        "ok   Intel igb/igc driver path (QEMU igb): DHCP lease, ICMP echo and a TCP round trip"
    );
    Ok(())
}

pub fn run_all(ctx: &Ctx, image: &Path) -> Result<()> {
    let mut failed = 0;
    type TestFn = fn(&Ctx, &Path) -> Result<()>;
    let tests: [(&str, TestFn); 27] = [
        ("ksparse", test_ksparse_host),
        ("boot", test_boot_banner),
        ("poweroff-key", test_power_off_key),
        ("poweroff-mouse", test_power_off_mouse),
        ("uefi-setup", test_uefi_setup),
        ("exception-breakpoint", test_exception_breakpoint),
        ("exception-fatal", test_fatal_exceptions),
        ("stack-overflow", test_stack_overflow),
        ("nmi", test_nmi),
        ("interrupts", test_interrupts),
        ("pci", test_pci),
        ("pci-msi", test_pci_msi),
        ("usb", test_usb_enumeration),
        ("usb-hid", test_usb_hid),
        ("firmware-modules", test_firmware_modules),
        ("diagnostic", test_diagnostic_mode),
        ("rng", test_rng),
        ("usb-ui", test_usb_ui),
        ("usb-bulk", test_usb_bulk),
        ("x2apic", test_x2apic),
        ("nmi-real", test_nmi_real),
        ("cpu-protections", test_cpu_protections),
        ("kaslr", test_kaslr),
        ("net-virtio", test_net_virtio),
        ("net-igb", test_net_igb),
        ("memory", test_memory_selftests),
        ("memory-violations", test_memory_violations),
    ];
    // `KS_TEST=net cargo xtask test` runs only the tests whose name contains the filter.
    let filter = std::env::var("KS_TEST").unwrap_or_default();
    for (name, t) in tests {
        if !name.contains(&filter) {
            continue;
        }
        if let Err(e) = t(ctx, image) {
            eprintln!("FAIL {name}: {e}");
            failed += 1;
        }
    }
    if failed > 0 {
        return Err(format!("{failed} test(s) failed").into());
    }
    println!("all tests passed");
    Ok(())
}
