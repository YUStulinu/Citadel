//! Sampling profiler (placeholder).
use crate::arch::trap::TrapFrame;
use kcore::errno::*;
pub fn sample(_f: &TrapFrame) {}
pub fn control(_cmd: u64, _arg: u64) -> KResult<u64> {
    Err(ENOSYS)
}
