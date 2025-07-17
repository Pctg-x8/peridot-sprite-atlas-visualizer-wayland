#![allow(dead_code)]

pub fn hires_tick() -> u64 {
    let mut ts = core::mem::MaybeUninit::uninit();
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, ts.as_mut_ptr());
        ts.assume_init().tv_sec as u64 * 1_000_000_000 + ts.assume_init().tv_nsec as u64
    }
}

// always nsec frequency
pub fn hires_tick_freq() -> u64 {
    1_000_000_000
}
