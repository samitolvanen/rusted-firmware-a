// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

include!("../../platforms/fvp/config.rs");

use super::{DummyService, Platform};
use crate::{
    aarch64::{dsb_ish, isb, wfi},
    context::{CoresImpl, EntryPointInfo},
    debug::DEBUG,
    gicv3::{GicConfig, InterruptConfig},
    logger::{self, LockedWriter},
    pagetable::{IdMap, MT_DEVICE, map_region},
    services::{
        arch::WorkaroundSupport,
        psci::{
            PlatformPowerStateInterface, PowerStateType, PsciCompositePowerState,
            PsciPlatformInterface, PsciPlatformOptionalFeatures,
        },
    },
    sysregs::{IccSre, MpidrEl1, ScrEl3, Spsr, read_mpidr_el1, read_scr_el3, write_scr_el3},
};
use aarch64_paging::paging::MemoryRegion;
use arm_fvp_base_pac::{
    Peripherals,
    power_controller::{FvpPowerController, FvpPowerControllerRegisters, SystemStatus},
    system::{FvpSystemPeripheral, FvpSystemRegisters, SystemConfigFunction},
};
use arm_gic::{
    IntId, Trigger,
    gicv3::{
        GicV3, Group, SecureIntGroup,
        registers::{Gicd, GicrSgi},
    },
};
use arm_pl011_uart::{PL011Registers, Uart, UniqueMmioPointer};
use arm_psci::{ErrorCode, HwState, Mpidr, PowerState};
use core::{arch::global_asm, mem::offset_of, ptr::NonNull};
use percore::Cores;
use spin::Mutex;

const BLD_GIC_VE_MMAP: u32 = 0x0;

/// Base address of GICv3 distributor.
const BASE_GICD_BASE: usize = 0x2f00_0000;
/// Base address of GICv3 redistributor frame.
const BASE_GICR_BASE: usize = 0x2f10_0000;
const VE_GICD_BASE: usize = 0x2c00_1000;

const V2M_SYSREGS_BASE: usize = 0x1c01_0000;
const V2M_SYS_ID: usize = 0x0;
const V2M_SYS_ID_BLD_SHIFT: u32 = 12;

const DEVICE0_BASE: usize = 0x2000_0000;
const DEVICE0_SIZE: usize = 0x0c20_0000;
const DEVICE1_BASE: usize = BASE_GICD_BASE;
const PLATFORM_CORE_COUNT: usize =
    FVP_CLUSTER_COUNT * FVP_MAX_CPUS_PER_CLUSTER * FVP_MAX_PE_PER_CPU;
const DEVICE1_SIZE: usize = (BASE_GICR_BASE - BASE_GICD_BASE) + (PLATFORM_CORE_COUNT * 0x2_0000);

const ARM_TRUSTED_SRAM_BASE: usize = 0x0400_0000;
const ARM_SHARED_RAM_BASE: usize = ARM_TRUSTED_SRAM_BASE;
const ARM_SHARED_RAM_SIZE: usize = 0x0000_1000; /* 4 KB */

const V2M_IOFPGA_BASE: usize = 0x1c00_0000;
const V2M_IOFPGA_SIZE: usize = 0x0300_0000;

const SHARED_RAM: MemoryRegion = MemoryRegion::new(
    ARM_SHARED_RAM_BASE,
    ARM_SHARED_RAM_BASE + ARM_SHARED_RAM_SIZE,
);

const V2M_MAP_IOFPGA: MemoryRegion =
    MemoryRegion::new(V2M_IOFPGA_BASE, V2M_IOFPGA_BASE + V2M_IOFPGA_SIZE);

const DEVICE0: MemoryRegion = MemoryRegion::new(DEVICE0_BASE, DEVICE0_BASE + DEVICE0_SIZE);
const DEVICE1: MemoryRegion = MemoryRegion::new(DEVICE1_BASE, DEVICE1_BASE + DEVICE1_SIZE);

// Base address of the primary PL011 UART.
const PL011_BASE_ADDRESS: *mut PL011Registers = 0x1C09_0000 as _;

const V2M_IOFPGA_UART1_BASE: usize = 0x1c0a_0000;

// TODO: These addresses should be parsed from FW_CONFIG
/// The physical address of the SPMC manifest blob.
const TOS_FW_CONFIG_ADDRESS: u64 = 0x0400_1500;
const NT_FW_CONFIG_ADDRESS: u64 = 0x8000_0000;
const HW_CONFIG_ADDRESS: u64 = 0;
const HW_CONFIG_ADDRESS_NS: u64 = 0x8200_0000; // TODO: this is a copy of the one in secure memory

// TODO: Use the correct values here (see services/std_svc/rmmd/rmmd_main.c).
/// Version of the RMM Boot Interface.
const RMM_BOOT_VERSION: u64 = 0;
/// Base address for the EL3 - RMM shared area. The boot manifest should be stored at the beginning
/// of this area.
const RMM_SHARED_AREA_BASE_ADDRESS: u64 = 0;

/// Secure timers' interrupt IDs.
const SEL2_TIMER_ID: IntId = IntId::ppi(4);
const SEL1_TIMER_ID: IntId = IntId::ppi(13);

/// Fixed Virtual Platform
pub struct Fvp;

impl Platform for Fvp {
    const CORE_COUNT: usize = PLATFORM_CORE_COUNT;
    const CACHE_WRITEBACK_GRANULE: usize = 1 << 6;

    type LogSinkImpl = LockedWriter<Uart<'static>>;
    type PsciPlatformImpl = FvpPsciPlatformImpl<'static>;

    type PlatformServiceImpl = DummyService;

    const GIC_CONFIG: GicConfig = GicConfig {
        interrupts_config: &[
            (
                SEL2_TIMER_ID,
                InterruptConfig {
                    priority: 0x80,
                    group: Group::Secure(SecureIntGroup::Group1S),
                    trigger: Trigger::Level,
                },
            ),
            (
                SEL1_TIMER_ID,
                InterruptConfig {
                    priority: 0x80,
                    group: Group::Secure(SecureIntGroup::Group1S),
                    trigger: Trigger::Level,
                },
            ),
        ],
    };

    fn init_before_mmu() {
        // SAFETY: `PL011_BASE_ADDRESS` is the base address of a PL011 device, and nothing else
        // accesses that address range. The address remains valid after turning on the MMU
        // because of the identity mapping of the `V2M_MAP_IOFPGA` region.
        let uart_pointer =
            unsafe { UniqueMmioPointer::new(NonNull::new(PL011_BASE_ADDRESS).unwrap()) };
        logger::init(LockedWriter::new(Uart::new(uart_pointer)))
            .expect("Failed to initialise logger");
    }

    fn map_extra_regions(idmap: &mut IdMap) {
        map_region(idmap, &SHARED_RAM, MT_DEVICE);
        map_region(idmap, &V2M_MAP_IOFPGA, MT_DEVICE);
        map_region(idmap, &DEVICE0, MT_DEVICE);
        map_region(idmap, &DEVICE1, MT_DEVICE);
    }

    unsafe fn create_gic() -> GicV3<'static> {
        // SAFETY: `GICD_BASE_ADDRESS` and `GICR_BASE_ADDRESS` are base addresses of a GIC device,
        // and nothing else accesses that address range.
        // TODO: Powering on-off secondary cores will also access their GIC Redistributors.
        unsafe {
            GicV3::new(
                BASE_GICD_BASE as *mut Gicd,
                BASE_GICR_BASE as *mut GicrSgi,
                Fvp::CORE_COUNT,
                false,
            )
        }
    }

    fn create_service() -> Self::PlatformServiceImpl {
        DummyService
    }

    fn handle_group0_interrupt(int_id: IntId) {
        todo!("Handle group0 interrupt {:?}", int_id)
    }

    fn secure_entry_point() -> EntryPointInfo {
        let core_linear_id = CoresImpl::core_index() as u64;
        EntryPointInfo {
            pc: 0x0600_0000,
            #[cfg(feature = "sel2")]
            spsr: Spsr::D | Spsr::A | Spsr::I | Spsr::F | Spsr::M_AARCH64_EL2H,
            #[cfg(not(feature = "sel2"))]
            spsr: Spsr::D | Spsr::A | Spsr::I | Spsr::F | Spsr::M_AARCH64_EL1H,
            args: [
                TOS_FW_CONFIG_ADDRESS,
                HW_CONFIG_ADDRESS,
                0,
                0,
                core_linear_id,
                0,
                0,
                0,
            ],
        }
    }

    fn non_secure_entry_point() -> EntryPointInfo {
        EntryPointInfo {
            pc: 0x8800_0000,
            spsr: Spsr::D | Spsr::A | Spsr::I | Spsr::F | Spsr::M_AARCH64_EL2H,
            args: [NT_FW_CONFIG_ADDRESS, HW_CONFIG_ADDRESS_NS, 0, 0, 0, 0, 0, 0],
        }
    }

    #[cfg(feature = "rme")]
    fn realm_entry_point() -> EntryPointInfo {
        let core_linear_id = CoresImpl::core_index() as u64;
        EntryPointInfo {
            pc: 0xfdc0_0000,
            spsr: Spsr::D | Spsr::A | Spsr::I | Spsr::F | Spsr::M_AARCH64_EL2H,
            args: [
                core_linear_id,
                RMM_BOOT_VERSION,
                Self::CORE_COUNT as u64,
                RMM_SHARED_AREA_BASE_ADDRESS,
                0,
                0,
                0,
                0,
            ],
        }
    }

    fn mpidr_is_valid(mpidr: MpidrEl1) -> bool {
        if mpidr.mt() {
            mpidr.aff3() == 0
                && usize::from(mpidr.aff2()) < FVP_CLUSTER_COUNT
                && usize::from(mpidr.aff1()) < FVP_MAX_CPUS_PER_CLUSTER
                && usize::from(mpidr.aff0()) < FVP_MAX_PE_PER_CPU
        } else {
            mpidr.aff3() == 0
                && mpidr.aff2() == 0
                && usize::from(mpidr.aff1()) < FVP_CLUSTER_COUNT
                && usize::from(mpidr.aff0()) < FVP_MAX_CPUS_PER_CLUSTER
        }
    }

    fn psci_platform() -> Option<Self::PsciPlatformImpl> {
        let peripherals = Peripherals::take().unwrap();

        let power_controller_address =
            peripherals.power_controller.pa() as *mut FvpPowerControllerRegisters;

        // SAFETY: `power_controller_address` is the base address of a power controller device, and
        // nothing else accesses that address range. The address remains valid after turning on the
        // MMU because of the identity mapping of the `V2M_MAP_IOFPGA` region.
        let power_controller_pointer =
            unsafe { UniqueMmioPointer::new(NonNull::new(power_controller_address).unwrap()) };

        let system_regs_address = peripherals.system.pa() as *mut FvpSystemRegisters;

        // SAFETY: `system_regs_address` is the base address of a system registers, and nothing
        // else accesses that address range. The address remains valid after turning on the
        // MMU because of the identity mapping of the `V2M_MAP_IOFPGA` region.
        let system_regs_pointer =
            unsafe { UniqueMmioPointer::new(NonNull::new(system_regs_address).unwrap()) };

        Some(FvpPsciPlatformImpl {
            power_controller: Mutex::new(FvpPowerController::new(power_controller_pointer)),
            system: Mutex::new(FvpSystemPeripheral::new(system_regs_pointer)),
        })
    }

    fn arch_workaround_1_supported() -> WorkaroundSupport {
        WorkaroundSupport::SafeButNotRequired
    }

    fn arch_workaround_1() {}

    fn arch_workaround_2_supported() -> WorkaroundSupport {
        WorkaroundSupport::SafeButNotRequired
    }

    fn arch_workaround_2() {}

    fn arch_workaround_3_supported() -> WorkaroundSupport {
        WorkaroundSupport::SafeButNotRequired
    }

    fn arch_workaround_3() {}

    fn arch_workaround_4_supported() -> WorkaroundSupport {
        WorkaroundSupport::SafeButNotRequired
    }
}

#[derive(PartialEq, PartialOrd, Debug, Eq, Ord, Clone, Copy)]
pub enum FvpPowerState {
    Run = 0,
    Retention = 1,
    Off = 2,
}

impl PlatformPowerStateInterface for FvpPowerState {
    const OFF: Self = Self::Off;
    const RUN: Self = Self::Run;

    fn power_state_type(&self) -> PowerStateType {
        match self {
            Self::Run => PowerStateType::Run,
            Self::Retention => PowerStateType::StandbyOrRetention,
            Self::Off => PowerStateType::PowerDown,
        }
    }
}

impl From<FvpPowerState> for usize {
    fn from(_value: FvpPowerState) -> Self {
        todo!()
    }
}

unsafe extern "C" {
    pub unsafe fn bl31_warm_entrypoint();
}

pub struct FvpPsciPlatformImpl<'a> {
    power_controller: Mutex<FvpPowerController<'a>>,
    system: Mutex<FvpSystemPeripheral<'a>>,
}

impl FvpPsciPlatformImpl<'_> {
    const CLUSTER_POWER_LEVEL: usize = 1;

    fn cluster_off(&self, mpidr: u32) {
        // TODO: fvp_interconnect_disable();
        self.power_controller.lock().power_off_cluster(mpidr);
    }

    fn power_domain_on_finish_common(&self, target_state: &PsciCompositePowerState) {
        assert_eq!(target_state.cpu_level_state(), FvpPowerState::Off);

        let mpidr = read_mpidr_el1().bits() as u32;

        // Perform the common cluster specific operations.
        if target_state.states[Self::CLUSTER_POWER_LEVEL] == FvpPowerState::Off {
            // This CPU might have woken up whilst the cluster was attempting to power down. In
            // this case the FVP power controller will have a pending cluster power off request
            // which needs to be cleared by writing to the PPONR register. This prevents the power
            // controller from interpreting a subsequent entry of this cpu into a simple wfi as a
            // power down request.
            self.power_controller.lock().power_on_processor(mpidr);

            /* Enable coherency if this cluster was off */
            // TODO: fvp_interconnect_enable();
        }

        // Perform the common system specific operations.
        if target_state.highest_level_state() == FvpPowerState::Off {
            self.restore_system_power_domain();
        }

        // Clear PWKUPR.WEN bit to ensure interrupts do not interfere with a cpu power down unless
        // the bit is set again.
        self.power_controller.lock().disable_wakeup_requests(mpidr);
    }

    fn enable_gic_cpu_interface(&self) {
        // TODO: implement enable_gic_cpu_interface
    }
    fn disable_gic_cpu_interface(&self) {
        // TODO: implement disable_gic_cpu_interface
    }

    fn enable_gic_redistributor(&self) {
        // TODO: implement enable_gic_redistributor
    }

    fn disable_gic_redistributor(&self) {
        // TODO: implement disable_gic_redistributor
    }

    fn save_system_power_domain(&self) {
        // TODO: implement save_system_power_domain
        // plat_arm_gic_save();

        // Unregister console now so that it is not registered for a second time during resume.
        // arm_console_runtime_end();

        // All the other peripheral which are configured by ARM TF are re-initialized on resume
        // from system suspend. Hence we don't save their state here.
    }

    fn restore_system_power_domain(&self) {
        // TODO: implement restore_system_power_domain
        // plat_arm_gic_resume();
        // plat_arm_security_setup();
        // arm_configure_sys_timer();
    }
}

const _: () = assert!(
    (FVP_CLUSTER_COUNT > 0) && (FVP_CLUSTER_COUNT <= 256),
    "Invalid FVP cluster count"
);

impl PsciPlatformInterface for FvpPsciPlatformImpl<'_> {
    const POWER_DOMAIN_COUNT: usize =
        1 + FVP_CLUSTER_COUNT + FVP_CLUSTER_COUNT * FVP_MAX_CPUS_PER_CLUSTER;
    const MAX_POWER_LEVEL: usize = 2;

    const FEATURES: PsciPlatformOptionalFeatures = PsciPlatformOptionalFeatures::empty();

    type PlatformPowerState = FvpPowerState;

    fn topology() -> &'static [usize] {
        const TOPOLOGY: [usize; 2 + FVP_CLUSTER_COUNT] = {
            let mut topology = [0; 2 + FVP_CLUSTER_COUNT];

            topology[0] = 1;
            topology[1] = FVP_CLUSTER_COUNT;

            let mut i = 0;
            loop {
                if i >= FVP_CLUSTER_COUNT {
                    break;
                }
                topology[i + 2] = FVP_MAX_CPUS_PER_CLUSTER;
                i += 1;
            }
            topology
        };

        &TOPOLOGY
    }

    /// Based on 6.5 Recommended StateID Encoding
    fn try_parse_power_state(power_state: PowerState) -> Option<PsciCompositePowerState> {
        let states = match power_state {
            PowerState::StandbyOrRetention(0x01) => [
                FvpPowerState::Retention,
                FvpPowerState::Run,
                FvpPowerState::Run,
            ],
            PowerState::PowerDown(0x02) => {
                [FvpPowerState::Off, FvpPowerState::Run, FvpPowerState::Run]
            }
            PowerState::PowerDown(0x22) => {
                [FvpPowerState::Off, FvpPowerState::Off, FvpPowerState::Run]
            }
            PowerState::PowerDown(0x222) => {
                [FvpPowerState::Off, FvpPowerState::Off, FvpPowerState::Off]
            }
            _ => return None,
        };

        Some(PsciCompositePowerState { states })
    }

    fn cpu_standby(&self, cpu_state: FvpPowerState) {
        let scr = read_scr_el3();

        assert!(cpu_state.power_state_type() == PowerStateType::StandbyOrRetention);

        // Enable non-secure interrupts to wake the CPU. In GICv3 affinity routing mode, the
        // non-secure Group 1 interrupts use Physical FIQ.
        write_scr_el3(scr | ScrEl3::FIQ);
        isb();

        // Enter standby state. DSB is good practice before using WFI to enter low power states.
        dsb_ish();
        wfi();

        write_scr_el3(scr);
    }

    fn power_domain_suspend(&self, target_state: &PsciCompositePowerState) {
        // FVP has retention only at cpu level. Just return as nothing is to be done for retention.
        if target_state.cpu_level_state() == FvpPowerState::Retention {
            return;
        }

        assert_eq!(target_state.cpu_level_state(), FvpPowerState::Off);

        let mpidr = read_mpidr_el1().bits() as u32;

        self.power_controller.lock().enable_wakeup_requests(mpidr);

        // Prevent interrupts from spuriously waking up this cpu.
        self.disable_gic_cpu_interface();

        // The Redistributor is not powered off as it can potentially prevent wake up events
        // reaching the CPUIF and/or might lead to losing register context.

        if target_state.states[Self::CLUSTER_POWER_LEVEL] == FvpPowerState::Off {
            self.cluster_off(mpidr);
        }

        // Perform the common system specific operations.
        if target_state.highest_level_state() == FvpPowerState::Off {
            self.save_system_power_domain();
        }

        self.power_controller.lock().power_off_processor(mpidr);
    }

    fn power_domain_suspend_finish(&self, target_state: &PsciCompositePowerState) {
        // Nothing to be done on waking up from retention at CPU level.
        if target_state.cpu_level_state() == FvpPowerState::Retention {
            return;
        }

        self.power_domain_on_finish_common(target_state);
        self.enable_gic_cpu_interface();
    }

    fn power_domain_off(&self, target_state: &PsciCompositePowerState) {
        assert_eq!(FvpPowerState::Off, target_state.cpu_level_state());

        self.disable_gic_cpu_interface();
        self.disable_gic_redistributor();

        let mpidr = read_mpidr_el1().bits() as u32;
        self.power_controller.lock().power_off_processor(mpidr);

        if target_state.states[Self::CLUSTER_POWER_LEVEL] == FvpPowerState::Off {
            self.cluster_off(mpidr);
        }
    }

    fn power_domain_on(&self, mpidr: Mpidr) -> Result<(), ErrorCode> {
        // Write warm boot entry point
        unsafe {
            core::ptr::write_volatile(
                ARM_SHARED_RAM_BASE as *mut u64,
                bl31_warm_entrypoint as *const () as u64,
            );
            core::arch::asm!("dsb st");
        }

        let raw_mpidr: u32 = mpidr.try_into().map_err(ErrorCode::from)?;

        // Ensure that we do not cancel an inflight power off request for the
        // target cpu. That would leave it in a zombie wfi. Wait for it to power
        // off and then program the power controller to turn that CPU on.
        loop {
            let psysr = self.power_controller.lock().system_status(raw_mpidr);
            if !psysr.contains(SystemStatus::L0) {
                break;
            }
        }

        self.power_controller.lock().power_on_processor(raw_mpidr);

        Ok(())
    }

    fn power_domain_on_finish(&self, target_state: &PsciCompositePowerState) {
        self.power_domain_on_finish_common(target_state);
        self.enable_gic_redistributor();
        self.enable_gic_cpu_interface();
    }

    fn system_off(&self) -> ! {
        self.system
            .lock()
            .write_system_configuration(SystemConfigFunction::Shutdown);
        wfi();
        unreachable!("expected system off did not happen");
    }

    fn system_reset(&self) -> ! {
        self.system
            .lock()
            .write_system_configuration(SystemConfigFunction::Reboot);
        wfi();
        unreachable!("expected system reset did not happen");
    }

    fn node_hw_state(&self, target_cpu: Mpidr, power_level: u32) -> Result<HwState, ErrorCode> {
        let raw_mpidr: u32 = target_cpu.try_into().map_err(ErrorCode::from)?;

        let status_flag = match power_level as usize {
            PsciCompositePowerState::CPU_POWER_LEVEL => SystemStatus::L0,
            Self::CLUSTER_POWER_LEVEL => {
                // Use L1 affinity if MPIDR_EL1.MT bit is not set else use L2 affinity.
                if raw_mpidr & 0x1 == 0 {
                    SystemStatus::L1
                } else {
                    SystemStatus::L2
                }
            }
            _ => return Err(ErrorCode::InvalidParameters),
        };

        let psysr = self.power_controller.lock().system_status(raw_mpidr);
        Ok(if psysr.contains(status_flag) {
            HwState::On
        } else {
            HwState::Off
        })
    }
}

global_asm!(
    include_str!("../asm_macros_common.S"),
    include_str!("../arm_macros.S"),
    // Calculates core linear index as: ClusterId * FVP_MAX_CPUS_PER_CLUSTER * FVP_MAX_PE_PER_CPU +
    // CPUId * FVP_MAX_PE_PER_CPU + ThreadId
    ".globl plat_calc_core_pos",
    "func plat_calc_core_pos",
        // Check for MT bit in MPIDR. If not set, shift MPIDR to left to make it look as if in a
        // multi-threaded implementation.
        "tst	x0, #{MPIDR_MT_MASK}",
        "lsl	x3, x0, #{MPIDR_AFFINITY_BITS}",
        "csel	x3, x3, x0, eq",

        // Extract individual affinity fields from MPIDR.
        "ubfx	x0, x3, #{MPIDR_AFF0_SHIFT}, #{MPIDR_AFFINITY_BITS}",
        "ubfx	x1, x3, #{MPIDR_AFF1_SHIFT}, #{MPIDR_AFFINITY_BITS}",
        "ubfx	x2, x3, #{MPIDR_AFF2_SHIFT}, #{MPIDR_AFFINITY_BITS}",

        // Compute linear position.
        "mov	x4, #{FVP_MAX_CPUS_PER_CLUSTER}",
        "madd	x1, x2, x4, x1",
        "mov	x5, #{FVP_MAX_PE_PER_CPU}",
        "madd	x0, x1, x5, x0",
        "ret",
    "endfunc plat_calc_core_pos",
    include_str!("fvp/crash_print_regs.S"),
    include_str!("fvp/arm_helpers.S"),
    include_str!("../arm_macros_purge.S"),
    include_str!("../asm_macros_common_purge.S"),
    DEBUG = const DEBUG as i32,
    ICC_SRE_SRE_BIT = const IccSre::SRE.bits(),
    GICD_ISPENDR = const offset_of!(Gicd, ispendr),
    MPIDR_MT_MASK = const MpidrEl1::MT.bits(),
    MPIDR_AFF0_SHIFT = const MpidrEl1::AFF0_SHIFT,
    MPIDR_AFF1_SHIFT = const MpidrEl1::AFF1_SHIFT,
    MPIDR_AFF2_SHIFT = const MpidrEl1::AFF2_SHIFT,
    FVP_MAX_CPUS_PER_CLUSTER = const FVP_MAX_CPUS_PER_CLUSTER,
    MPIDR_AFFINITY_BITS = const MpidrEl1::AFFINITY_BITS,
    FVP_MAX_PE_PER_CPU = const FVP_MAX_PE_PER_CPU,
    V2M_SYSREGS_BASE = const V2M_SYSREGS_BASE,
    V2M_SYS_ID = const V2M_SYS_ID,
    V2M_SYS_ID_BLD_SHIFT = const V2M_SYS_ID_BLD_SHIFT,
    BLD_GIC_VE_MMAP = const BLD_GIC_VE_MMAP,
    BASE_GICD_BASE = const BASE_GICD_BASE,
    VE_GICD_BASE = const VE_GICD_BASE,
    PLAT_ARM_CRASH_UART_BASE = const V2M_IOFPGA_UART1_BASE,
    PLAT_ARM_CRASH_UART_CLK_IN_HZ = const 24_000_000,
    ARM_CONSOLE_BAUDRATE = const 115_200,
);
