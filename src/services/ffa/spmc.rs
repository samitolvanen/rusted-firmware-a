// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use crate::{
    context::World,
    pagetable::{MT_DEVICE, MT_MEMORY_WB},
    services::ffa::spmd::Spmd,
};
use aarch64_paging::paging::{Attributes, MemoryRegion, PhysicalAddress, VirtualAddress};
use libspmc::{
    KernelInterface, SpInterface,
    arm_ffa::{Interface, Version},
    manifest::{PartitionProperties, SpManifest},
};
use log::debug;
use spin::{Lazy, mutex::SpinMutex};
use uuid::uuid;

fn convert_access_rights(value: libspmc::MemoryAccessRights) -> Attributes {
    let mut access_rights = if value.contains(libspmc::MemoryAccessRights::DEVICE) {
        MT_DEVICE
    } else {
        MT_MEMORY_WB
    };

    if value.contains(libspmc::MemoryAccessRights::RW) {
        access_rights |= Attributes::PXN | Attributes::UXN;
    } else if value.contains(libspmc::MemoryAccessRights::R) {
        access_rights |= Attributes::PXN | Attributes::UXN | Attributes::READ_ONLY;
    }

    if value.contains(libspmc::MemoryAccessRights::NS) {
        access_rights |= Attributes::NS;
    }

    if value.contains(libspmc::MemoryAccessRights::USER) {
        access_rights |= Attributes::USER;
    }

    if !value.contains(libspmc::MemoryAccessRights::GLOBAL) {
        access_rights |= Attributes::NON_GLOBAL;
    }

    access_rights
}

#[derive(Clone, Copy)]
pub struct KernelPa(PhysicalAddress);

impl From<usize> for KernelPa {
    fn from(value: usize) -> Self {
        Self(PhysicalAddress(value))
    }
}

impl From<KernelPa> for usize {
    fn from(value: KernelPa) -> Self {
        value.0.0
    }
}

#[derive(Clone, Copy)]
pub struct KernelVa(VirtualAddress);

impl From<usize> for KernelVa {
    fn from(value: usize) -> Self {
        Self(VirtualAddress(value))
    }
}

impl From<KernelVa> for usize {
    fn from(value: KernelVa) -> Self {
        value.0.0
    }
}

impl From<KernelVa> for KernelPa {
    fn from(value: KernelVa) -> Self {
        KernelPa(crate::pagetable::va_to_pa(value.0))
    }
}

#[derive(Clone, Copy)]
pub struct KernelSpace {}

impl KernelInterface for KernelSpace {
    type PhysicalAddress = KernelPa;
    type VirtualAddress = KernelVa;

    fn map_memory(
        &self,
        pa: Self::PhysicalAddress,
        length: usize,
        perm: libspmc::MemoryAccessRights,
    ) -> Self::VirtualAddress {
        debug!(
            "Kernel mapping memory: PA={:#x}, length={:#x}, perm={:?}",
            usize::from(pa),
            length,
            perm
        );

        let region = &MemoryRegion::new(pa.0.0, pa.0.0 + length);
        let attributes = convert_access_rights(perm);

        let mut page_table = crate::pagetable::get_page_table().lock();
        crate::pagetable::map_region(&mut page_table, region, attributes);

        pa.0.0.into()
    }

    fn unmap_memory(&self, va: Self::VirtualAddress, length: usize) {
        let region = &MemoryRegion::new(va.0.0, va.0.0 + length);

        let mut page_table = crate::pagetable::get_page_table().lock();
        crate::pagetable::unmap_region(&mut page_table, region);
    }
}

pub struct Sp {
    id: u16,
    version: Version,
}

impl SpInterface for Sp {
    type PhysicalAddress = usize;

    type VirtualAddress = usize;

    fn map_memory(
        &self,
        _pa: Self::PhysicalAddress,
        _length: usize,
        _perm: libspmc::MemoryAccessRights,
    ) -> Self::VirtualAddress {
        0
    }

    fn unmap_memory(&self, _va: Self::VirtualAddress, _length: usize) {}

    fn set_access_rights(&self, _va: Self::VirtualAddress, _perm: libspmc::MemoryAccessRights) {}

    fn va_to_pa(&self, va: Self::VirtualAddress, _length: usize) -> Self::PhysicalAddress {
        0
    }
}

pub struct Spmc<'a> {
    pub libspmc: libspmc::spmc::Spmc<'a, KernelSpace, Spmd, Sp>,
}

impl<'a> Spmc<'a> {
    pub fn new() -> Self {
        let kernel_space = KernelSpace {};

        let mut libspmc = libspmc::spmc::Spmc::new(&crate::services::SERVICES.spmd, kernel_space);

        let sp_manifest1 = SpManifest {
            properties: PartitionProperties {
                version: Version(1, 2),
                partition_id: Some(0x8001),
                // uuids: &[uuid!("bdcd76d7-825e-4751-963b-86d4f84943ac")],
                uuids: &[uuid!("b4b5671e-4a90-4fe1-b81f-fb13dae1dacb")],
                messaging_method: (),
                name: "Cactus SP",
                // name: "STF SP",
                execution_ctx_count: 1,
                runtime_el: 1,
                execution_state: (),
                load_address: None,
                entry_point_offset: 0,
                translation_granule: 0,
                boot_order: None,
                runtime_model: (),
                ns_interrupt_action: (),
                ffa_boot_info_reg: None,
                cold_boot_reason_reg: None,
            },
            memory_regions: &[],
            device_regions: &[],
        };

        let sp1 = Sp {
            // TODO: what if the SP ID is allocated by the SPMC
            id: sp_manifest1.properties.partition_id.unwrap(),
            version: sp_manifest1.properties.version,
        };

        let sp_pkg_lib1 = libspmc::manifest::SpPackage {
            manifest: sp_manifest1,
            image: &[],
            sp: sp1,
        };

        libspmc.add_sp(sp_pkg_lib1);
        let _ = libspmc.sp_initial_args(0x8001);

        Self { libspmc }
    }

    pub fn handle_call_from_spmd(msg: &mut Interface) -> World {
        match SPMC.lock().libspmc.handle_msg_from_spmd(msg) {
            libspmc::NextComponent::Spmd => World::NonSecure,
            libspmc::NextComponent::Sp(_) => World::Secure,
        }
    }

    pub fn handle_call_from_sp(msg: &mut Interface, sp_id: u16) -> World {
        match SPMC.lock().libspmc.handle_msg_from_sp(msg, sp_id) {
            libspmc::NextComponent::Spmd => World::NonSecure,
            libspmc::NextComponent::Sp(_) => World::Secure,
        }
    }
}

pub static SPMC: Lazy<SpinMutex<Spmc<'static>>> = Lazy::new(|| SpinMutex::new(Spmc::new()));
