// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use num_enum::{TryFromPrimitive, TryFromPrimitiveError};

use crate::{context::World, smccc::SmcReturn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    MalformedCommand,
}

impl<T: TryFromPrimitive> From<TryFromPrimitiveError<T>> for Error {
    fn from(_: TryFromPrimitiveError<T>) -> Self {
        Self::MalformedCommand
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, TryFromPrimitive)]
#[repr(u64)]
pub enum RecCommandReturnCode {
    Ok = 0,
    Unk = 0xFFFF_FFFF,
    BadAddress = 0xFFFF_FFFE,
    BadPas = 0xFFFF_FFFD,
    NoMemory = 0xFFFF_FFFC,
    InvalidValue = 0xFFFF_FFFB,
    Again = 0xFFFF_FFFA,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, TryFromPrimitive)]
#[repr(u64)]
pub enum EccCurve {
    EccSecp384r1 = 0,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, TryFromPrimitive)]
#[repr(u64)]
pub enum El3TokenSignOpcode {
    Push = 0,
    Pull = 1,
    GetRak = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, TryFromPrimitive)]
#[repr(u64)]
pub enum MecRefreshReason {
    RealmCreation = 0,
    RealmDestruction = 1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamSelector {
    pub keyset: bool,
    pub dir: bool,
    pub substream: u8,
    pub stream_id: u8,
}

impl StreamSelector {
    fn from_reg(reg: u64) -> Self {
        Self {
            keyset: reg & (1 << 12) == (1 << 12),
            dir: reg & (1 << 11) == (1 << 11),
            substream: ((reg >> 8) & 0b111) as u8,
            stream_id: reg as u8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecCall {
    RmiReqComplete {
        regs: [u64; 6],
    },
    GtsiDelegate {
        base_pa: u64,
    },
    GtsiUndelegate {
        base_pa: u64,
    },
    AttestGetRealmKey {
        buf_pa: u64,
        buf_size: u64,
        ecc_curve: EccCurve,
    },
    AttestGetPlatToken {
        buf_pa: u64,
        buf_size: u64,
        c_size: u64,
    },
    El3Features {
        feat_reg_idx: u64,
    },
    El3TokenSign {
        opcode: El3TokenSignOpcode,
        buf_pa: u64,
        buf_size: u64,
        ecc_curve: EccCurve,
    },
    MecRefresh {
        mecid: u16,
        reason: MecRefreshReason,
    },
    IdeKeyProg {
        ecam_address: u64,
        rp_id: u64,
        stream: StreamSelector,
        keq_qw: [u64; 4],
        ifv_qw: [u64; 2],
        request_id: u64,
        cookie: u64,
    },
    IdeKeySetGo {
        ecam_address: u64,
        rp_id: u64,
        stream: StreamSelector,
        request_id: u64,
        cookie: u64,
    },
    IdeKeySetStop {
        ecam_address: u64,
        rp_id: u64,
        stream: StreamSelector,
        request_id: u64,
        cookie: u64,
    },
    IdeKmPullResponse {
        ecam_address: u64,
        rp_id: u64,
    },
    ReserveMemory {
        size: u64,
        alignment: u8,
        local_cpu: bool,
    },
}

impl RecCall {
    pub fn from_regs(regs: &[u64]) -> Result<Self, Error> {
        Ok(match regs[0] {
            0xC400_018F => Self::RmiReqComplete {
                regs: regs[1..7].try_into().unwrap(),
            },
            0xC400_01B0 => Self::GtsiDelegate { base_pa: regs[1] },
            0xC400_01B1 => Self::GtsiUndelegate { base_pa: regs[1] },
            0xC400_01B2 => Self::AttestGetRealmKey {
                buf_pa: regs[1],
                buf_size: regs[2],
                ecc_curve: regs[3].try_into()?,
            },
            0xC400_01B3 => Self::AttestGetPlatToken {
                buf_pa: regs[1],
                buf_size: regs[2],
                c_size: regs[3],
            },
            0xC400_01B4 => Self::El3Features {
                feat_reg_idx: regs[1],
            },
            0xC400_01B5 => Self::El3TokenSign {
                opcode: regs[1].try_into()?,
                buf_pa: regs[2],
                buf_size: regs[3],
                ecc_curve: regs[4].try_into()?,
            },
            0xC400_01B6 => Self::MecRefresh {
                mecid: regs[1] as u16,
                reason: regs[2].try_into()?,
            },
            0xC400_01B7 => Self::IdeKeyProg {
                ecam_address: regs[1],
                rp_id: regs[2],
                stream: StreamSelector::from_reg(regs[3]),
                keq_qw: [regs[4], regs[5], regs[6], regs[7]],
                ifv_qw: [regs[8], regs[9]],
                request_id: regs[10],
                cookie: regs[12],
            },
            0xC400_01B8 => Self::IdeKeySetGo {
                ecam_address: regs[1],
                rp_id: regs[2],
                stream: StreamSelector::from_reg(regs[3]),
                request_id: regs[4],
                cookie: regs[5],
            },
            0xC400_01B99 => Self::IdeKeySetStop {
                ecam_address: regs[1],
                rp_id: regs[2],
                stream: StreamSelector::from_reg(regs[3]),
                request_id: regs[4],
                cookie: regs[5],
            },
            0xC400_01BA => Self::IdeKmPullResponse {
                ecam_address: regs[1],
                rp_id: regs[2],
            },
            0xC400_01BB => Self::ReserveMemory {
                size: regs[1],
                alignment: (regs[2] >> 56) as u8,
                local_cpu: regs[3] & 0b1 == 0b1,
            },
            _ => return Err(Error::MalformedCommand),
        })
    }
}

pub trait ToSmcReturn {
    fn to_regs(&self, _regs: &mut [u64]);
}

macro_rules! derive_to_smc_return {
    ($name:ident $(,$field:ident)*) => {
        impl From<$name> for (SmcReturn, World) {
            #[allow(unused_variables)]
            fn from(value: $name) -> Self {
                let mut regs = [0; 18];
                regs[0] = RecCommandReturnCode::Ok as u64;

                #[allow(unused)]
                let rem_regs = &mut regs[1..];
                $(
                    rem_regs[0] = value.$field as u64;
                    #[allow(unused)]
                    let rem_regs = &mut rem_regs[1..];
                )*

                (regs.into(), World::Realm)
            }
        }
    };
}

impl ToSmcReturn for RecCommandReturnCode {
    fn to_regs(&self, regs: &mut [u64]) {
        regs[0] = *self as u64;
    }
}
impl From<RecCommandReturnCode> for (SmcReturn, World) {
    fn from(value: RecCommandReturnCode) -> Self {
        let mut regs = [0; 18];
        value.to_regs(&mut regs);
        (regs.into(), World::Realm)
    }
}

pub struct RecEmptyResponse;
derive_to_smc_return!(RecEmptyResponse);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecAttestGetRealmKeyResponse {
    pub key_size: u64,
}
derive_to_smc_return!(RecAttestGetRealmKeyResponse, key_size);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecAttestGetPlatTokenResponse {
    pub token_hunk_size: u64,
    pub remaining_size: u64,
}
derive_to_smc_return!(
    RecAttestGetPlatTokenResponse,
    token_hunk_size,
    remaining_size
);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecEl3FeaturesResponse {
    pub feat_reg: u64,
}
derive_to_smc_return!(RecEl3FeaturesResponse, feat_reg);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecEl3TokenSignGetRakResponse {
    pub key_size: u64,
}
derive_to_smc_return!(RecEl3TokenSignGetRakResponse, key_size);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecIdeKmPullResponse {
    pub previous: RecCommandReturnCode,
    pub r1: u64,
    pub r2: u64,
}
derive_to_smc_return!(RecIdeKmPullResponse, previous, r1, r2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecReserveMemoryResponse {
    pub address: u64,
}
derive_to_smc_return!(RecReserveMemoryResponse, address);
