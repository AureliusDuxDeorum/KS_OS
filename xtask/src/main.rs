// SPDX-License-Identifier: Apache-2.0
//! Build, image, run and test automation for KS_OS (`cargo xtask <command>`).
//!
//! Commands:
//!   build   [--debug]        compile the kernel
//!   image   [--debug] [--firmware-dir DIR]
//!                            build a bootable UEFI disk image (GPT + FAT ESP + Limine); firmware
//!                            blobs (*.bin, *.bin.zst) in DIR are decompressed and shipped as boot
//!                            modules (e.g. the MediaTek Bluetooth patch files; never committed)
//!   run     [--debug] [--headless]
//!                            boot the image in QEMU with OVMF
//!   test    [--debug]        automated boot, power-off, UEFI-setup, mouse and exception tests in QEMU
//!   release                  build KS_OS-vX.Y.Z/ at the repository root: image, checksums, README and
//!                            the Bluetooth-firmware helper script (no firmware inside)
//!   image-cmdline <args>     build target/ks_os-cmdline.img whose kernel command line is <args>
//!                            (e.g. `selftest=page-fault`)
//!
//! Environment: `QEMU_EXTRA="-d int -D log"` appends QEMU arguments; `OVMF_CODE`/`OVMF_VARS`
//! override firmware discovery.

mod disk;
mod firmware;
mod limine;
mod notices;
mod qemu;
mod release;
mod structure;
mod tests;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub struct Ctx {
    pub root: PathBuf,
    pub release: bool,
    /// Firmware blobs supplied with `--firmware-dir`; shipped to the kernel as boot modules.
    pub firmware: Vec<firmware::Blob>,
}

impl Ctx {
    pub fn target_dir(&self) -> PathBuf {
        self.root.join("target")
    }
    pub fn kernel_elf(&self) -> PathBuf {
        let profile = if self.release { "release" } else { "debug" };
        self.target_dir()
            .join("x86_64-unknown-none")
            .join(profile)
            .join("kernel")
    }
    pub fn image(&self) -> PathBuf {
        self.target_dir().join("ks_os.img")
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a parent dir")
        .to_path_buf()
}

fn build_kernel(ctx: &Ctx) -> Result<()> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(&ctx.root).args([
        "build",
        "--package",
        "kernel",
        "--target",
        "x86_64-unknown-none",
    ]);
    if ctx.release {
        cmd.arg("--release");
    }
    if !cmd.status()?.success() {
        return Err("kernel build failed".into());
    }
    Ok(())
}

fn build_image(ctx: &Ctx) -> Result<PathBuf> {
    build_kernel(ctx)?;
    let out = ctx.image();
    write_image(ctx, "", &out)?;
    println!("image: {}", out.display());
    Ok(out)
}

/// Writes a disk image whose kernel command line is `cmdline` (kernel must already be built).
pub fn write_image(ctx: &Ctx, cmdline: &str, out: &Path) -> Result<()> {
    let limine = limine::fetch(ctx)?;
    let kernel = std::fs::read(ctx.kernel_elf())?;
    let esp = disk::make_esp(&limine, &kernel, cmdline, &ctx.firmware)?;
    std::fs::write(out, disk::wrap_gpt(&esp))?;
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().map(String::as_str) else {
        eprintln!("usage: cargo xtask <build|image|run|test> [--debug] [--headless]");
        return ExitCode::from(2);
    };
    let fw_dir = args
        .iter()
        .position(|a| a == "--firmware-dir")
        .and_then(|i| args.get(i + 1).cloned())
        .or_else(|| std::env::var("KS_FIRMWARE_DIR").ok());
    let firmware = match fw_dir.as_deref().map(|d| firmware::load(Path::new(d))) {
        Some(Ok(f)) => f,
        Some(Err(e)) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
        None => Vec::new(),
    };
    let ctx = Ctx {
        root: workspace_root(),
        release: !args.iter().any(|a| a == "--debug"),
        firmware,
    };
    let headless = args.iter().any(|a| a == "--headless");
    let result = match cmd {
        "build" => build_kernel(&ctx),
        "image" => build_image(&ctx).map(|_| ()),
        "release" => release::run(&ctx),
        "structure" => structure::run(&ctx, args.iter().any(|a| a == "--check")),
        "image-cmdline" => build_kernel(&ctx).and_then(|()| {
            write_image(&ctx, &args[1], &ctx.target_dir().join("ks_os-cmdline.img"))
        }),
        "run" => build_image(&ctx).and_then(|img| qemu::run_interactive(&ctx, &img, headless)),
        "test" => build_image(&ctx).and_then(|img| tests::run_all(&ctx, &img)),
        other => Err(format!("unknown command `{other}`").into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
