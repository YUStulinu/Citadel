//! Hardware-independent parts of the Citadel kernel: everything that can be tested on the
//! host with `cargo test` lives here, and the kernel calls into it.
#![cfg_attr(not(test), no_std)]

pub mod buddy;
pub mod cpio;
pub mod elf;
pub mod errno;
pub mod path;
