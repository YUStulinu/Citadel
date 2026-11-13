//! Networking (filled in with the virtio-net driver and the TCP/IP stack).
pub mod socket;

use kcore::errno::*;

pub fn syscall(_nr: u64, _a: &[u64; 6]) -> KResult<u64> {
    Err(ENOSYS)
}
