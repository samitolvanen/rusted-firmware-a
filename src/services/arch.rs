// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use crate::{
    context::{World, world_context},
    services::{Service, owns},
    smccc::{
        FunctionId, INVALID_PARAMETER, NOT_SUPPORTED, OwningEntityNumber, SUCCESS, SmcReturn,
        SmcccCallType,
    },
    sysregs::ScrEl3,
};

use crate::platform::{Platform, PlatformImpl};

pub const SMCCC_VERSION: u32 = 0x8000_0000;
const SMCCC_ARCH_FEATURES: u32 = 0x8000_0001;
const SMCCC_ARCH_SOC_ID_32: u32 = 0x8000_0002;
const SMCCC_ARCH_SOC_ID_64: u32 = 0xc000_0002;
const SMCCC_ARCH_SOC_ID_VERSION: u32 = 0x0;
const SMCCC_ARCH_SOC_ID_REVISION: u32 = 0x1;
const SMCCC_ARCH_SOC_ID_NAME: u32 = 0x2;
const SMCCC_ARCH_WORKAROUND_1: u32 = 0x8000_8000;
const SMCCC_ARCH_WORKAROUND_2: u32 = 0x8000_7FFF;
const SMCCC_ARCH_WORKAROUND_3: u32 = 0x8000_3FFF;
const SMCCC_ARCH_WORKAROUND_4: u32 = 0x8000_0004;
const SMCCC_ARCH_FEATURE_AVAILABILITY: u32 = 0x8000_0003;

pub const SMCCC_VERSION_1_5: i32 = 0x0001_0005;

// Opcodes for the arch feature availability SMC
const SCR_EL3_OPCODE: u64 = 0x1e_1100;

// Mask for "active low" features in SCR_EL3 (enabled when the bit is 0)
const SCR_EL3_ACTIVE_LOW_MASK: ScrEl3 = ScrEl3::from_bits_truncate(
    ScrEl3::IRQ.bits()
        | ScrEl3::FIQ.bits()
        | ScrEl3::EA.bits()
        | ScrEl3::SMD.bits()
        | ScrEl3::TWI.bits()
        | ScrEl3::TWE.bits(),
);

const SCR_EL3_ARCH_FEATS_MASK: ScrEl3 =
    ScrEl3::from_bits_truncate(0xFFFF_FFFF_FFFF_FFFF).difference(ScrEl3::RES1);

/// Arm architecture SMCs.
pub struct Arch;

impl Service for Arch {
    owns!(OwningEntityNumber::ARM_ARCHITECTURE);

    fn handle_non_secure_smc(&self, regs: &[u64; 18]) -> (SmcReturn, World) {
        (
            Self::handle_common_smc(regs, World::NonSecure),
            World::NonSecure,
        )
    }

    fn handle_secure_smc(&self, regs: &[u64; 18]) -> (SmcReturn, World) {
        (Self::handle_common_smc(regs, World::Secure), World::Secure)
    }

    #[cfg(feature = "rme")]
    fn handle_realm_smc(&self, regs: &[u64; 18]) -> (SmcReturn, World) {
        (Self::handle_common_smc(regs, World::Realm), World::Realm)
    }
}

impl Arch {
    pub(super) fn new() -> Self {
        Self
    }

    fn handle_common_smc(regs: &[u64; 18], world: World) -> SmcReturn {
        let mut function = FunctionId(regs[0] as u32);
        function.clear_sve_hint();

        match function.0 {
            SMCCC_VERSION => version().into(),
            SMCCC_ARCH_FEATURES => arch_features(regs[1] as u32).into(),
            SMCCC_ARCH_SOC_ID_32 | SMCCC_ARCH_SOC_ID_64 => {
                arch_soc_id(regs[1] as u32, function.call_type())
            }
            SMCCC_ARCH_WORKAROUND_1 => arch_workaround_1().into(),
            SMCCC_ARCH_WORKAROUND_2 => arch_workaround_2(regs[1] as u32).into(),
            SMCCC_ARCH_WORKAROUND_3 => arch_workaround_3().into(),
            SMCCC_ARCH_FEATURE_AVAILABILITY => arch_feature_availability(regs[1], world),
            _ => NOT_SUPPORTED.into(),
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(i32)]
pub enum WorkaroundSupport {
    Required = 0,
    SafeButNotRequired = 1,
}

fn version() -> i32 {
    SMCCC_VERSION_1_5
}

fn arch_features(arch_func_id: u32) -> i32 {
    match arch_func_id {
        SMCCC_VERSION | SMCCC_ARCH_FEATURES | SMCCC_ARCH_SOC_ID_32 | SMCCC_ARCH_SOC_ID_64 => {
            SUCCESS
        }
        SMCCC_ARCH_WORKAROUND_1 => PlatformImpl::arch_workaround_1_supported() as i32,
        SMCCC_ARCH_WORKAROUND_2 => PlatformImpl::arch_workaround_2_supported() as i32,
        SMCCC_ARCH_WORKAROUND_3 => PlatformImpl::arch_workaround_3_supported() as i32,
        SMCCC_ARCH_WORKAROUND_4 => PlatformImpl::arch_workaround_4_supported() as i32,
        _ => NOT_SUPPORTED,
    }
}

/// This SMC is specified in §7.4 of [the Arm SMC Calling
/// Convention](https://developer.arm.com/documentation/den0028/galp1/?lang=en).
fn arch_soc_id(soc_id_type: u32, call_type: SmcccCallType) -> SmcReturn {
    // TODO/NOTE: Note that according to the SMCCC spec, section 7.4.6: we "must
    // ensure that SoC version and revision uniquely identify the SoC", and "SoC
    // name must not contain SoC identifying information not captured by <SoC
    // version, SoC revision>."
    match soc_id_type {
        SMCCC_ARCH_SOC_ID_VERSION => 0.into(), // TODO: Implement this properly.
        SMCCC_ARCH_SOC_ID_REVISION => 0.into(), // TODO: Implement this properly.
        SMCCC_ARCH_SOC_ID_NAME if call_type == SmcccCallType::Fast64 => {
            [
                // TODO: Implement this properly.
                0u64, // w0
                u64::from_le_bytes([b'm', b'I', b' ', b':', b'O', b'D', b'O', b'T']),
                u64::from_le_bytes([b' ', b't', b'n', b'e', b'm', b'e', b'l', b'p']),
                u64::from_le_bytes([b'o', b'r', b'p', b' ', b's', b'i', b'h', b't']),
                u64::from_le_bytes([0x00, 0x00, b'.', b'y', b'l', b'r', b'e', b'p']),
            ]
            .into()
        }
        _ => INVALID_PARAMETER.into(),
    }
}

/// Execute the mitigation for CVE-2017-5715 on the calling PE.
fn arch_workaround_1() {
    if PlatformImpl::arch_workaround_1_supported() == WorkaroundSupport::Required {
        PlatformImpl::arch_workaround_1()
    }
}

/// Enable the mitigation for CVE-2018-3639 on the calling PE. (Contrary to the
/// latest specification as of January 2025, the argument is ignored.)
fn arch_workaround_2(_: u32) {
    if PlatformImpl::arch_workaround_2_supported() == WorkaroundSupport::Required {
        PlatformImpl::arch_workaround_2()
    }
}

/// Execute the mitigation for CVE-2017-5715 and CVE-2022-23960 on the calling PE.
fn arch_workaround_3() {
    if PlatformImpl::arch_workaround_3_supported() == WorkaroundSupport::Required {
        PlatformImpl::arch_workaround_3()
    }
}

fn arch_feature_availability(bitmask_selector: u64, world: World) -> SmcReturn {
    let context = world_context(world);

    // SAFETY: The world context pointer is guaranteed to be valid for the
    // duration of the SMC handler. This converts it to a safe reference.
    let context = unsafe { &*world_context(world) };

    // TODO: Read reg features per the opcode
    // This is a simple boilerplate that returns success with a zero bitmask,
    // indicating no extra features are enabled by the firmware.

    let (feat_bitmask, check) = match bitmask_selector {
        SCR_EL3_OPCODE => {
            let scr = context.el3_state.scr_el3;
            let arch_feat_mask = scr & !ScrEl3::RES1;
            let chck = arch_feat_mask & !SCR_EL3_ARCH_FEATS_MASK;
            let mut features = arch_feat_mask & SCR_EL3_ARCH_FEATS_MASK;
            features ^= SCR_EL3_ACTIVE_LOW_MASK;
            (features.bits(), chck.bits())
        }

        _ => return [INVALID_PARAMETER as u64, 0u64].into(),
    };

    assert_eq!(
        check, 0,
        "Unexpected bits {:#x} set in register with opcode {:#x}",
        check, bitmask_selector
    );

    // Per the spec, on success this returns:
    // x0 = SUCCESS (0)
    // x1 = feat_bitmask
    // Return SUCCESS in x0 and the calculated bitmask in x1.
    [SUCCESS as u64, feat_bitmask].into()
}
