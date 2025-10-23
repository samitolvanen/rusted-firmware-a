// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use crate::naked_asm;

macro_rules! select_platform {
    (platform = $condition:literal, $mod:ident::$sub:ident::$plat_impl:ident) => {
        #[cfg(platform = $condition)]
        mod $mod;

        #[cfg(platform = $condition)]
        pub use $mod::$sub::{CPU_OPS, $plat_impl as PlatformImpl};
    };
    (platform = $condition:literal, $mod:ident::$plat_impl:ident) => {
        #[cfg(platform = $condition)]
        mod $mod;

        #[cfg(platform = $condition)]
        pub use $mod::{CPU_OPS, $plat_impl as PlatformImpl};
    };
    (test, $mod:ident::$plat_impl:ident) => {
        #[cfg(test)]
        mod $mod;

        #[cfg(test)]
        pub use $mod::{CPU_OPS, $plat_impl as PlatformImpl};
    };
}

select_platform!(platform = "fvp", fvp::Fvp);
select_platform!(platform = "qemu", qemu::Qemu);
select_platform!(test, test::TestPlatform);

use crate::{
    context::EntryPointInfo,
    cpu_extensions::CpuExtension,
    gicv3::{self, Gic},
    logger::LogSink,
    pagetable::IdMap,
    services::{
        Service, arch::WorkaroundSupport, psci::PsciPlatformInterface, trng::TrngPlatformInterface,
    },
    smccc::FunctionId,
};
use arm_gic::IntId;
use arm_sysregs::MpidrEl1;
#[cfg(not(test))]
pub use percore::exception_free;
#[cfg(test)]
pub use test::exception_free;

/// For platforms that do not want to implement any custom SMC handlers.
pub struct DummyService;

impl Service for DummyService {
    fn owns(&self, _function: FunctionId) -> bool {
        // Does not own any function id.
        false
    }
}

/// Type alias for convenience, to avoid having to use the complicated type name everywhere.
pub type LogSinkImpl = <PlatformImpl as Platform>::LogSinkImpl;

pub type PsciPlatformImpl = <PlatformImpl as Platform>::PsciPlatformImpl;
pub type TrngPlatformImpl = <PlatformImpl as Platform>::TrngPlatformImpl;
pub type PlatformPowerState = <PsciPlatformImpl as PsciPlatformInterface>::PlatformPowerState;

pub type PlatformServiceImpl = <PlatformImpl as Platform>::PlatformServiceImpl;

/// The hooks implemented by all platforms.
///
/// # Safety
///
/// The implementation of `core_position` must be a naked function which doesn't use the stack, and
/// only clobbers registers x0-x5. For any valid MPIDR value it must always return an index less than
/// `CORE_COUNT`, and must return a different index for different MPIDR values.
///
/// The implementations of `cold_boot_handler`, `crash_console_init`, `crash_console_putc` and
/// `crash_console_flush` must be naked functions which doesn't use the stack, and only clobber the
/// registers they are documented to clobber.
///
/// (These requirements don't apply to the test platform, as it is only used in unit tests.)
pub unsafe trait Platform {
    /// The number of CPU cores.
    const CORE_COUNT: usize;

    /// The size in bytes of the largest cache line across all the cache levels in the platform.
    const CACHE_WRITEBACK_GRANULE: usize;

    /// The GIC configuration.
    const GIC_CONFIG: gicv3::GicConfig;

    /// The CPU extensions enabled by this platform.
    const CPU_EXTENSIONS: &'static [&'static dyn CpuExtension];

    /// The number of pages to reserve for the page heap.
    const PAGE_HEAP_PAGE_COUNT: usize = 5;

    /// Platform dependent LogSink implementation type for Logger.
    type LogSinkImpl: LogSink;

    /// Platform dependent PsciPlatformInterface implementation type.
    type PsciPlatformImpl: PsciPlatformInterface;

    /// Platform dependent TrngPlatformInterface implementation type.
    type TrngPlatformImpl: TrngPlatformInterface;

    /// Service that handles platform-specific SMC calls.
    type PlatformServiceImpl: Service;

    /// Initialises the logger and anything else the platform needs.
    fn init();

    /// Maps device memory and any other regions specific to the platform, before the MMU is
    /// enabled.
    fn map_extra_regions(idmap: &mut IdMap);

    /// Creates instance of GIC driver.
    ///
    /// # Safety
    ///
    /// This must only be called once, to avoid creating aliases of the GIC driver.
    unsafe fn create_gic() -> Gic<'static>;

    /// Creates instance of PlatformServiceImpl.
    ///
    /// This is used for dispatching platform-specific SMCs.
    ///
    /// TODO: provide default implementation with DummyService
    /// once associated type defaults become stable
    /// see issue #29661 <https://github.com/rust-lang/rust/issues/29661>
    fn create_service() -> Self::PlatformServiceImpl;

    /// Handles a Group 0 interrupt.
    ///
    /// Interrupt with id `int_id` has already been acknowledged at this point
    /// and platform-independent code will set EOI after this function returns.
    fn handle_group0_interrupt(int_id: IntId);

    /// Returns the entry point for the secure world, i.e. BL32.
    fn secure_entry_point() -> EntryPointInfo;

    /// Returns the entry point for the non-secure world, i.e. BL33.
    fn non_secure_entry_point() -> EntryPointInfo;

    /// Returns the entry point for the realm world.
    #[cfg(feature = "rme")]
    fn realm_entry_point() -> EntryPointInfo;

    /// Returns whether the given MPIDR is valid for this platform.
    fn mpidr_is_valid(mpidr: MpidrEl1) -> bool;

    /// Returns an option with a PSCI platform implementation handle. The function should only be
    /// called once, when it returns `Some`. All subsequent calls must return `None`.
    fn psci_platform() -> Option<Self::PsciPlatformImpl>;

    /// Returns whether this platform supports the arch WORKAROUND_1 SMC.
    fn arch_workaround_1_supported() -> WorkaroundSupport;

    /// If safe and necessary, performs the workaround specified for the WORKAROUND_1 SMC.
    fn arch_workaround_1();

    /// Returns whether this platform supports the arch WORKAROUND_2 SMC.
    fn arch_workaround_2_supported() -> WorkaroundSupport;

    /// If safe and necessary, performs the workaround specified for the WORKAROUND_2 SMC.
    fn arch_workaround_2();

    /// Returns whether this platform supports the arch WORKAROUND_3 SMC.
    fn arch_workaround_3_supported() -> WorkaroundSupport;

    /// If safe and necessary, performs the workaround specified for the WORKAROUND_3 SMC.
    fn arch_workaround_3();

    /// Returns whether this platform supports the arch WORKAROUND_4 SMC.
    fn arch_workaround_4_supported() -> WorkaroundSupport;

    /// Given a valid MPIDR value, returns the corresponding linear core index.
    ///
    /// The implementation must never return the same index for two different valid MPIDR values,
    /// and must never return a value greater than or equal to the corresponding
    /// `Platform::CORE_COUNT`.
    ///
    /// For an invalid MPIDR value no guarantees are made about the return value.
    extern "C" fn core_position(mpidr: u64) -> usize;

    /// Performs platform-specific initialisation on early cold boot before enabling MMU and
    /// running Rust code.
    ///
    /// # Safety
    ///
    /// This should only be called once during cold boot, after the BSS has been zeroed but before
    /// any Rust code runs.
    #[cfg_attr(test, allow(unused))]
    unsafe extern "C" fn cold_boot_handler();

    /// Initialises the crash console to print a crash report.
    ///
    /// This may be called without a Rust runtime, e.g. with no stack.
    ///
    /// May clobber x0-x2.
    #[cfg_attr(test, allow(unused))]
    extern "C" fn crash_console_init() -> u32;

    /// Prints a character on the crash console.
    ///
    /// This may be called without a Rust runtime, e.g. with no stack.
    ///
    /// May clobber x1-x2.
    #[cfg_attr(test, allow(unused))]
    extern "C" fn crash_console_putc(char: u32) -> i32;

    /// Forces a write of all buffered data that hasn't been output.
    ///
    /// This may be called without a Rust runtime, e.g. with no stack.
    ///
    /// May clobber x0-x1.
    #[cfg_attr(test, allow(unused))]
    extern "C" fn crash_console_flush();
}

#[cfg(all(target_arch = "aarch64", not(test)))]
mod asm {
    use super::*;
    use crate::debug::DEBUG;
    use crate::naked_asm;
    use core::arch::global_asm;

    global_asm!(
        include_str!("asm_macros_common.S"),
        include_str!("platform_helpers.S"),
        include_str!("asm_macros_common_purge.S"),
        DEBUG = const DEBUG as i32,
    );

    /// Uses `PlatformImpl::core_position` to get the index of the calling CPU.
    ///
    /// Clobbers x0-x5.
    #[unsafe(naked)]
    #[unsafe(no_mangle)]
    extern "C" fn plat_my_core_pos() -> usize {
        naked_asm!(
            "mrs	x0, mpidr_el1",
            "b	{core_position}",
            core_position = sym PlatformImpl::core_position,
        );
    }
}

#[unsafe(naked)]
pub unsafe extern "C" fn plat_init_mmu_early() {
    naked_asm!(
        // Disable MMU and D$
        "mrs x1, sctlr_el3",
        "mov x2, 5", // M=C=0
        "orr x2, x2, 0x80000", // WXN=0
        "bic x1, x1, x2",
        "msr sctlr_el3, x1",
        "isb",

        // Clear TLB
        "tlbi alle3",
        "dsb sy",
        "isb",

        // TCR_EL3
        // T0SZ=25 (39b), IRGN0=11b, ORGN0=11b, SH0=11b
        "mov x1, (25UL << 0) | (3UL << 8) | (3UL << 10) | (3UL << 12)",
        // PS=101b (48b), TBI=1, IPS=01b, TBI0=1
        "mov x2, (1UL << 20) | (5UL << 16)",
        "orr x2, x2, (1UL << 32)",
        "orr x2, x2, (1UL << 37)",
        "orr x1, x1, x2",
        "msr tcr_el3, x1",

        // Set MAIR_EL3
        "mov x1, 0xff00",
        "movk x1, 0xffff, lsl 16",
        "msr mair_el3, x1",

        // Set TTBR0_EL3
        "adr x1, __INIT_PT_START__",
        "mov x2, x1",
        "msr ttbr0_el3, x1",

        // Clear initial page tables
        "mov x3, 3 * 4096",
        "0: stp xzr, xzr, [x2], #16",
        "subs x3, x3, #16",
        "bne 0b",

        "add x2, x1, 4096",
        "add x3, x2, 4096",
        "adr x4, __TEXT_START__",

        // Calculate L1 PT entry address
        "lsr x5, x4, 30",
        "and x6, x5, 0x1ff",
        "lsl x6, x6, 3",
        "add x1, x1, x6",
        "orr x5, x2, 3", // Page table, valid
        "str x5, [x1]",

        // Calculate L2 PT entry address
        "lsr x5, x4, 21",
        "and x6, x5, 0x1ff",
        "lsl x6, x6, 3",
        "add x2, x2, x6",
        "orr x5, x3, 3", // Page table, valid
        "str x5, [x2]",

        // Calculate L3 PT entry address
        "lsr x5, x4, 12",
        "and x6, x5, 0x1ff",
        "lsl x6, x6, 3",
        "add x3, x3, x6",

        // Fill L3 table with page descriptors
        "adr x5, __BL31_END__",
        "orr x4, x4, (1UL << 10)",
        "orr x4, x4, 3", // Page descriptor, valid
        "1: add x6, x4, 4096",
        "stp x4, x6, [x3], #16",
        "add x4, x4, 8192",
        "cmp x4, x5",
        "blt 1b",

        // Enable MMU and D$
        "mrs x1, sctlr_el3",
        "orr x1, x1, 4",
        "orr x1, x1, 1",
        "msr sctlr_el3, x1",
        "isb",

        // Clear I$
        "ic iallu",
        "isb",
        "ret"
    );
}
