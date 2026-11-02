// SPDX-License-Identifier: Apache-2.0
//! Passes the kernel linker script to the linker.
fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    println!("cargo:rustc-link-arg-bins=-T{dir}/linker.ld");
    println!("cargo:rustc-link-arg-bins=-zmax-page-size=0x1000");
    println!("cargo:rerun-if-changed=linker.ld");
}
