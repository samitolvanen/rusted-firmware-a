// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use crate::{
    context::{PerCoreState, World},
    platform::{Platform, PlatformImpl, exception_free},
    services::{Service, owns},
    smccc::{OwningEntityNumber, SmcReturn},
};
use arm_ffa::{
    DirectMsgArgs, FfaError, Interface, SecondaryEpRegisterAddr, SuccessArgsIdGet,
    SuccessArgsSpmIdGet, TargetInfo, Version,
};
use core::{
    cell::RefCell,
    sync::atomic::{AtomicUsize, Ordering::Relaxed},
};
use log::{debug, error, info, warn};
use percore::{ExceptionLock, PerCore};

const FUNCTION_NUMBER_MIN: u16 = 0x0060;
const FUNCTION_NUMBER_MAX: u16 = 0x00FF;

/// Core-local state of the SPMD service
struct SpmdLocal {
    spmc_state: SpmcState,
}

impl SpmdLocal {
    const fn new() -> Self {
        Self {
            spmc_state: SpmcState::Boot,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpmcState {
    Boot,
    Runtime,
    SecureInterrupt,
}

/// Secure Partition Manager Dispatcher, defined by Arm Firmware Framework for A-Profile (FF-A)
pub struct Spmd {
    spmc_id: u16,
    spmc_version: Version,
    spmc_primary_ep: usize,
    spmc_secondary_ep: AtomicUsize,
    core_local: PerCoreState<SpmdLocal>,
}

impl Service for Spmd {
    owns!(
        OwningEntityNumber::STANDARD_SECURE,
        FUNCTION_NUMBER_MIN..=FUNCTION_NUMBER_MAX
    );

    fn handle_non_secure_smc(&self, regs: &[u64; 18]) -> (SmcReturn, World) {
        // TODO: forward SVE hint bit

        // TODO: should we use a different version for NWd?
        let version = self.spmc_version;

        value_or_error(
            World::NonSecure,
            version,
            self.handle_non_secure_call(regs, version),
        )
    }

    fn handle_secure_smc(&self, regs: &[u64; 18]) -> (SmcReturn, World) {
        let version = self.spmc_version;

        value_or_error(
            World::NonSecure,
            version,
            self.handle_secure_call(regs, version),
        )
    }
}

/// Converts the result of of a call handler method to the appropriate `SmcReturn` value and next
/// world.
fn value_or_error(
    world: World,
    version: Version,
    result: Result<(Option<Interface>, World), FfaError>,
) -> (SmcReturn, World) {
    match result {
        Ok((None, next_world)) => (SmcReturn::EMPTY, next_world),
        Ok((Some(out_msg), next_world)) => (interface_to_regs(out_msg, version), next_world),
        Err(e) => (interface_to_regs(Interface::error(e), version), world),
    }
}

/// Converts the set of registers of an SMC call from the given world to the equivalent FF-A
/// [`Interface`].
///
/// If it isn't a valid FF-A interface then logs an error and returns an error to be passed back to
/// the calling world.
fn interface_from_regs(
    regs: &[u64; 18],
    world: World,
    version: Version,
) -> Result<Interface, FfaError> {
    let in_regs = if version.needs_18_regs() {
        &regs[..]
    } else {
        &regs[..8]
    };

    Interface::from_regs(version, in_regs).map_err(|e| {
        error!("Invalid FF-A call from {:?} World: {}", world, e);
        e.into()
    })
}

/// Converts an FF-A [`Interface`] to the equivalent `SmcReturn` value.
fn interface_to_regs(interface: Interface, version: Version) -> SmcReturn {
    let mut out_regs = if version.needs_18_regs() {
        SmcReturn::from([0u64; 18])
    } else {
        SmcReturn::from([0u64; 8])
    };
    interface.to_regs(version, out_regs.values_mut());
    out_regs
}

impl Spmd {
    const OWN_ID: u16 = 0xffff;
    const VERSION: Version = Version(1, 2);
    const NS_EP_ID: u16 = 0; // TODO: this should come from arm_ffa

    /// Initialises the SPMD state.
    ///
    /// This should be called exactly once, before any other SPMD methods are called or any
    /// secondary CPUs are started.
    pub(super) fn new() -> Self {
        info!("Initializing SPMD");

        // TODO: read these attributes from the SPMC manifest
        let spmc_id = 0x8000;
        let spmc_version = Version(1, 2);
        let spmc_primary_ep = 0x0600_0000;

        assert!(spmc_version.is_compatible_to(&Spmd::VERSION));

        let core_local = PerCore::new(
            [const { ExceptionLock::new(RefCell::new(SpmdLocal::new())) };
                PlatformImpl::CORE_COUNT],
        );

        Self {
            spmc_id,
            spmc_version,
            spmc_primary_ep,
            // By default the secondary EP is same as primary
            spmc_secondary_ep: spmc_primary_ep.into(),
            core_local,
        }
    }

    pub fn primary_ep(&self) -> usize {
        self.spmc_primary_ep
    }

    pub fn secondary_ep(&self) -> usize {
        self.spmc_secondary_ep.load(Relaxed)
    }

    fn handle_secure_call_common(
        &self,
        in_msg: &Interface,
    ) -> Result<(Option<Interface>, World), FfaError> {
        let out_msg = match in_msg {
            Interface::Features { .. } => {
                // TODO: add list of supported features
                Interface::success32_noargs()
            }
            Interface::IdGet => Interface::Success {
                target_info: TargetInfo::default(),
                args: SuccessArgsIdGet { id: self.spmc_id }.into(),
            },
            Interface::SpmIdGet => Interface::Success {
                target_info: TargetInfo::default(),
                args: SuccessArgsSpmIdGet { id: Self::OWN_ID }.into(),
            },
            _ => {
                warn!("Unsupported FF-A call from Secure World: {:x?}", in_msg);
                return Err(FfaError::NotSupported);
            }
        };

        Ok((Some(out_msg), World::Secure))
    }

    fn handle_secure_call_boot(
        &self,
        in_msg: &Interface,
    ) -> Result<(Option<Interface>, World), FfaError> {
        let out_msg = match in_msg {
            Interface::Error { error_code, .. } => {
                // TODO: should we return an error instead of panic?
                panic!("SPMC init failed with error {}", error_code);
            }
            Interface::Version { input_version } => Interface::VersionOut {
                output_version: Self::VERSION.min(*input_version),
            },
            Interface::MsgWait { .. } => {
                // Receiving this message for the first time means that SPMC init succeeded
                exception_free(|token| {
                    let mut spmd_state = self.core_local.get().borrow_mut(token);
                    assert_eq!(spmd_state.spmc_state, SpmcState::Boot);
                    spmd_state.spmc_state = SpmcState::Runtime;
                });

                // In this case the FFA_MSG_WAIT message shouldn't be forwarded, because this is not
                // a response to a call made by NWd.
                return Ok((None, World::NonSecure));
            }
            Interface::SecondaryEpRegister { entrypoint } => {
                // TODO: check if the entrypoint is within the range of the SPMC's memory range
                // TODO: return Denied error if this is called on a secondary core
                let secondary_ep = match entrypoint {
                    SecondaryEpRegisterAddr::Addr32(addr) => *addr as usize,
                    SecondaryEpRegisterAddr::Addr64(addr) => *addr as usize,
                };
                self.spmc_secondary_ep.store(secondary_ep, Relaxed);
                Interface::success32_noargs()
            }
            Interface::Features { .. }
            | Interface::IdGet
            | Interface::SpmIdGet
            | Interface::PartitionInfoGetRegs { .. } => {
                return self.handle_secure_call_common(in_msg);
            }
            _ => {
                warn!("Denied FF-A call from Secure World: {:x?}", in_msg);
                return Err(FfaError::Denied);
            }
        };

        Ok((Some(out_msg), World::Secure))
    }

    fn handle_secure_call_runtime(
        &self,
        in_msg: &Interface,
    ) -> Result<(Option<Interface>, World), FfaError> {
        let next_world;

        let out_msg = match in_msg {
            Interface::NormalWorldResume => {
                // Normal world execution was not preempted
                return Err(FfaError::Denied);
            }
            Interface::MsgSendDirectResp {
                src_id,
                dst_id,
                args,
            } => {
                if *dst_id == Self::OWN_ID {
                    match *args {
                        DirectMsgArgs::VersionResp { version } if *src_id == self.spmc_id => {
                            next_world = World::NonSecure;
                            match version {
                                None => Interface::error(FfaError::NotSupported),
                                Some(v) => Interface::VersionOut { output_version: v },
                            }
                        }
                        _ => return Err(FfaError::InvalidParameters),
                    }
                } else {
                    // Forward to NWd
                    next_world = World::NonSecure;
                    *in_msg
                }
            }
            Interface::Features { .. }
            | Interface::IdGet
            | Interface::SpmIdGet
            | Interface::PartitionInfoGetRegs { .. } => {
                return self.handle_secure_call_common(in_msg);
            }
            Interface::Error { .. }
            | Interface::Success { .. }
            | Interface::Interrupt { .. }
            | Interface::MsgWait { .. }
            | Interface::Yield
            | Interface::MemRetrieveResp { .. } => {
                // Forward to NWd
                next_world = World::NonSecure;
                *in_msg
            }
            _ => {
                warn!("Unsupported FF-A call from Secure World: {:x?}", in_msg);
                return Err(FfaError::NotSupported);
            }
        };

        Ok((Some(out_msg), next_world))
    }

    fn handle_secure_call_interrupt(
        &self,
        in_msg: &Interface,
    ) -> Result<(Option<Interface>, World), FfaError> {
        match in_msg {
            Interface::NormalWorldResume => {
                exception_free(|token| {
                    let mut spmd_state = self.core_local.get().borrow_mut(token);
                    assert_eq!(spmd_state.spmc_state, SpmcState::SecureInterrupt);
                    spmd_state.spmc_state = SpmcState::Runtime;
                });

                // Interrupt was handled, return to NWd which was preempted by a secure interrupt.
                // Instead of forwarding the FFA_NORMAL_WORLD_RESUME message, NWd must be resumed
                // without any modification to its context. Returning None here will be converted to
                // SmcReturn::EMPTY by handle_secure_smc(), which means that no register will get
                // overwritten in NWd's context.
                return Ok((None, World::NonSecure));
            }
            _ => {
                warn!("Denied FF-A call from Secure World: {:x?}", in_msg);
                return Err(FfaError::Denied);
            }
        }
    }

    fn handle_secure_call(
        &self,
        regs: &[u64; 18],
        version: Version,
    ) -> Result<(Option<Interface>, World), FfaError> {
        let in_msg = interface_from_regs(regs, World::Secure, version)?;

        debug!("Handle FF-A call from SWd {:x?}", in_msg);

        let spmc_state =
            exception_free(|token| self.core_local.get().borrow(token).borrow().spmc_state);

        match spmc_state {
            SpmcState::Boot => self.handle_secure_call_boot(&in_msg),
            SpmcState::Runtime => self.handle_secure_call_runtime(&in_msg),
            SpmcState::SecureInterrupt => self.handle_secure_call_interrupt(&in_msg),
        }
    }

    fn handle_non_secure_call(
        &self,
        regs: &[u64; 18],
        version: Version,
    ) -> Result<(Option<Interface>, World), FfaError> {
        let in_msg = interface_from_regs(regs, World::NonSecure, version)?;

        // By default return to the same world
        let mut next_world = World::NonSecure;

        debug!("Handle FF-A call from NWd {:x?}", in_msg);

        let out_msg = match in_msg {
            Interface::Version { input_version } => {
                if self.spmc_version == Version(1, 0) {
                    // If the SPMC version is 1.0, we have to show this to NWd
                    Interface::VersionOut {
                        output_version: Version(1, 0),
                    }
                } else {
                    // Forward version call to the SPMC
                    next_world = World::Secure;
                    Interface::MsgSendDirectReq {
                        src_id: Self::OWN_ID,
                        dst_id: self.spmc_id,
                        args: DirectMsgArgs::VersionReq {
                            version: input_version,
                        },
                    }
                }
            }
            Interface::IdGet => {
                // Return Hypervisor / NWd kernel endpoint ID
                Interface::Success {
                    target_info: TargetInfo::default(),
                    args: SuccessArgsIdGet { id: Self::NS_EP_ID }.into(),
                }
            }
            Interface::SpmIdGet => Interface::Success {
                target_info: TargetInfo::default(),
                args: SuccessArgsSpmIdGet { id: self.spmc_id }.into(),
            },
            Interface::MsgSendDirectReq { src_id, .. } => {
                // Validate source endpoint ID
                // TODO: create a function to check this
                if src_id & 0x8000 != 0 {
                    return Err(FfaError::InvalidParameters);
                } else {
                    // Forward to SWd
                    next_world = World::Secure;
                    in_msg
                }
            }
            Interface::Error { .. }
            | Interface::Success { .. }
            | Interface::Features { .. }
            | Interface::RxAcquire { .. }
            | Interface::RxRelease { .. }
            | Interface::RxTxMap { .. }
            | Interface::RxTxUnmap { .. }
            | Interface::PartitionInfoGet { .. }
            | Interface::PartitionInfoGetRegs { .. }
            | Interface::Run { .. }
            | Interface::MemDonate { .. }
            | Interface::MemLend { .. }
            | Interface::MemShare { .. }
            | Interface::MemRetrieveReq { .. }
            | Interface::MemReclaim { .. } => {
                // Forward to SWd
                next_world = World::Secure;
                in_msg
            }
            _ => {
                warn!("Unsupported FF-A call from Normal World: {:x?}", in_msg);
                return Err(FfaError::NotSupported);
            }
        };

        Ok((Some(out_msg), next_world))
    }

    pub fn forward_secure_interrupt(&self) -> (SmcReturn, World) {
        let version = self.spmc_version;

        let msg = Interface::Interrupt {
            // The endpoint and vCPU ID fields MBZ in this case
            target_info: TargetInfo {
                endpoint_id: 0,
                vcpu_id: 0,
            },
            // The SPMD shouldn't query the GIC
            interrupt_id: 0,
        };

        exception_free(|token| {
            let mut spmd_state = self.core_local.get().borrow_mut(token);
            assert_eq!(spmd_state.spmc_state, SpmcState::Runtime);
            spmd_state.spmc_state = SpmcState::SecureInterrupt;
        });

        (interface_to_regs(msg, version), World::Secure)
    }
}
