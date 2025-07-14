// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

//! Tests for PSCI calls.

use crate::{expect, expect_eq, fail, normal_world_test, secure_world_test, util::log_error};
use aarch64_rt::{Stack, start_core};
use core::{
    hint::spin_loop,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};
use log::debug;
use smccc::{
    Smc,
    psci::{self, AffinityState, LowestAffinityLevel},
};

normal_world_test!(test_psci_version);
fn test_psci_version() -> Result<(), ()> {
    expect_eq!(
        psci::version::<Smc>(),
        Ok(psci::Version { major: 1, minor: 3 })
    );
    Ok(())
}

secure_world_test!(test_psci_version_secure);
fn test_psci_version_secure() -> Result<(), ()> {
    expect_eq!(psci::version::<Smc>(), Err(psci::Error::NotSupported));
    Ok(())
}

normal_world_test!(test_cpu_on_off);
fn test_cpu_on_off() -> Result<(), ()> {
    const SECONDARY_CPU_MPIDR: u64 = 0x0001;
    /// The number of pages to reserve for the secondary CPU stack.
    const SECONDARY_STACK_PAGES: usize = 4;

    #[unsafe(link_section = ".stack")]
    static mut SECONDARY_STACK: Stack<SECONDARY_STACK_PAGES> = Stack::new();

    static SECONDARY_CPU_STARTED: AtomicBool = AtomicBool::new(false);
    static SECONDARY_CPU_ARG: AtomicU64 = AtomicU64::new(0);
    static CPU_OFF_READY: AtomicBool = AtomicBool::new(false);
    static CPU_OFF_FAILED: AtomicBool = AtomicBool::new(false);

    // Reset statics, in case the test gets run a second time somehow.
    SECONDARY_CPU_STARTED.store(false, Ordering::SeqCst);
    SECONDARY_CPU_ARG.store(0, Ordering::SeqCst);
    CPU_OFF_READY.store(false, Ordering::SeqCst);
    CPU_OFF_FAILED.store(false, Ordering::SeqCst);

    extern "C" fn bl33_secondary_cpu_main(arg: u64) -> ! {
        debug!("BL33 secondary CPU started with arg {}.", arg);

        SECONDARY_CPU_ARG.store(arg, Ordering::SeqCst);
        SECONDARY_CPU_STARTED.store(true, Ordering::SeqCst);

        // Wait until the primary CPU is ready for us to turn off.
        while !CPU_OFF_READY.load(Ordering::SeqCst) {
            spin_loop();
        }

        let result = psci::cpu_off::<Smc>();
        CPU_OFF_FAILED.store(true, Ordering::SeqCst);
        panic!("PSCI CPU_OFF returned {:?}", result);
    }

    let secondary_cpu_affinity_info = log_error(
        "PSCI AFFINITY_INFO failed",
        psci::affinity_info::<Smc>(SECONDARY_CPU_MPIDR, LowestAffinityLevel::All),
    )?;
    expect_eq!(secondary_cpu_affinity_info, AffinityState::Off);

    debug!("Calling PSCI cpu_on...");
    // SAFETY: Nothing else is using `SECONDARY_STACK`, and it will remain valid for the duration of
    // the program.
    let result = unsafe {
        start_core::<Smc, SECONDARY_STACK_PAGES>(
            SECONDARY_CPU_MPIDR,
            &raw mut SECONDARY_STACK,
            bl33_secondary_cpu_main,
            42,
        )
    };
    expect_eq!(result, Ok(()));
    debug!("PSCI CPU_ON succeeded");

    // Wait for secondary CPU to start.
    while !SECONDARY_CPU_STARTED.load(Ordering::SeqCst) {
        spin_loop();
    }
    expect_eq!(SECONDARY_CPU_ARG.load(Ordering::SeqCst), 42);

    let secondary_cpu_affinity_info = log_error(
        "PSCI AFFINITY_INFO failed",
        psci::affinity_info::<Smc>(SECONDARY_CPU_MPIDR, LowestAffinityLevel::All),
    )?;
    expect_eq!(secondary_cpu_affinity_info, AffinityState::On);

    // Let secondary CPU know it can turn off again.
    CPU_OFF_READY.store(true, Ordering::SeqCst);

    // Wait for secondary CPU to turn itself off or fail to do so.
    loop {
        expect!(!CPU_OFF_FAILED.load(Ordering::SeqCst));

        let secondary_cpu_affinity_info = log_error(
            "PSCI AFFINITY_INFO failed",
            psci::affinity_info::<Smc>(SECONDARY_CPU_MPIDR, LowestAffinityLevel::All),
        )?;
        match secondary_cpu_affinity_info {
            AffinityState::Off => break,
            AffinityState::On => {}
            AffinityState::OnPending => {
                fail!("Unexpected affinity state ON_PENDING for secondary CPU");
            }
        }

        spin_loop();
    }

    Ok(())
}
