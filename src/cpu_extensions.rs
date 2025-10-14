// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

//! A framework for managing ARM architectural CPU extensions using a trait-based approach.

use crate::context::{CpuContext, PerWorldContext, World};

/// A trait for managing CPU extensions.
pub trait CpuExtension {
    /// Checks if the CPU extension is supported by the hardware.
    fn is_present(&self) -> bool;

    /// Optional function to enable the feature in-place in any EL3 registers that are never
    /// context switched.
    /// The values written must never change.
    fn init(&self) {}

    /// Configures the per-world EL3 registers to enable this extension.
    /// TODO: Switch to const traits when it becomes a stable feature:
    /// https://github.com/rust-lang/rust/issues/143874
    fn configure_per_world(&self, _world: World, _ctx: &mut PerWorldContext) {}

    /// Configures the per-cpu EL3 registers related to this extension.
    fn configure_per_cpu(&self, _world: World, _context: &mut CpuContext) {}

    /// Save the extension-specific registers before switching from world `world`.
    fn save_context(&self, _world: World) {}

    /// Restore the extension-specific registers after switching to world `world`.
    fn restore_context(&self, _world: World) {}
}
