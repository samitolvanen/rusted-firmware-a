// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

//! Delay timer implementation.

use crate::{
    platform::{Platform, PlatformImpl},
};
use arm_sysregs::{read_cntpct_el0, write_cntfrq_el0};
use core::time::Duration;
use log::info;

const MICROSECONDS_PER_SECOND: u128 = 1_000_000;

/// Initializes the delay timer for the current core.
///
/// This function must be called before any other function in this module on a given core.
/// It sets the generic timer frequency and stores it for future calculations.
pub fn timer_init() {
    write_cntfrq_el0(PlatformImpl::SYS_COUNTER_FREQ_IN_HZ);
    info!(
        "Generic delay timer initialized with frequency {} Hz",
        PlatformImpl::SYS_COUNTER_FREQ_IN_HZ
    );
}

/// A number of clock ticks.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ClockTick(u64);

impl From<Duration> for ClockTick {
    fn from(delay: Duration) -> Self {
        // Calculate the number of timer ticks required for the delay, rounding up
        // to ensure the delay is at least as long as requested.
        // Use u128 for the multiplication to prevent overflow.
        // Add an extra tick to avoid delaying less than requested.
        let ticks_to_wait = (delay.as_micros()
            * u128::from(PlatformImpl::SYS_COUNTER_FREQ_IN_HZ))
        .div_ceil(MICROSECONDS_PER_SECOND) as u64
            + 1;
        Self(ticks_to_wait)
    }
}

/// Represents a timer that has been set for a specific duration.
///
/// Creating an instance of this struct starts the countdown. The timer's
/// expiration can be checked in a non-blocking way or awaited in a blocking
/// manner.
pub struct LogicalTimer {
    /// CNTPCT_EL0 value at the time of starting the timer
    start_time: u64,
    /// Number of ticks since the start time for the expiry
    ticks_to_wait: ClockTick,
}

impl LogicalTimer {
    /// Creates a new timer that expires after delay `delay`.
    pub fn new(delay: Duration) -> Self {
        Self {
            start_time: read_cntpct_el0(),
            ticks_to_wait: ClockTick::from(delay),
        }
    }

    /// Checks if the timer has expired.
    ///
    /// This is a non-blocking check that correctly handles counter wrap-around.
    pub fn expired(&self) -> bool {
        let current_time = read_cntpct_el0();
        // This subtraction handles the wrap-around case correctly.
        let elapsed_ticks = current_time.wrapping_sub(self.start_time);
        elapsed_ticks >= self.ticks_to_wait.0
    }

    /// Waits for the timer to expire.
    ///
    /// This is a blocking busy-wait.
    pub fn busy_wait(&self) {
        while !self.expired() {
            core::hint::spin_loop();
        }
    }
}
