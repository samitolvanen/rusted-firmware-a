// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

//! A framework for managing ARM architectural CPU extensions using a trait-based approach.

use crate::{
    context::{CpuContext, PerWorldContext, World},
    sysregs::{
        CptrEl3, IdAa64isar1El1, IdAa64pfr0El1, Mpam3El3, ScrEl3, read_id_aa64isar1_el1,
        read_id_aa64pfr0_el1, write_mpam3_el3,
    },
};
use log::info;

/// A trait for managing CPU extensions.
pub trait CpuExtension {
    /// Checks if the CPU extension is supported by the hardware.
    fn is_present(&self) -> bool {
        true // TODO: implement for each extension and remove this default.
    }

    /// Configures the per-world EL3 registers to enable this extension.
    /// TODO: Switch to const traits when it becomes a stable feature:
    /// https://github.com/rust-lang/rust/issues/143874
    fn configure_per_world(&self, world: World, ctx: &mut PerWorldContext) {
        // No per-world configuration needed for by default.
        // TODO: Should we remove this default, so that every extension
        // is explicitly configured?
    }

    /// Configures the per-cpu EL3 registers to enable this extension.
    fn configure_per_cpu(&self, world: World, context: &mut CpuContext);
}
