// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use crate::{
    context::World,
    services::{Service, owns},
    smccc::{FunctionId, NOT_SUPPORTED, OwningEntityNumber, SmcReturn},
};
use arm_sysregs::ExceptionLevel;
use log::trace;

const FUNCTION_NUMBER_MIN: u16 = 0x00F0;
const FUNCTION_NUMBER_MAX: u16 = 0x00FF;

const EM_VERSION: u32 = 0x8400_00F0;
const EM_FEATURES: u32 = 0x8400_00F1;
const EM_CPU_ERRATUM_FEATURES: u32 = 0x8400_00F2;

const VERSION_1_0: i32 = 0x0001_0000;

/// A status value returned by errata management functions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i32)]
enum Status {
    HigherElMitigation = 3,
    NotAffected = 2,
    Affected = 1,
    Success = 0,
    NotSupported = -1,
    InvalidParameters = -2,
    UnknownErratum = -3,
}

impl From<Status> for SmcReturn {
    fn from(status: Status) -> Self {
        (status as i32).into()
    }
}

/// Arm Errata Management Firmware Interface.
pub struct ErrataManagement;

impl ErrataManagement {
    pub fn new() -> Self {
        Self
    }

    fn handle_smc(regs: &[u64; 18], world: World) -> SmcReturn {
        let mut function = FunctionId(regs[0] as u32);
        function.clear_sve_hint();

        match function.0 {
            EM_VERSION => version().into(),
            EM_FEATURES => features(regs[1] as u32).into(),
            EM_CPU_ERRATUM_FEATURES => cpu_erratum_features(regs, world).into(),
            _ => NOT_SUPPORTED.into(),
        }
    }
}

impl Service for ErrataManagement {
    owns!(
        OwningEntityNumber::STANDARD_SECURE,
        FUNCTION_NUMBER_MIN..=FUNCTION_NUMBER_MAX
    );

    fn handle_non_secure_smc(&self, regs: &[u64; 18]) -> (SmcReturn, World) {
        (Self::handle_smc(regs, World::NonSecure), World::NonSecure)
    }

    fn handle_secure_smc(&self, regs: &[u64; 18]) -> (SmcReturn, World) {
        (Self::handle_smc(regs, World::Secure), World::Secure)
    }

    #[cfg(feature = "rme")]
    fn handle_realm_smc(&self, regs: &[u64; 18]) -> (SmcReturn, World) {
        (Self::handle_smc(regs, World::Realm), World::Realm)
    }
}

fn version() -> i32 {
    VERSION_1_0
}

fn features(em_func_id: u32) -> i32 {
    match em_func_id {
        EM_VERSION | EM_FEATURES | EM_CPU_ERRATUM_FEATURES => Status::Success as i32,
        _ => Status::NotSupported as i32,
    }
}

fn cpu_erratum_features(regs: &[u64; 18], world: World) -> Status {
    let cpu_erratum_id = regs[1] as u32;
    let forward_flag = regs[2] != 0;

    if regs[3] != 0 || regs[4] != 0 || regs[5] != 0 || regs[6] != 0 || regs[7] != 0 {
        return Status::InvalidParameters;
    }

    let effective_originator = if world == World::Secure && !cfg!(feature = "sel2") {
        if forward_flag {
            return Status::InvalidParameters;
        }
        ExceptionLevel::El1
    } else if forward_flag {
        ExceptionLevel::El1
    } else {
        ExceptionLevel::El2
    };

    trace!(
        "Checking erratum {} for {:?}",
        cpu_erratum_id, effective_originator,
    );

    Status::UnknownErratum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn em_version_non_secure() {
        assert_eq!(
            ErrataManagement.handle_non_secure_smc(&[
                EM_VERSION.into(),
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0
            ]),
            (SmcReturn::from(0x0001_0000), World::NonSecure)
        );
    }

    #[test]
    fn em_version_secure() {
        assert_eq!(
            ErrataManagement.handle_secure_smc(&[
                EM_VERSION.into(),
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0
            ]),
            (SmcReturn::from(0x0001_0000), World::Secure)
        );
    }

    #[test]
    fn em_features() {
        assert_eq!(
            ErrataManagement.handle_non_secure_smc(&[
                EM_FEATURES.into(),
                EM_VERSION.into(),
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0
            ]),
            (SmcReturn::from(0), World::NonSecure)
        );
        assert_eq!(
            ErrataManagement.handle_non_secure_smc(&[
                EM_FEATURES.into(),
                EM_FEATURES.into(),
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0
            ]),
            (SmcReturn::from(0), World::NonSecure)
        );
        assert_eq!(
            ErrataManagement.handle_non_secure_smc(&[
                EM_FEATURES.into(),
                EM_CPU_ERRATUM_FEATURES.into(),
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0
            ]),
            (SmcReturn::from(0), World::NonSecure)
        );
        assert_eq!(
            ErrataManagement.handle_non_secure_smc(&[
                EM_FEATURES.into(),
                0x8400_00F3,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0
            ]),
            (SmcReturn::from(-1), World::NonSecure)
        );
    }

    /// Reserved parameters must be 0.
    #[test]
    fn em_cpu_erratum_features_extra_parameters() {
        assert_eq!(
            ErrataManagement.handle_non_secure_smc(&[
                EM_CPU_ERRATUM_FEATURES.into(),
                42,
                0,
                66, // Reserved, shouldn't be set
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0
            ]),
            (SmcReturn::from(-2), World::NonSecure)
        );
    }

    /// Calls from S-EL1 shouldn't pass a non-zero forward_flag.
    #[cfg(not(feature = "sel2"))]
    #[test]
    fn em_cpu_erratum_features_invalid_forward_flag() {
        assert_eq!(
            ErrataManagement.handle_secure_smc(&[
                EM_CPU_ERRATUM_FEATURES.into(),
                42,
                1,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0
            ]),
            (SmcReturn::from(-2), World::Secure)
        );
    }

    #[test]
    fn em_cpu_erratum_features_unknown() {
        assert_eq!(
            ErrataManagement.handle_non_secure_smc(&[
                EM_CPU_ERRATUM_FEATURES.into(),
                42, // Assuming there's no erratum with ID 42.
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0
            ]),
            (SmcReturn::from(-3), World::NonSecure)
        );
    }
}
