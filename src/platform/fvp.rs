// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

mod config;

use self::config::{FVP_CLUSTER_COUNT, FVP_MAX_CPUS_PER_CLUSTER, FVP_MAX_PE_PER_CPU};
use super::{DummyService, Platform};
#[cfg(feature = "rme")]
use crate::services::rmmd::manifest::{
    ManifestList, RmmBootManifest, RmmConsoleInfo, RmmMemoryBank,
};
use crate::{
    aarch64::{dsb_ish, dsb_sy, wfi},
    context::{CoresImpl, EntryPointInfo},
    cpu::{aem_generic::AemGeneric, define_cpu_ops},
    debug::DEBUG,
    gicv3::{Gic, GicConfig, InterruptConfig},
    logger::{self, LockedWriter},
    naked_asm,
    pagetable::{
        IdMap, MT_DEVICE, MT_MEMORY, MT_MEMORY_NS,
        early_pagetable::{EarlyRegion, define_early_mapping},
        map_region,
    },
    platform::CpuExtension,
    services::{
        arch::WorkaroundSupport,
        psci::{
            PlatformPowerStateInterface, PowerStateType, PsciCompositePowerState,
            PsciPlatformInterface, PsciPlatformOptionalFeatures, bl31_warm_entrypoint,
        },
        trng::NotSupportedTrngPlatformImpl,
    },
};
use aarch64_paging::paging::{MemoryRegion, VirtualAddress};
use arm_fvp_base_pac::{
    MemoryMap, Peripherals, PhysicalInstance,
    arm_generic_timer::{CntAcr, CntControlBase, CntCtlBase, GenericTimerControl, GenericTimerCtl},
    power_controller::{FvpPowerController, FvpPowerControllerRegisters, SystemStatus},
    system::{FvpSystemPeripheral, FvpSystemRegisters, SystemConfigFunction},
};
use arm_gic::{
    IntId, Trigger,
    gicv3::{
        GicDistributorContext, GicRedistributorContext, Group, HIGHEST_S_PRIORITY, SecureIntGroup,
        registers::{Gicd, GicrSgi},
    },
};
#[cfg(feature = "rme")]
use arm_gpt::GPIAccessType;
use arm_pl011_uart::{Uart, UniqueMmioPointer};
use arm_psci::{EntryPoint, ErrorCode, HwState, Mpidr, PowerState};
use arm_sysregs::{IccSre, MpidrEl1, Spsr, read_mpidr_el1, write_cntfrq_el0};
#[cfg(feature = "rme")]
use core::ops::Range;
use core::{arch::global_asm, mem::offset_of, ptr::NonNull};
#[cfg(feature = "rme")]
use log::{debug, info};
use percore::Cores;
use spin::mutex::SpinMutex;

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
const DEVICE2_BASE: usize = 0x2a00_0000;
const DEVICE2_SIZE: usize = 0x10000;

const ARM_TRUSTED_SRAM_BASE: usize = 0x0400_0000;
const ARM_TRUSTED_SRAM_SIZE: usize = 0x0008_0000;
const ARM_SHARED_RAM_BASE: usize = ARM_TRUSTED_SRAM_BASE;
const ARM_SHARED_RAM_SIZE: usize = 0x0000_1000; /* 4 KB */

const ARM_GPT_L0_BASE: usize = ARM_TRUSTED_SRAM_BASE + ARM_TRUSTED_SRAM_SIZE - ARM_GPT_L0_SIZE;
const ARM_GPT_L0_SIZE: usize = 0x0000_2000;
const ARM_GPT_L1_BASE: usize = 0xfff0_0000;
const ARM_GPT_L1_SIZE: usize = 0x0010_0000;

const UART_BASE: usize = 0x1c09_0000;
const UART_SIZE: usize = 0x0001_0000;

const WARM_ENTRYPOINT_FIELD: *mut unsafe extern "C" fn() =
    ARM_SHARED_RAM_BASE as *mut unsafe extern "C" fn();

const V2M_IOFPGA_BASE: usize = 0x1c00_0000;
const V2M_IOFPGA_SIZE: usize = 0x0300_0000;

const SHARED_RAM: MemoryRegion = MemoryRegion::new(
    ARM_SHARED_RAM_BASE,
    ARM_SHARED_RAM_BASE + ARM_SHARED_RAM_SIZE,
);

const GPT_L0: MemoryRegion = MemoryRegion::new(ARM_GPT_L0_BASE, ARM_GPT_L0_BASE + ARM_GPT_L0_SIZE);
const GPT_L1: MemoryRegion = MemoryRegion::new(ARM_GPT_L1_BASE, ARM_GPT_L1_BASE + ARM_GPT_L1_SIZE);

const DEVICE_REGIONS: [MemoryRegion; 4] = [
    MemoryRegion::new(V2M_IOFPGA_BASE, V2M_IOFPGA_BASE + V2M_IOFPGA_SIZE),
    MemoryRegion::new(DEVICE0_BASE, DEVICE0_BASE + DEVICE0_SIZE),
    MemoryRegion::new(DEVICE1_BASE, DEVICE1_BASE + DEVICE1_SIZE),
    MemoryRegion::new(DEVICE2_BASE, DEVICE2_BASE + DEVICE2_SIZE),
];

const V2M_IOFPGA_UART1_BASE: usize = 0x1c0a_0000;

// TODO: These addresses should be parsed from FW_CONFIG
/// The physical address of the SPMC manifest blob.
const TOS_FW_CONFIG_ADDRESS: u64 = 0x0400_1500;
const NT_FW_CONFIG_ADDRESS: u64 = 0x8000_0000;
const HW_CONFIG_ADDRESS: u64 = 0x07f0_0000;
const HW_CONFIG_ADDRESS_NS: u64 = 0x8200_0000;

/// Version of the RMM Boot Interface.
#[cfg(feature = "rme")]
const RMM_BOOT_VERSION: u64 = 0x4;

const EARLY_REGIONS: [EarlyRegion; 2] = [
    EarlyRegion {
        address_range: ARM_TRUSTED_SRAM_BASE..(ARM_TRUSTED_SRAM_BASE + ARM_TRUSTED_SRAM_SIZE),
        attributes: MT_MEMORY,
    },
    EarlyRegion {
        address_range: UART_BASE..(UART_BASE + UART_SIZE),
        attributes: MT_DEVICE,
    },
];

define_early_mapping!(EARLY_REGIONS);

const fn secure_sgi_configuration(index: u32) -> (IntId, InterruptConfig) {
    (
        IntId::sgi(index),
        InterruptConfig {
            priority: HIGHEST_S_PRIORITY,
            group: Group::Secure(SecureIntGroup::Group1S),
            trigger: Trigger::Edge,
        },
    )
}

fn device_regions_include<T>(physical_instance: &PhysicalInstance<T>) -> bool {
    let start = physical_instance.pa();
    let end = start + size_of::<T>() - 1;

    DEVICE_REGIONS.iter().any(|region| {
        let range = region.start()..region.end();

        range.contains(&VirtualAddress(start)) && range.contains(&VirtualAddress(end))
    })
}

/// Creates an identity mapped `UniqueMmioPointer` from a `PhysicalInstance`. The function will
/// panic if called with a physical_instance that is not part of the mapped DEVICE_REGIONS.
fn map_peripheral<T>(physical_instance: PhysicalInstance<T>) -> UniqueMmioPointer<'static, T> {
    assert!(device_regions_include(&physical_instance));

    // Safety: Physical instances are unique pointers to peripherals. The addresses remains valid
    // after turning on the MMU because of the identity mapping of the DEVICE_REGIONS.
    unsafe { UniqueMmioPointer::new(NonNull::new(physical_instance.pa() as *mut T).unwrap()) }
}

static FVP_PSCI_PLATFORM_IMPL: SpinMutex<Option<FvpPsciPlatformImpl>> = SpinMutex::new(None);

define_cpu_ops!(AemGeneric);

/// Fixed Virtual Platform
pub struct Fvp;

const ATTESTION_KEY_ECC_SECP384R1: [u8; 48] = [
    0x20, 0x11, 0xC7, 0xF0, 0x3C, 0xEE, 0x43, 0x25, 0x17, 0x6E, 0x52, 0x4F, 0x03, 0x3C, 0x0C, 0xE1,
    0xE2, 0x1A, 0x76, 0xE6, 0xC1, 0xA4, 0xF0, 0xB8, 0x39, 0xAA, 0x1D, 0xF6, 0x1E, 0x0E, 0x8A, 0x5C,
    0x8A, 0x05, 0x74, 0x0F, 0x9B, 0x69, 0xEF, 0xA7, 0xEB, 0x1A, 0x41, 0x85, 0xBD, 0x11, 0x7F, 0x68,
];
const ATTESTATION_TOKEN: [u8; 1518] = [
    0xd2, 0x84, 0x44, 0xa1, 0x01, 0x38, 0x22, 0xa0, 0x59, 0x05, 0x81, 0xa9, 0x19, 0x01, 0x09, 0x78,
    0x23, 0x74, 0x61, 0x67, 0x3a, 0x61, 0x72, 0x6d, 0x2e, 0x63, 0x6f, 0x6d, 0x2c, 0x32, 0x30, 0x32,
    0x33, 0x3a, 0x63, 0x63, 0x61, 0x5f, 0x70, 0x6c, 0x61, 0x74, 0x66, 0x6f, 0x72, 0x6d, 0x23, 0x31,
    0x2e, 0x30, 0x2e, 0x30, 0x0a, 0x58, 0x20, 0x0d, 0x22, 0xe0, 0x8a, 0x98, 0x46, 0x90, 0x58, 0x48,
    0x63, 0x18, 0x28, 0x34, 0x89, 0xbd, 0xb3, 0x6f, 0x09, 0xdb, 0xef, 0xeb, 0x18, 0x64, 0xdf, 0x43,
    0x3f, 0xa6, 0xe5, 0x4e, 0xa2, 0xd7, 0x11, 0x19, 0x09, 0x5c, 0x58, 0x20, 0x7f, 0x45, 0x4c, 0x46,
    0x02, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x3e, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x50, 0x58, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x19, 0x01, 0x00, 0x58,
    0x21, 0x01, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, 0x00, 0x0f, 0x0e, 0x0d, 0x0c, 0x0b, 0x0a,
    0x09, 0x08, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12, 0x11, 0x10, 0x1f, 0x1e, 0x1d, 0x1c, 0x1b, 0x1a,
    0x19, 0x18, 0x19, 0x09, 0x61, 0x44, 0xcf, 0xcf, 0xcf, 0xcf, 0x19, 0x09, 0x5b, 0x19, 0x30, 0x03,
    0x19, 0x09, 0x62, 0x67, 0x73, 0x68, 0x61, 0x2d, 0x32, 0x35, 0x36, 0x19, 0x09, 0x60, 0x78, 0x3a,
    0x68, 0x74, 0x74, 0x70, 0x73, 0x3a, 0x2f, 0x2f, 0x76, 0x65, 0x72, 0x61, 0x69, 0x73, 0x6f, 0x6e,
    0x2e, 0x65, 0x78, 0x61, 0x6d, 0x70, 0x6c, 0x65, 0x2f, 0x2e, 0x77, 0x65, 0x6c, 0x6c, 0x2d, 0x6b,
    0x6e, 0x6f, 0x77, 0x6e, 0x2f, 0x76, 0x65, 0x72, 0x61, 0x69, 0x73, 0x6f, 0x6e, 0x2f, 0x76, 0x65,
    0x72, 0x69, 0x66, 0x69, 0x63, 0x61, 0x74, 0x69, 0x6f, 0x6e, 0x19, 0x09, 0x5f, 0x8d, 0xa4, 0x01,
    0x69, 0x52, 0x53, 0x45, 0x5f, 0x42, 0x4c, 0x31, 0x5f, 0x32, 0x05, 0x58, 0x20, 0x53, 0x78, 0x79,
    0x63, 0x07, 0x53, 0x5d, 0xf3, 0xec, 0x8d, 0x8b, 0x15, 0xa2, 0xe2, 0xdc, 0x56, 0x41, 0x41, 0x9c,
    0x3d, 0x30, 0x60, 0xcf, 0xe3, 0x22, 0x38, 0xc0, 0xfa, 0x97, 0x3f, 0x7a, 0xa3, 0x02, 0x58, 0x20,
    0x9a, 0x27, 0x1f, 0x2a, 0x91, 0x6b, 0x0b, 0x6e, 0xe6, 0xce, 0xcb, 0x24, 0x26, 0xf0, 0xb3, 0x20,
    0x6e, 0xf0, 0x74, 0x57, 0x8b, 0xe5, 0x5d, 0x9b, 0xc9, 0x4f, 0x6f, 0x3f, 0xe3, 0xab, 0x86, 0xaa,
    0x06, 0x67, 0x73, 0x68, 0x61, 0x2d, 0x32, 0x35, 0x36, 0xa4, 0x01, 0x67, 0x52, 0x53, 0x45, 0x5f,
    0x42, 0x4c, 0x32, 0x05, 0x58, 0x20, 0x53, 0x78, 0x79, 0x63, 0x07, 0x53, 0x5d, 0xf3, 0xec, 0x8d,
    0x8b, 0x15, 0xa2, 0xe2, 0xdc, 0x56, 0x41, 0x41, 0x9c, 0x3d, 0x30, 0x60, 0xcf, 0xe3, 0x22, 0x38,
    0xc0, 0xfa, 0x97, 0x3f, 0x7a, 0xa3, 0x02, 0x58, 0x20, 0x53, 0xc2, 0x34, 0xe5, 0xe8, 0x47, 0x2b,
    0x6a, 0xc5, 0x1c, 0x1a, 0xe1, 0xca, 0xb3, 0xfe, 0x06, 0xfa, 0xd0, 0x53, 0xbe, 0xb8, 0xeb, 0xfd,
    0x89, 0x77, 0xb0, 0x10, 0x65, 0x5b, 0xfd, 0xd3, 0xc3, 0x06, 0x67, 0x73, 0x68, 0x61, 0x2d, 0x32,
    0x35, 0x36, 0xa4, 0x01, 0x65, 0x52, 0x53, 0x45, 0x5f, 0x53, 0x05, 0x58, 0x20, 0x53, 0x78, 0x79,
    0x63, 0x07, 0x53, 0x5d, 0xf3, 0xec, 0x8d, 0x8b, 0x15, 0xa2, 0xe2, 0xdc, 0x56, 0x41, 0x41, 0x9c,
    0x3d, 0x30, 0x60, 0xcf, 0xe3, 0x22, 0x38, 0xc0, 0xfa, 0x97, 0x3f, 0x7a, 0xa3, 0x02, 0x58, 0x20,
    0x11, 0x21, 0xcf, 0xcc, 0xd5, 0x91, 0x3f, 0x0a, 0x63, 0xfe, 0xc4, 0x0a, 0x6f, 0xfd, 0x44, 0xea,
    0x64, 0xf9, 0xdc, 0x13, 0x5c, 0x66, 0x63, 0x4b, 0xa0, 0x01, 0xd1, 0x0b, 0xcf, 0x43, 0x02, 0xa2,
    0x06, 0x67, 0x73, 0x68, 0x61, 0x2d, 0x32, 0x35, 0x36, 0xa4, 0x01, 0x66, 0x41, 0x50, 0x5f, 0x42,
    0x4c, 0x31, 0x05, 0x58, 0x20, 0x53, 0x78, 0x79, 0x63, 0x07, 0x53, 0x5d, 0xf3, 0xec, 0x8d, 0x8b,
    0x15, 0xa2, 0xe2, 0xdc, 0x56, 0x41, 0x41, 0x9c, 0x3d, 0x30, 0x60, 0xcf, 0xe3, 0x22, 0x38, 0xc0,
    0xfa, 0x97, 0x3f, 0x7a, 0xa3, 0x02, 0x58, 0x20, 0x15, 0x71, 0xb5, 0xec, 0x78, 0xbd, 0x68, 0x51,
    0x2b, 0xf7, 0x83, 0x0b, 0xb6, 0xa2, 0xa4, 0x4b, 0x20, 0x47, 0xc7, 0xdf, 0x57, 0xbc, 0xe7, 0x9e,
    0xb8, 0xa1, 0xc0, 0xe5, 0xbe, 0xa0, 0xa5, 0x01, 0x06, 0x67, 0x73, 0x68, 0x61, 0x2d, 0x32, 0x35,
    0x36, 0xa4, 0x01, 0x66, 0x41, 0x50, 0x5f, 0x42, 0x4c, 0x32, 0x05, 0x58, 0x20, 0x53, 0x78, 0x79,
    0x63, 0x07, 0x53, 0x5d, 0xf3, 0xec, 0x8d, 0x8b, 0x15, 0xa2, 0xe2, 0xdc, 0x56, 0x41, 0x41, 0x9c,
    0x3d, 0x30, 0x60, 0xcf, 0xe3, 0x22, 0x38, 0xc0, 0xfa, 0x97, 0x3f, 0x7a, 0xa3, 0x02, 0x58, 0x20,
    0x10, 0x15, 0x9b, 0xaf, 0x26, 0x2b, 0x43, 0xa9, 0x2d, 0x95, 0xdb, 0x59, 0xda, 0xe1, 0xf7, 0x2c,
    0x64, 0x51, 0x27, 0x30, 0x16, 0x61, 0xe0, 0xa3, 0xce, 0x4e, 0x38, 0xb2, 0x95, 0xa9, 0x7c, 0x58,
    0x06, 0x67, 0x73, 0x68, 0x61, 0x2d, 0x32, 0x35, 0x36, 0xa4, 0x01, 0x67, 0x53, 0x43, 0x50, 0x5f,
    0x42, 0x4c, 0x31, 0x05, 0x58, 0x20, 0x53, 0x78, 0x79, 0x63, 0x07, 0x53, 0x5d, 0xf3, 0xec, 0x8d,
    0x8b, 0x15, 0xa2, 0xe2, 0xdc, 0x56, 0x41, 0x41, 0x9c, 0x3d, 0x30, 0x60, 0xcf, 0xe3, 0x22, 0x38,
    0xc0, 0xfa, 0x97, 0x3f, 0x7a, 0xa3, 0x02, 0x58, 0x20, 0x10, 0x12, 0x2e, 0x85, 0x6b, 0x3f, 0xcd,
    0x49, 0xf0, 0x63, 0x63, 0x63, 0x17, 0x47, 0x61, 0x49, 0xcb, 0x73, 0x0a, 0x1a, 0xa1, 0xcf, 0xaa,
    0xd8, 0x18, 0x55, 0x2b, 0x72, 0xf5, 0x6d, 0x6f, 0x68, 0x06, 0x67, 0x73, 0x68, 0x61, 0x2d, 0x32,
    0x35, 0x36, 0xa4, 0x01, 0x67, 0x53, 0x43, 0x50, 0x5f, 0x42, 0x4c, 0x32, 0x05, 0x58, 0x20, 0xf1,
    0x4b, 0x49, 0x87, 0x90, 0x4b, 0xcb, 0x58, 0x14, 0xe4, 0x45, 0x9a, 0x05, 0x7e, 0xd4, 0xd2, 0x0f,
    0x58, 0xa6, 0x33, 0x15, 0x22, 0x88, 0xa7, 0x61, 0x21, 0x4d, 0xcd, 0x28, 0x78, 0x0b, 0x56, 0x02,
    0x58, 0x20, 0xaa, 0x67, 0xa1, 0x69, 0xb0, 0xbb, 0xa2, 0x17, 0xaa, 0x0a, 0xa8, 0x8a, 0x65, 0x34,
    0x69, 0x20, 0xc8, 0x4c, 0x42, 0x44, 0x7c, 0x36, 0xba, 0x5f, 0x7e, 0xa6, 0x5f, 0x42, 0x2c, 0x1f,
    0xe5, 0xd8, 0x06, 0x67, 0x73, 0x68, 0x61, 0x2d, 0x32, 0x35, 0x36, 0xa4, 0x01, 0x67, 0x41, 0x50,
    0x5f, 0x42, 0x4c, 0x33, 0x31, 0x05, 0x58, 0x20, 0x53, 0x78, 0x79, 0x63, 0x07, 0x53, 0x5d, 0xf3,
    0xec, 0x8d, 0x8b, 0x15, 0xa2, 0xe2, 0xdc, 0x56, 0x41, 0x41, 0x9c, 0x3d, 0x30, 0x60, 0xcf, 0xe3,
    0x22, 0x38, 0xc0, 0xfa, 0x97, 0x3f, 0x7a, 0xa3, 0x02, 0x58, 0x20, 0x2e, 0x6d, 0x31, 0xa5, 0x98,
    0x3a, 0x91, 0x25, 0x1b, 0xfa, 0xe5, 0xae, 0xfa, 0x1c, 0x0a, 0x19, 0xd8, 0xba, 0x3c, 0xf6, 0x01,
    0xd0, 0xe8, 0xa7, 0x06, 0xb4, 0xcf, 0xa9, 0x66, 0x1a, 0x6b, 0x8a, 0x06, 0x67, 0x73, 0x68, 0x61,
    0x2d, 0x32, 0x35, 0x36, 0xa4, 0x01, 0x63, 0x52, 0x4d, 0x4d, 0x05, 0x58, 0x20, 0x53, 0x78, 0x79,
    0x63, 0x07, 0x53, 0x5d, 0xf3, 0xec, 0x8d, 0x8b, 0x15, 0xa2, 0xe2, 0xdc, 0x56, 0x41, 0x41, 0x9c,
    0x3d, 0x30, 0x60, 0xcf, 0xe3, 0x22, 0x38, 0xc0, 0xfa, 0x97, 0x3f, 0x7a, 0xa3, 0x02, 0x58, 0x20,
    0xa1, 0xfb, 0x50, 0xe6, 0xc8, 0x6f, 0xae, 0x16, 0x79, 0xef, 0x33, 0x51, 0x29, 0x6f, 0xd6, 0x71,
    0x34, 0x11, 0xa0, 0x8c, 0xf8, 0xdd, 0x17, 0x90, 0xa4, 0xfd, 0x05, 0xfa, 0xe8, 0x68, 0x81, 0x64,
    0x06, 0x67, 0x73, 0x68, 0x61, 0x2d, 0x32, 0x35, 0x36, 0xa4, 0x01, 0x69, 0x48, 0x57, 0x5f, 0x43,
    0x4f, 0x4e, 0x46, 0x49, 0x47, 0x05, 0x58, 0x20, 0x53, 0x78, 0x79, 0x63, 0x07, 0x53, 0x5d, 0xf3,
    0xec, 0x8d, 0x8b, 0x15, 0xa2, 0xe2, 0xdc, 0x56, 0x41, 0x41, 0x9c, 0x3d, 0x30, 0x60, 0xcf, 0xe3,
    0x22, 0x38, 0xc0, 0xfa, 0x97, 0x3f, 0x7a, 0xa3, 0x02, 0x58, 0x20, 0x1a, 0x25, 0x24, 0x02, 0x97,
    0x2f, 0x60, 0x57, 0xfa, 0x53, 0xcc, 0x17, 0x2b, 0x52, 0xb9, 0xff, 0xca, 0x69, 0x8e, 0x18, 0x31,
    0x1f, 0xac, 0xd0, 0xf3, 0xb0, 0x6e, 0xca, 0xae, 0xf7, 0x9e, 0x17, 0x06, 0x67, 0x73, 0x68, 0x61,
    0x2d, 0x32, 0x35, 0x36, 0xa4, 0x01, 0x69, 0x46, 0x57, 0x5f, 0x43, 0x4f, 0x4e, 0x46, 0x49, 0x47,
    0x05, 0x58, 0x20, 0x53, 0x78, 0x79, 0x63, 0x07, 0x53, 0x5d, 0xf3, 0xec, 0x8d, 0x8b, 0x15, 0xa2,
    0xe2, 0xdc, 0x56, 0x41, 0x41, 0x9c, 0x3d, 0x30, 0x60, 0xcf, 0xe3, 0x22, 0x38, 0xc0, 0xfa, 0x97,
    0x3f, 0x7a, 0xa3, 0x02, 0x58, 0x20, 0x9a, 0x92, 0xad, 0xbc, 0x0c, 0xee, 0x38, 0xef, 0x65, 0x8c,
    0x71, 0xce, 0x1b, 0x1b, 0xf8, 0xc6, 0x56, 0x68, 0xf1, 0x66, 0xbf, 0xb2, 0x13, 0x64, 0x4c, 0x89,
    0x5c, 0xcb, 0x1a, 0xd0, 0x7a, 0x25, 0x06, 0x67, 0x73, 0x68, 0x61, 0x2d, 0x32, 0x35, 0x36, 0xa4,
    0x01, 0x6c, 0x54, 0x42, 0x5f, 0x46, 0x57, 0x5f, 0x43, 0x4f, 0x4e, 0x46, 0x49, 0x47, 0x05, 0x58,
    0x20, 0x53, 0x78, 0x79, 0x63, 0x07, 0x53, 0x5d, 0xf3, 0xec, 0x8d, 0x8b, 0x15, 0xa2, 0xe2, 0xdc,
    0x56, 0x41, 0x41, 0x9c, 0x3d, 0x30, 0x60, 0xcf, 0xe3, 0x22, 0x38, 0xc0, 0xfa, 0x97, 0x3f, 0x7a,
    0xa3, 0x02, 0x58, 0x20, 0x23, 0x89, 0x03, 0x18, 0x0c, 0xc1, 0x04, 0xec, 0x2c, 0x5d, 0x8b, 0x3f,
    0x20, 0xc5, 0xbc, 0x61, 0xb3, 0x89, 0xec, 0x0a, 0x96, 0x7d, 0xf8, 0xcc, 0x20, 0x8c, 0xdc, 0x7c,
    0xd4, 0x54, 0x17, 0x4f, 0x06, 0x67, 0x73, 0x68, 0x61, 0x2d, 0x32, 0x35, 0x36, 0xa4, 0x01, 0x6d,
    0x53, 0x4f, 0x43, 0x5f, 0x46, 0x57, 0x5f, 0x43, 0x4f, 0x4e, 0x46, 0x49, 0x47, 0x05, 0x58, 0x20,
    0x53, 0x78, 0x79, 0x63, 0x07, 0x53, 0x5d, 0xf3, 0xec, 0x8d, 0x8b, 0x15, 0xa2, 0xe2, 0xdc, 0x56,
    0x41, 0x41, 0x9c, 0x3d, 0x30, 0x60, 0xcf, 0xe3, 0x22, 0x38, 0xc0, 0xfa, 0x97, 0x3f, 0x7a, 0xa3,
    0x02, 0x58, 0x20, 0xe6, 0xc2, 0x1e, 0x8d, 0x26, 0x0f, 0xe7, 0x18, 0x82, 0xde, 0xbd, 0xb3, 0x39,
    0xd2, 0x40, 0x2a, 0x2c, 0xa7, 0x64, 0x85, 0x29, 0xbc, 0x23, 0x03, 0xf4, 0x86, 0x49, 0xbc, 0xe0,
    0x38, 0x00, 0x17, 0x06, 0x67, 0x73, 0x68, 0x61, 0x2d, 0x32, 0x35, 0x36, 0x58, 0x60, 0x31, 0xd0,
    0x4d, 0x52, 0xcc, 0xde, 0x95, 0x2c, 0x1e, 0x32, 0xcb, 0xa1, 0x81, 0x88, 0x5a, 0x40, 0xb8, 0xcc,
    0x38, 0xe0, 0x52, 0x8c, 0x1e, 0x89, 0x58, 0x98, 0x07, 0x64, 0x2a, 0xa5, 0xe3, 0xf2, 0xbc, 0x37,
    0xf9, 0x53, 0x74, 0x50, 0x6b, 0xff, 0x4d, 0x2e, 0x4b, 0xe7, 0x06, 0x3c, 0x4d, 0x72, 0x41, 0x92,
    0x70, 0xc7, 0x22, 0xe8, 0xd4, 0xd9, 0x3e, 0xe8, 0xb6, 0xc9, 0xfa, 0xce, 0x3b, 0x43, 0xc9, 0x76,
    0x1a, 0x49, 0x94, 0x1a, 0xb6, 0xf3, 0x8f, 0xfd, 0xff, 0x49, 0x6a, 0xd4, 0x63, 0xb4, 0xcb, 0xfa,
    0x11, 0xd8, 0x3e, 0x23, 0xe3, 0x1f, 0x7f, 0x62, 0x32, 0x9d, 0xe3, 0x0c, 0x1c, 0xc8,
];

const GPT_REGIONS: &[(Range<usize>, GPIAccessType)] = &[
    (0..0x4000_0000, GPIAccessType::Any),
    (0x4000_0000..0x8000_0000, GPIAccessType::Any),
    (0x4000_0000..0x5000_0000, GPIAccessType::NonSecure),
    (0x8000_0000..0xfc00_0000, GPIAccessType::NonSecure),
    (0xfc00_0000..0xfdc0_0000, GPIAccessType::Secure),
    (0xfdc0_0000..0xffc0_0000, GPIAccessType::Realm),
    (0xffc0_0000..0x1_0000_0000, GPIAccessType::Root),
    (0x8_8000_0000..0x9_0000_0000, GPIAccessType::NonSecure),
    (0x40_0000_0000..0x40_c000_0000, GPIAccessType::NonSecure),
];

// SAFETY: `core_position` is indeed a naked function, doesn't access the stack or any other memory,
// only clobbers x0-x5, and returns a unique core index as long as `FVP_MAX_CPUS_PER_CLUSTER` and
// `FVP_MAX_PE_PER_CPU` are correct.
unsafe impl Platform for Fvp {
    const CORE_COUNT: usize = PLATFORM_CORE_COUNT;
    const CACHE_WRITEBACK_GRANULE: usize = 1 << 6;
    const PAGE_HEAP_PAGE_COUNT: usize = 10;

    #[cfg(feature = "rme")]
    const RMM_SHARED_BUFFER_START: usize = 0xffbf_f000;

    #[cfg(feature = "rme")]
    fn write_attestion_key_ecc_secp384r1(buf: &mut [u8], start_index: usize) -> Result<usize, ()> {
        if start_index >= buf.len() {
            return Err(());
        }

        let range = start_index
            ..ATTESTION_KEY_ECC_SECP384R1
                .len()
                .min(start_index + buf.len());
        let size = range.end - range.start;

        buf[..size].copy_from_slice(&ATTESTION_KEY_ECC_SECP384R1[range]);
        Ok(size)
    }
    #[cfg(feature = "rme")]
    fn write_attestation_token(
        buf: &mut [u8],
        _hash: &[u8],
        start_index: usize,
    ) -> Result<(usize, usize), ()> {
        if start_index >= buf.len() {
            return Err(());
        }

        let hunk_size = buf.len().min(ATTESTATION_TOKEN.len() - start_index);
        let end_index = start_index + hunk_size;

        buf[0..hunk_size].copy_from_slice(&ATTESTATION_TOKEN[start_index..end_index]);

        Ok((hunk_size, ATTESTATION_TOKEN.len() - end_index))
    }

    #[cfg(feature = "rme")]
    fn setup_gpt() {
        use arm_gpt::{GpccConfig, declare_granule_protection};
        use arm_sysregs::rme::{Cacheability, Shareability};
        use core::slice::from_raw_parts_mut;

        let (l0, l1) = unsafe {
            (
                from_raw_parts_mut(ARM_GPT_L0_BASE as *mut u8, ARM_GPT_L0_SIZE),
                from_raw_parts_mut(ARM_GPT_L1_BASE as *mut u8, ARM_GPT_L1_SIZE),
            )
        };

        declare_granule_protection!(GPT, PPS = 40, L0GPTSZ = 30, PGS = 12);

        GPT.init(l0, l1).unwrap();

        debug!("GPT initialized");

        for (range, gpi) in GPT_REGIONS {
            debug!("Mapping range {range:#x?} into GPT with GPI {gpi:?}",);
            GPT.set(range.clone(), *gpi).unwrap();
        }

        debug!("GPT mapping done");

        unsafe {
            GPT.enable(Some(GpccConfig {
                appsaa: false,
                nso: false,
                tbgpcd: false,
                gpcp: false,
                sh: Shareability::Inner,
                orgn: Cacheability::WriteBackAllocate,
                irgn: Cacheability::WriteBackAllocate,
                spad: false,
                nspad: false,
                rlpad: false,
            }))
            .unwrap();
        }

        info!("GPT enabled!");
    }

    #[cfg(feature = "rme")]
    fn rme_prepare_manifest(buf: &mut [u8]) {
        let manifest = RmmBootManifest::new(buf, 2, 1, 0, 0, 0, &[]);

        // TODO: These addresses should be parsed from FW_CONFIG
        manifest.plat_dram.as_slice_mut()[0] = RmmMemoryBank {
            base: NT_FW_CONFIG_ADDRESS,
            size: 0x7c00_0000,
        };
        manifest.plat_dram.as_slice_mut()[1] = RmmMemoryBank {
            base: 0x0008_8000_0000,
            size: 0x0000_8000_0000,
        };

        manifest.plat_console.as_slice_mut()[0] = RmmConsoleInfo {
            // Value from the pl011_uart crate.
            base: 0x1C09_0000,

            // Values from TF-A.
            map_pages: 0x1,
            name: [0x70, 0x6c, 0x30, 0x31, 0x31, 0x0, 0x0, 0x0], // "pl011"
            clk_in_hz: 0x00e1_0000,
            baud_rate: 0x1c200,
            flags: 0,
        };
    }

    type LogSinkImpl = LockedWriter<Uart<'static>>;
    type PsciPlatformImpl = FvpPsciPlatformImpl<'static>;
    // TODO: Implement TRNG for FVP.
    type TrngPlatformImpl = NotSupportedTrngPlatformImpl;

    type PlatformServiceImpl = DummyService;

    const GIC_CONFIG: GicConfig = GicConfig {
        interrupts_config: &[
            secure_sgi_configuration(8),
            secure_sgi_configuration(9),
            secure_sgi_configuration(10),
            secure_sgi_configuration(11),
            secure_sgi_configuration(12),
            secure_sgi_configuration(13),
            secure_sgi_configuration(14),
            secure_sgi_configuration(15),
        ],
    };

    const CPU_EXTENSIONS: &'static [&'static dyn CpuExtension] = &[
        #[cfg(feature = "rme")]
        &crate::cpu_extensions::hcx::Hcx,
        // #[cfg(feature = "rme")]
        // &crate::cpu_extensions::fgt::Fgt,
        #[cfg(feature = "rme")]
        &crate::cpu_extensions::fgt2::Fgt2,
        #[cfg(feature = "rme")]
        &crate::cpu_extensions::tcr2::Tcr2,
    ];

    fn init() {
        let peripherals = Peripherals::take().unwrap();

        let uart_pointer = map_peripheral(peripherals.uart0);

        logger::init(LockedWriter::new(Uart::new(uart_pointer)))
            .expect("Failed to initialise logger");

        let psci_platform = FvpPsciPlatformImpl::new(
            peripherals.power_controller,
            peripherals.system,
            peripherals.refclk_cntcontrol,
            peripherals.ap_refclk_cntctl,
        );

        psci_platform.init_generic_timer();

        *FVP_PSCI_PLATFORM_IMPL.lock() = Some(psci_platform);

        // Write warm boot entry point the shared memory, so secondary cores can pick it up during
        // boot.
        // Safety: WARM_ENTRYPOINT_FIELD points to a valid, writable address.
        unsafe {
            *WARM_ENTRYPOINT_FIELD = bl31_warm_entrypoint;
        }
        dsb_sy();
    }

    fn map_extra_regions(idmap: &mut IdMap) {
        map_region(idmap, &SHARED_RAM, MT_DEVICE);

        map_region(idmap, &GPT_L0, MT_MEMORY);
        map_region(idmap, &GPT_L1, MT_MEMORY_NS);

        for region in &DEVICE_REGIONS {
            map_region(idmap, region, MT_DEVICE);
        }
    }

    unsafe fn create_gic() -> Gic<'static> {
        // Safety: `BASE_GICD_BASE` is a unique pointer to the FVP's GICD register block.
        let gicd =
            unsafe { UniqueMmioPointer::new(NonNull::new(BASE_GICD_BASE as *mut Gicd).unwrap()) };
        let gicr_base = NonNull::new(BASE_GICR_BASE as *mut GicrSgi).unwrap();

        // Safety: `gicr_base` points to a continuously mapped GIC redistributor memory area until
        // the last redistributor block. There are no other references to this address range.
        unsafe { Gic::new(gicd, gicr_base, false) }
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
                Self::RMM_SHARED_BUFFER_START as u64,
                0,
                0,
                0,
                0,
            ],
        }
    }

    fn mpidr_is_valid(mpidr: MpidrEl1) -> bool {
        if mpidr.contains(MpidrEl1::MT) {
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
        FVP_PSCI_PLATFORM_IMPL.lock().take()
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

    /// Calculates core linear index as: ClusterId * FVP_MAX_CPUS_PER_CLUSTER * FVP_MAX_PE_PER_CPU +
    /// CPUId * FVP_MAX_PE_PER_CPU + ThreadId
    #[unsafe(naked)]
    extern "C" fn core_position(mpidr: u64) -> usize {
        naked_asm!(
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
            MPIDR_MT_MASK = const MpidrEl1::MT.bits(),
            MPIDR_AFF0_SHIFT = const MpidrEl1::AFF0_SHIFT,
            MPIDR_AFF1_SHIFT = const MpidrEl1::AFF1_SHIFT,
            MPIDR_AFF2_SHIFT = const MpidrEl1::AFF2_SHIFT,
            FVP_MAX_CPUS_PER_CLUSTER = const FVP_MAX_CPUS_PER_CLUSTER,
            MPIDR_AFFINITY_BITS = const MpidrEl1::AFFINITY_BITS,
            FVP_MAX_PE_PER_CPU = const FVP_MAX_PE_PER_CPU,
        );
    }

    #[unsafe(naked)]
    unsafe extern "C" fn cold_boot_handler() {
        naked_asm!("ret");
    }

    #[unsafe(naked)]
    extern "C" fn crash_console_init() -> u32 {
        naked_asm!(
            include_str!("../asm_macros_common.S"),
            "mov_imm	x0, {PLAT_ARM_CRASH_UART_BASE}",
            "mov_imm	x1, {PLAT_ARM_CRASH_UART_CLK_IN_HZ}",
            "mov_imm	x2, {ARM_CONSOLE_BAUDRATE}",
            "b	console_pl011_core_init",
            include_str!("../asm_macros_common_purge.S"),
            DEBUG = const DEBUG as i32,
            PLAT_ARM_CRASH_UART_BASE = const V2M_IOFPGA_UART1_BASE,
            PLAT_ARM_CRASH_UART_CLK_IN_HZ = const 24_000_000,
            ARM_CONSOLE_BAUDRATE = const 115_200,
        );
    }

    #[unsafe(naked)]
    extern "C" fn crash_console_putc(char: u32) -> i32 {
        naked_asm!(
            include_str!("../asm_macros_common.S"),
            "mov_imm	x1, {PLAT_ARM_CRASH_UART_BASE}",
            "b	console_pl011_core_putc",
            include_str!("../asm_macros_common_purge.S"),
            DEBUG = const DEBUG as i32,
            PLAT_ARM_CRASH_UART_BASE = const V2M_IOFPGA_UART1_BASE,
        );
    }

    #[unsafe(naked)]
    extern "C" fn crash_console_flush() {
        naked_asm!(
            include_str!("../asm_macros_common.S"),
            "mov_imm	x0, {PLAT_ARM_CRASH_UART_BASE}",
            "b	console_pl011_core_flush",
            include_str!("../asm_macros_common_purge.S"),
            DEBUG = const DEBUG as i32,
            PLAT_ARM_CRASH_UART_BASE = const V2M_IOFPGA_UART1_BASE,
        );
    }

    /// Dumps relevant GIC registers.
    ///
    /// Clobbers x0-x11, x16, x17, sp.
    #[unsafe(naked)]
    unsafe extern "C" fn dump_registers() {
        naked_asm!(
            include_str!("../asm_macros_common.S"),
            include_str!("../arm_macros.S"),
            // Detect if we're using the base memory map or the legacy VE memory map.
            "mov_imm	x0, ({V2M_SYSREGS_BASE} + {V2M_SYS_ID})",
            "ldr	w16, [x0]",
            // Extract BLD (12th - 15th bits) from the SYS_ID.
            "ubfx	x16, x16, #{V2M_SYS_ID_BLD_SHIFT}, #4",
            // Check if VE mmap.
            "cmp	w16, #{BLD_GIC_VE_MMAP}",
            "b.eq	0f",
            // Assume Base Cortex mmap.
            "mov_imm	x16, {BASE_GICD_BASE}",
            "b	1f",
        "0:",
            "mov_imm	x16, {VE_GICD_BASE}",
        "1:",
            "arm_print_gic_regs",
            "ret",

            include_str!("../arm_macros_purge.S"),
            include_str!("../asm_macros_common_purge.S"),
            DEBUG = const DEBUG as i32,
            ICC_SRE_SRE_BIT = const IccSre::SRE.bits(),
            GICD_ISPENDR = const offset_of!(Gicd, ispendr),
            V2M_SYSREGS_BASE = const V2M_SYSREGS_BASE,
            V2M_SYS_ID = const V2M_SYS_ID,
            V2M_SYS_ID_BLD_SHIFT = const V2M_SYS_ID_BLD_SHIFT,
            BLD_GIC_VE_MMAP = const BLD_GIC_VE_MMAP,
            BASE_GICD_BASE = const BASE_GICD_BASE,
            VE_GICD_BASE = const VE_GICD_BASE,
        );
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
    fn from(value: FvpPowerState) -> Self {
        value as usize
    }
}

struct FvpGicContext {
    distributor_context: GicDistributorContext<
        { GicDistributorContext::ireg_count(988) },
        { GicDistributorContext::ireg_e_count(1024) },
    >,
    redistributor_context: GicRedistributorContext<{ GicRedistributorContext::ireg_count(96) }>,
}

impl FvpGicContext {
    const fn new() -> Self {
        Self {
            distributor_context: GicDistributorContext::new(),
            redistributor_context: GicRedistributorContext::new(),
        }
    }
}

static GIC_CONTEXT: SpinMutex<FvpGicContext> = SpinMutex::new(FvpGicContext::new());

pub struct FvpPsciPlatformImpl<'a> {
    power_controller: SpinMutex<FvpPowerController<'a>>,
    system: SpinMutex<FvpSystemPeripheral<'a>>,
    timer_control: SpinMutex<GenericTimerControl<'a>>,
    timer_ctl: SpinMutex<GenericTimerCtl<'a>>,
}

impl FvpPsciPlatformImpl<'_> {
    const CLUSTER_POWER_LEVEL: usize = 1;
    const NS_TIMER_INDEX: usize = 1;

    pub fn new(
        power_controller: PhysicalInstance<FvpPowerControllerRegisters>,
        system: PhysicalInstance<FvpSystemRegisters>,
        timer_control: PhysicalInstance<CntControlBase>,
        timer_ctl: PhysicalInstance<CntCtlBase>,
    ) -> Self {
        Self {
            power_controller: SpinMutex::new(FvpPowerController::new(map_peripheral(
                power_controller,
            ))),
            system: SpinMutex::new(FvpSystemPeripheral::new(map_peripheral(system))),
            timer_control: SpinMutex::new(GenericTimerControl::new(map_peripheral(timer_control))),
            timer_ctl: SpinMutex::new(GenericTimerCtl::new(map_peripheral(timer_ctl))),
        }
    }

    fn power_domain_on_finish_common(&self, previous_state: &PsciCompositePowerState) {
        assert_eq!(previous_state.cpu_level_state(), FvpPowerState::Off);

        let mpidr = read_mpidr_el1().bits() as u32;

        // Perform the common cluster specific operations.
        if previous_state.states[Self::CLUSTER_POWER_LEVEL] == FvpPowerState::Off {
            // This CPU might have woken up whilst the cluster was attempting to power down. In
            // this case the FVP power controller will have a pending cluster power off request
            // which needs to be cleared by writing to the PPONR register. This prevents the power
            // controller from interpreting a subsequent entry of this cpu into a simple wfi as a
            // power down request.
            self.power_controller.lock().power_on_processor(mpidr);
        }

        // Perform the common system specific operations.
        if previous_state.highest_level_state() == FvpPowerState::Off {
            self.restore_system_power_domain();
        }

        // Clear PWKUPR.WEN bit to ensure interrupts do not interfere with a cpu power down unless
        // the bit is set again.
        self.power_controller.lock().disable_wakeup_requests(mpidr);

        let frequency = self.timer_control.lock().base_frequency();
        write_cntfrq_el0(frequency.into());
    }

    // Enable and initialize the system level generic timer
    pub fn init_generic_timer(&self) {
        let mut timer_control = self.timer_control.lock();

        timer_control.set_enable(true);

        let frequency = timer_control.base_frequency();

        let mut timer_ctl = self.timer_ctl.lock();

        timer_ctl.set_access_control(Self::NS_TIMER_INDEX, CntAcr::all());
        timer_ctl.set_non_secure_access(Self::NS_TIMER_INDEX, true);
        timer_ctl.set_frequency(frequency);

        write_cntfrq_el0(frequency.into());
    }

    fn save_system_power_domain() {
        let mut context = GIC_CONTEXT.lock();

        Gic::get().redistributor_save(&mut context.redistributor_context);
        Gic::get().distributor_save(&mut context.distributor_context);

        log::logger().flush();

        // All the other peripheral which are configured by ARM TF are re-initialized on resume
        // from system suspend. Hence we don't save their state here.
    }

    fn restore_system_power_domain(&self) {
        let context = GIC_CONTEXT.lock();

        Gic::get().distributor_restore(&context.distributor_context);
        Gic::get().redistributor_restore(&context.redistributor_context);

        // TODO: plat_arm_security_setup();

        self.init_generic_timer();
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

    const FEATURES: PsciPlatformOptionalFeatures = PsciPlatformOptionalFeatures::NODE_HW_STATE
        .union(PsciPlatformOptionalFeatures::SYSTEM_SUSPEND);

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
        const POWER_LEVEL_STATE_MASK: u32 = 0x0000_0fff;

        let states = match power_state {
            PowerState::StandbyOrRetention(0x01) => [
                FvpPowerState::Retention,
                FvpPowerState::Run,
                FvpPowerState::Run,
            ],
            PowerState::PowerDown(power_downstate) => {
                match power_downstate & POWER_LEVEL_STATE_MASK {
                    0x002 => [FvpPowerState::Off, FvpPowerState::Run, FvpPowerState::Run],
                    0x022 => [FvpPowerState::Off, FvpPowerState::Off, FvpPowerState::Run],
                    // Ensure that the system power domain level is never suspended via PSCI
                    // CPU_SUSPEND API. System suspend is only supported via PSCI SYSTEM_SUSPEND
                    // API.
                    0x222 => [FvpPowerState::Off, FvpPowerState::Off, FvpPowerState::Run],
                    _ => return None,
                }
            }
            _ => return None,
        };

        Some(PsciCompositePowerState { states })
    }

    fn cpu_standby(&self, cpu_state: FvpPowerState) {
        assert!(cpu_state.power_state_type() == PowerStateType::StandbyOrRetention);

        // Enter standby state. DSB is good practice before using WFI to enter low power states.
        dsb_ish();
        wfi();
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
        Gic::get().cpu_interface_disable();

        // The Redistributor is not powered off as it can potentially prevent wake up events
        // reaching the CPUIF and/or might lead to losing register context.

        if target_state.states[Self::CLUSTER_POWER_LEVEL] == FvpPowerState::Off {
            self.power_controller.lock().power_off_cluster(mpidr);
        }

        // Perform the common system specific operations.
        if target_state.highest_level_state() == FvpPowerState::Off {
            Self::save_system_power_domain();
        }

        self.power_controller.lock().power_off_processor(mpidr);
    }

    fn power_domain_suspend_finish(&self, previous_state: &PsciCompositePowerState) {
        // Nothing to be done on waking up from retention at CPU level.
        if previous_state.cpu_level_state() == FvpPowerState::Retention {
            return;
        }

        self.power_domain_on_finish_common(previous_state);
        Gic::get().cpu_interface_enable();
    }

    fn power_domain_off(&self, target_state: &PsciCompositePowerState) {
        assert_eq!(FvpPowerState::Off, target_state.cpu_level_state());

        Gic::get().cpu_interface_disable();
        Gic::get().redistributor_off();

        let mpidr = read_mpidr_el1().bits() as u32;
        self.power_controller.lock().power_off_processor(mpidr);

        if target_state.states[Self::CLUSTER_POWER_LEVEL] == FvpPowerState::Off {
            self.power_controller.lock().power_off_cluster(mpidr);
        }
    }

    fn power_domain_on(&self, mpidr: Mpidr) -> Result<(), ErrorCode> {
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

    fn power_domain_on_finish(&self, previous_state: &PsciCompositePowerState) {
        self.power_domain_on_finish_common(previous_state);
        Gic::get().redistributor_init(&Fvp::GIC_CONFIG);
        Gic::get().cpu_interface_enable();
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

    fn sys_suspend_power_state(&self) -> PsciCompositePowerState {
        PsciCompositePowerState::OFF
    }

    /// Validates a non-secure entry point, optional.
    fn is_valid_ns_entrypoint(&self, entry: &EntryPoint) -> bool {
        let entrypoint = entry.entry_point_address() as usize;

        MemoryMap::DRAM0.contains(&entrypoint) || MemoryMap::DRAM1.contains(&entrypoint)
    }
}

global_asm!(include_str!("../arm_macros_data.S"));
