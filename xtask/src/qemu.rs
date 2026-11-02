// SPDX-License-Identifier: Apache-2.0
//! QEMU launch helpers.

use crate::{Ctx, Result};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

pub struct Firmware {
    pub code: PathBuf,
    pub vars_template: PathBuf,
}

/// Locates OVMF firmware (override with `OVMF_CODE` / `OVMF_VARS`).
pub fn find_ovmf() -> Result<Firmware> {
    if let (Ok(c), Ok(v)) = (std::env::var("OVMF_CODE"), std::env::var("OVMF_VARS")) {
        return Ok(Firmware {
            code: c.into(),
            vars_template: v.into(),
        });
    }
    const PAIRS: &[(&str, &str)] = &[
        (
            "/usr/share/OVMF/OVMF_CODE_4M.fd",
            "/usr/share/OVMF/OVMF_VARS_4M.fd",
        ),
        (
            "/usr/share/OVMF/OVMF_CODE.fd",
            "/usr/share/OVMF/OVMF_VARS.fd",
        ),
        (
            "/usr/share/edk2/ovmf/OVMF_CODE.fd",
            "/usr/share/edk2/ovmf/OVMF_VARS.fd",
        ),
        (
            "/usr/share/edk2/x64/OVMF_CODE.4m.fd",
            "/usr/share/edk2/x64/OVMF_VARS.4m.fd",
        ),
    ];
    for (c, v) in PAIRS {
        if Path::new(c).exists() && Path::new(v).exists() {
            return Ok(Firmware {
                code: c.into(),
                vars_template: v.into(),
            });
        }
    }
    Err("OVMF not found; install `ovmf` or set OVMF_CODE and OVMF_VARS".into())
}

pub struct Session {
    pub child: Child,
    pub serial_log: PathBuf,
    pub monitor_sock: PathBuf,
}

fn base_command(
    ctx: &Ctx,
    image: &Path,
    tag: &str,
    display_none: bool,
    extra: &[&str],
) -> Result<(Command, Session)> {
    let fw = find_ovmf()?;
    let dir = ctx.target_dir().join("qemu").join(tag);
    std::fs::create_dir_all(&dir)?;
    let vars = dir.join("OVMF_VARS.fd");
    std::fs::copy(&fw.vars_template, &vars)?;
    let serial_log = dir.join("serial.log");
    let monitor_sock = dir.join("monitor.sock");
    let _ = std::fs::remove_file(&serial_log);
    let _ = std::fs::remove_file(&monitor_sock);

    let mut c = Command::new("qemu-system-x86_64");
    c.args(["-machine", "q35", "-m", "256M"]);
    c.arg("-drive").arg(format!(
        "if=pflash,format=raw,unit=0,readonly=on,file={}",
        fw.code.display()
    ));
    c.arg("-drive").arg(format!(
        "if=pflash,format=raw,unit=1,file={}",
        vars.display()
    ));
    c.arg("-drive")
        .arg(format!("format=raw,file={}", image.display()));
    c.args(["-device", "VGA"]);
    c.arg("-serial")
        .arg(format!("file:{}", serial_log.display()));
    c.arg("-monitor")
        .arg(format!("unix:{},server,nowait", monitor_sock.display()));
    c.args(extra);
    // Developer hook: `QEMU_EXTRA="-d int -D target/qemu/int.log" cargo xtask run`.
    if let Ok(extra) = std::env::var("QEMU_EXTRA") {
        c.args(extra.split_whitespace());
    }
    if display_none {
        c.args(["-display", "none"]);
    }
    // The placeholder child is replaced by the caller after spawn.
    let placeholder = Command::new("true").spawn()?;
    Ok((
        c,
        Session {
            child: placeholder,
            serial_log,
            monitor_sock,
        },
    ))
}

impl Drop for Session {
    /// Never leak a QEMU process, whatever way a test ends.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Starts a headless QEMU instance used by tests.
pub fn spawn_test(ctx: &Ctx, image: &Path, tag: &str) -> Result<Session> {
    spawn_test_with(ctx, image, tag, &[])
}

/// Like [`spawn_test`] with extra QEMU arguments (e.g. `-cpu max`).
pub fn spawn_test_with(ctx: &Ctx, image: &Path, tag: &str, extra: &[&str]) -> Result<Session> {
    let (mut c, mut s) = base_command(ctx, image, tag, true, extra)?;
    c.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    let _ = s.child.wait();
    s.child = c
        .spawn()
        .map_err(|e| format!("failed to start qemu-system-x86_64: {e}"))?;
    Ok(s)
}

/// Runs QEMU with the serial console on stdio (interactive development).
pub fn run_interactive(ctx: &Ctx, image: &Path, headless: bool) -> Result<()> {
    let (mut c, mut s) = base_command(ctx, image, "run", headless, &[])?;
    let _ = s.child.wait();
    c.args(["-serial", "mon:stdio"]); // later -serial overrides the file one
    let st = c.status()?;
    if st.success() {
        Ok(())
    } else {
        Err(format!("qemu exited with {st}").into())
    }
}
