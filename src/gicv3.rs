// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use core::{panic, ptr::NonNull};

use crate::{
    aarch64::{dsb_sy, isb},
    context::{CoresImpl, World},
    platform::{Platform, PlatformImpl, plat_calc_core_pos},
    sysregs::{MpidrEl1, ScrEl3, read_mpidr_el1, read_scr_el3, write_scr_el3},
};
use arm_gic::{
    IntId, Trigger,
    gicv3::{
        Group, SecureIntGroup,
        cpu_interface::GicCpuInterface,
        distributor::{GicDistributor, GicDistributorContext},
        redistributor::{GicRedistributor, GicRedistributorContext, GicRedistributorIterator},
        registers::{Gicd, GicdCtlr, GicrSgi},
    },
};
use arm_pl011_uart::UniqueMmioPointer;
use log::debug;
use percore::Cores;
use spin::{Once, mutex::Mutex};

const GIC_PRI_MASK: u8 = 0xff;

pub static GIC: Once<Gic> = Once::new();

/// The configuration of a single interrupt.
#[derive(Clone, Copy, Debug)]
pub struct InterruptConfig {
    /// Interrupt priority.
    /// 0x00 is highest priority, 0xFF is the lowest.
    pub priority: u8,
    /// Interrupt group that this interrupt should belong to.
    pub group: Group,
    /// To specify whether this interrupt should be edge or level triggered.
    pub trigger: Trigger,
}

pub type InterruptConfigEntry = (IntId, InterruptConfig);

/// The configuration of platform's GIC.
pub struct GicConfig {
    /// This list specifies which interrupts will be configured
    /// to non-default setup.
    pub interrupts_config: &'static [InterruptConfigEntry],
}

impl GicConfig {
    /// Get iterator for all interrupts.
    fn all(&self) -> impl Iterator<Item = &InterruptConfigEntry> {
        self.interrupts_config.iter()
    }

    /// Get iterator for shared interrupts.
    fn shared(&self) -> impl Iterator<Item = &InterruptConfigEntry> {
        self.interrupts_config.iter().filter(|int| int.0.is_spi())
    }

    /// Get iterator for private interrupts.
    fn private(&self) -> impl Iterator<Item = &InterruptConfigEntry> {
        self.interrupts_config
            .iter()
            .filter(|int| int.0.is_private())
    }
}
/// Specifies where an interrupt should be handled.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum InterruptType {
    El3,
    Secure,
    NonSecure,
    Invalid,
}

/// Registry for storing GIC redistributor instances.
struct GicRedistributorRegistry<'a> {
    redistributors: [Once<Mutex<GicRedistributor<'a>>>; PlatformImpl::CORE_COUNT],
}

impl<'a> GicRedistributorRegistry<'a> {
    /// # Safety
    /// The caller must ensure that `base` points to a continiously mapped GIC redistributor memory
    /// area that spans until the last redistributor block where GICR_TYPER.Last is set. There must
    /// be no other references to this address.
    pub unsafe fn new(base: *mut GicrSgi, gic_v4: bool) -> Self {
        let redistributors = [const { Once::new() }; PlatformImpl::CORE_COUNT];

        // Safety: The function propages the safety requirements to the caller.
        for pointer in unsafe { GicRedistributorIterator::new(base, gic_v4) } {
            let redist = GicRedistributor::new(pointer);

            let mpidr = MpidrEl1::from_psci_mpidr(redist.typer().core_mpidr());
            assert!(PlatformImpl::mpidr_is_valid(mpidr));

            let core_index = plat_calc_core_pos(mpidr.bits());

            redistributors[core_index].call_once(|| Mutex::new(redist));
        }

        Self { redistributors }
    }

    /// Get redistributor by linear index.
    pub fn redistributor(&self, index: usize) -> &Mutex<GicRedistributor<'a>> {
        self.redistributors[index]
            .get()
            .expect("The redistributor has not been initialized")
    }

    /// Iitialize or get the redistributor of the local core.
    pub fn local_redistributor(&self) -> &Mutex<GicRedistributor<'a>> {
        self.redistributor(CoresImpl::core_index())
    }
}

pub struct Gic<'a> {
    distributor: Mutex<GicDistributor<'a>>,
    redistributors: GicRedistributorRegistry<'a>,
}

impl Gic<'_> {
    /// # Safety
    /// The caller must ensure that `gicr_base` points to a continiously mapped GIC redistributor
    /// memory area that spans until the last redistributor block where GICR_TYPER.Last is set and
    /// `gicd` point to a valid GIC distributor region. There must be no other references to these
    /// addresses.
    pub unsafe fn new(gicd: *mut Gicd, gicr_base: *mut GicrSgi, gic_v4: bool) -> Self {
        Self {
            // Safety: Our caller promised that `gicd` is a valid and unique pointer to a GIC
            // distributor.
            distributor: Mutex::new(GicDistributor::new(unsafe {
                UniqueMmioPointer::new(NonNull::new(gicd).unwrap())
            })),
            // Safety:  Our caller promised that `gicr_base` is a valid and unique pointer to a GIC
            // redistributor block.
            redistributors: unsafe { GicRedistributorRegistry::new(gicr_base, gic_v4) },
        }
    }

    /// Get GIC instance.
    pub fn get() -> &'static Self {
        GIC.get().unwrap()
    }

    /// Sets the default configuration for all interrupts of the distributor. Configures the shared
    /// interrupts that were specificied in the `GicConfig` and enables the required interrupt
    /// groups.
    pub fn distributor_init(&self, config: &GicConfig) {
        let mut distributor = self.distributor.lock();

        // Clear the "enable" bits for G0/G1S/G1NS interrupts before configuring the ARE_S bit. The
        // Distributor might generate a system error otherwise.
        distributor.modify_control(
            GicdCtlr::EnableGrp0 | GicdCtlr::EnableGrp1S | GicdCtlr::EnableGrp1NS,
            false,
        );

        // Set the ARE_S and ARE_NS bit now that interrupts have been disabled
        distributor.modify_control(GicdCtlr::ARE_S | GicdCtlr::ARE_NS, true);

        // Set the default attribute of all (E)SPIs
        distributor.configure_default_settings();

        let mpidr = Some(read_mpidr_el1().bits());

        for (intid, config) in config.shared() {
            distributor.set_group(*intid, config.group);
            distributor.set_trigger(*intid, config.trigger);
            distributor.set_interrupt_priority(*intid, config.priority);
            distributor.set_routing(*intid, mpidr);
            distributor.enable_interrupt(*intid, true);
        }

        let gicd_ctlr = config
            .all()
            .fold(GicdCtlr::empty(), |acc, (_intid, config)| {
                acc | match config.group {
                    Group::Secure(SecureIntGroup::Group0) => GicdCtlr::EnableGrp0,
                    Group::Secure(SecureIntGroup::Group1S) => GicdCtlr::EnableGrp1S,
                    Group::Group1NS => panic!("configuring Group1NS is not permitted"),
                }
            });

        distributor.modify_control(gicd_ctlr, true);
    }

    /// Saves the distributor context.
    pub fn distributor_save(&self, context: &mut GicDistributorContext) {
        self.distributor.lock().save(context);
    }

    /// Restores the distributor context.
    pub fn distributor_restore(&self, context: &GicDistributorContext) {
        self.distributor.lock().restore(context);
    }

    /// Powers on the redistributor instance of the local core, then sets the default configuration
    /// for all interrupts of the redistributor. Configures the private interrupts that were
    /// specificied in the `GicConfig`.
    pub fn redistributor_init(&self, config: &GicConfig) {
        let mut redist = self.redistributors.local_redistributor().lock();

        redist.power_on();

        redist.configure_default_settings();

        for (intid, config) in config.private() {
            redist.set_group(*intid, config.group);
            if intid.is_ppi() {
                // Set interrupt configuration for PPIs.
                // Configurations for SGIs 0-15 are ignored.
                redist.set_trigger(*intid, config.trigger);
            }
            redist.set_interrupt_priority(*intid, config.priority);
            redist.enable_interrupt(*intid, true);
        }
    }

    // Turns off the local core's redistributor.
    pub fn redistributor_off(&self) {
        self.redistributors.local_redistributor().lock().power_off();
    }

    /// Saves the context of the local core's redistributor.
    pub fn redistributor_save(&self, context: &mut GicRedistributorContext) {
        self.redistributors
            .local_redistributor()
            .lock()
            .save(context);
    }

    /// Restores the context of the local core's redistributor.
    pub fn redistributor_restore(&self, context: &GicRedistributorContext) {
        let mut redistributor = self.redistributors.local_redistributor().lock();

        redistributor.power_on();
        redistributor.restore(context);
    }

    /// Enables and configures the GIC CPU interface.
    pub fn cpu_interface_enable(&self) {
        let mut redist = self.redistributors.local_redistributor().lock();
        redist.mark_core_awake().unwrap();

        GicCpuInterface::disable_legacy_interrupt_bypass_el3(true);

        // Enable system register access for EL3 and allow lower exception
        // levels to configure the same for themselves. If the legacy mode is
        // not supported, the SRE bit is RAO/WI
        GicCpuInterface::enable_system_register_el3(true, true);

        let scr_el3 = read_scr_el3();

        // Switch to non-secure state
        write_scr_el3(scr_el3 | ScrEl3::NS);
        isb();

        GicCpuInterface::disable_legacy_interrupt_bypass_el2(true);
        GicCpuInterface::enable_system_register_el2(true, true);
        GicCpuInterface::enable_system_register_el1(true);
        isb();

        // Switch to secure state.
        write_scr_el3(scr_el3 & !ScrEl3::NS);
        isb();

        GicCpuInterface::enable_system_register_el1(true);
        isb();

        GicCpuInterface::set_priority_mask(GIC_PRI_MASK);
        GicCpuInterface::enable_group0(true);
        GicCpuInterface::enable_group1_secure(true);

        // Restore the original SCR_EL3
        write_scr_el3(scr_el3);

        isb();
        dsb_sy();
    }

    /// Disables the GIC CPU interface.
    pub fn cpu_interface_disable(&self) {
        // Disable legacy interrupt bypass
        GicCpuInterface::disable_legacy_interrupt_bypass_el3(true);
        GicCpuInterface::enable_group0(false);
        GicCpuInterface::enable_group1_secure(false);
        GicCpuInterface::enable_group1_non_secure(false);

        isb();
        dsb_sy();

        // dsb() already issued previously after clearing the CPU group enabled, apply below
        // workaround to toggle the "DPG*" bits of GICR_CTLR register for unblocking event.
        // TODO: gicv3_apply_errata_wa_2384374(gicr_base);

        let mut redist = self.redistributors.local_redistributor().lock();
        redist.mark_core_asleep().unwrap();
    }
}

/// Initializes the gic by configuring the distributor, redistributor and cpu interface, and puts
/// the global gic into GIC variable. This function should only be called once early in the boot
/// process. Subsequent calls will be ignored.
pub fn init() {
    GIC.call_once(|| {
        // SAFETY: This is the only place where GIC is created and there are no aliases.
        let gic = unsafe { PlatformImpl::create_gic() };

        gic.distributor_init(&PlatformImpl::GIC_CONFIG);
        gic.redistributor_init(&PlatformImpl::GIC_CONFIG);
        gic.cpu_interface_enable();

        gic
    });
}

/// Configures interrupt-routing related flags in `scr_el3` bitflags.
///
/// While in NS-ELx:
/// - G0 and G1s are signalled as FIQs and should go through EL3.
/// - G1ns are signalled as IRQs and should be handled without a world switch.
///
/// While in S-ELx:
/// - G1s are signalled as IRQs and should be handled without a world switch.
/// - G0 and G1ns are signalled as FIQs and should not be routed until execution goes back to the NS world.
pub fn set_routing_model(scr_el3: &mut ScrEl3, world: World) {
    match world {
        World::NonSecure => {
            *scr_el3 |= ScrEl3::FIQ;
            *scr_el3 -= ScrEl3::IRQ;
        }
        World::Secure => {
            *scr_el3 -= ScrEl3::IRQ;
            *scr_el3 -= ScrEl3::FIQ;
        }
        #[cfg(feature = "rme")]
        World::Realm => todo!("Routing model for Realms not configured."),
    }
}

/// Returns the type of the highest priority pending group0 interrupt.
pub fn get_pending_interrupt_type() -> InterruptType {
    let int_id = GicCpuInterface::get_pending_interrupt_group0();

    match int_id {
        IntId::SPECIAL_NONE => InterruptType::Invalid,
        IntId::SPECIAL_SECURE => InterruptType::Secure,
        IntId::SPECIAL_NONSECURE => InterruptType::NonSecure,
        _ => InterruptType::El3,
    }
}

/// Wraps a platform-specific group 0 interrupt handler.
pub fn handle_group0_interrupt() {
    let int_id = GicCpuInterface::get_and_acknowledge_interrupt_group0();
    assert_ne!(int_id, IntId::SPECIAL_NONE);
    debug!("Group 0 interrupt {int_id:?} acknowledged");

    PlatformImpl::handle_group0_interrupt(int_id);

    GicCpuInterface::end_interrupt_group0(int_id);
    debug!("Group 0 interrupt {int_id:?} EOI");
}
