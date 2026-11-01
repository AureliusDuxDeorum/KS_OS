//! Build, image, run and test automation for KS_OS (`cargo xtask <command>`).
//!
//! Commands:
//!   build   [--debug]        compile the kernel
//!   image   [--debug]        build a bootable UEFI disk image (GPT + FAT ESP + Limine)
//!   run     [--debug] [--headless]
//!                            boot the image in QEMU with OVMF
//!   test    [--debug]        automated boot, power-off, UEFI-setup and mouse tests in QEMU

mod disk;
mod limine;
mod qemu;
mod tests;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub struct Ctx {
    pub root: PathBuf,
    pub release: bool,
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
    let limine = limine::fetch(ctx)?;
    let kernel = std::fs::read(ctx.kernel_elf())?;
    let esp = disk::make_esp(&limine, &kernel)?;
    let img = disk::wrap_gpt(&esp);
    std::fs::write(ctx.image(), img)?;
    println!("image: {}", ctx.image().display());
    Ok(ctx.image())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().map(String::as_str) else {
        eprintln!("usage: cargo xtask <build|image|run|test> [--debug] [--headless]");
        return ExitCode::from(2);
    };
    let ctx = Ctx {
        root: workspace_root(),
        release: !args.iter().any(|a| a == "--debug"),
    };
    let headless = args.iter().any(|a| a == "--headless");
    let result = match cmd {
        "build" => build_kernel(&ctx),
        "image" => build_image(&ctx).map(|_| ()),
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
