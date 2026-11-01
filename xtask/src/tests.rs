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
    for needle in ["KS_OS 0.1.0 booting", "OS operating"] {
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
    let img = ctx.target_dir().join(format!("ks_os-selftest-{name}.img"));
    crate::write_image(ctx, &format!("selftest={name}"), &img)?;
    let s = qemu::spawn_test(ctx, &img, &format!("t-selftest-{name}"))?;
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
            "cpu: GDT/TSS/IDT loaded, stack guard pages installed",
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

pub fn run_all(ctx: &Ctx, image: &Path) -> Result<()> {
    let mut failed = 0;
    type TestFn = fn(&Ctx, &Path) -> Result<()>;
    let tests: [(&str, TestFn); 8] = [
        ("boot", test_boot_banner),
        ("poweroff-key", test_power_off_key),
        ("poweroff-mouse", test_power_off_mouse),
        ("uefi-setup", test_uefi_setup),
        ("exception-breakpoint", test_exception_breakpoint),
        ("exception-fatal", test_fatal_exceptions),
        ("stack-overflow", test_stack_overflow),
        ("nmi", test_nmi),
    ];
    for (name, t) in tests {
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
