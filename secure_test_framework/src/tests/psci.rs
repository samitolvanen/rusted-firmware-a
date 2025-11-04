// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

//! Tests for PSCI calls.

use crate::framework::{expect::expect_eq, normal_world_test, secure_world_test};
use arm_psci::FeatureFlagsCpuSuspend;
use smccc::{Smc, psci};

fn is_osi_supported() -> Result<bool, ()> {
    let suspend_features = psci::psci_features::<Smc>(psci::PSCI_CPU_SUSPEND_64).map_err(|_| ())?;
    let flags = FeatureFlagsCpuSuspend::from_bits(suspend_features).ok_or(())?;
    Ok(flags.intersects(FeatureFlagsCpuSuspend::OS_INITIATED_MODE))
}

normal_world_test!(test_psci_version);
fn test_psci_version() -> Result<(), ()> {
    expect_eq!(
        psci::version::<Smc>(),
        Ok(psci::Version { major: 1, minor: 3 })
    );
    Ok(())
}

normal_world_test!(test_set_suspend_mode_to_osi);
fn test_set_suspend_mode_to_osi() -> Result<(), ()> {
    let has_osi_support = is_osi_supported()?;
    if !has_osi_support {
        return Ok(());
    }
    expect_eq!(
        psci::set_suspend_mode::<Smc>(psci::SuspendMode::OsInitiated),
        Ok(())
    );
    Ok(())
}

secure_world_test!(test_psci_version_secure);
fn test_psci_version_secure() -> Result<(), ()> {
    expect_eq!(psci::version::<Smc>(), Err(psci::Error::NotSupported));
    Ok(())
}
