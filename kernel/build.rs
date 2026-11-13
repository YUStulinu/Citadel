//! Passes the linker script and generates the 256 interrupt entry stubs (assembly).
use std::fmt::Write;

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-arg-bins=-T{dir}/linker.ld");
    println!("cargo:rerun-if-changed=linker.ld");
    println!("cargo:rerun-if-changed=build.rs");

    // Vectors for which the CPU pushes an error code.
    const ERR: [u32; 10] = [8, 10, 11, 12, 13, 14, 17, 21, 29, 30];
    let mut s = String::new();
    s.push_str(".section .text\n.balign 16\n.global trap_stubs\ntrap_stubs:\n");
    for v in 0..256u32 {
        // Every stub is padded to 16 bytes so that the IDT can compute their addresses.
        writeln!(s, ".balign 16").unwrap();
        if !ERR.contains(&v) {
            writeln!(s, "    push 0").unwrap();
        }
        writeln!(s, "    push {v}").unwrap();
        writeln!(s, "    jmp trap_common").unwrap();
    }
    let out = std::env::var("OUT_DIR").unwrap();
    std::fs::write(format!("{out}/trap_stubs.s"), s).unwrap();
}
