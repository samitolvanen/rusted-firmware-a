// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

use bitflags::bitflags;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Allowed Shareability attributes.
#[allow(missing_docs)]
pub enum Shareability {
    Non,
    Outer,
    Inner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Allowed Cacheability attributes.
#[allow(missing_docs)]
pub enum Cacheability {
    Non,
    WriteBackAllocate,
    WriteThrough,
    WriteBackNoAllocate,
}

impl Cacheability {
    const fn from_bits(value: u64) -> Self {
        match value {
            0b00 => Cacheability::Non,
            0b01 => Cacheability::WriteBackAllocate,
            0b10 => Cacheability::WriteThrough,
            0b11 => Cacheability::WriteBackNoAllocate,
            _ => panic!(),
        }
    }

    const fn to_bits(self) -> u64 {
        match self {
            Cacheability::Non => 0b00,
            Cacheability::WriteBackAllocate => 0b01,
            Cacheability::WriteThrough => 0b10,
            Cacheability::WriteBackNoAllocate => 0b11,
        }
    }
}

bitflags! {
    /// GPCC_EL3, the control register for Granule Protection Checks..
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    #[repr(transparent)]
    pub struct GpccEl3: u64 {
        /// Above PPS All Access. This field governs the behavior of memory accesses to Secure,
        /// Realm and Root PA space, for physical addresses above the range configured by
        /// GPCCR_EL3.PPS.
        const APPSAA = 1 << 24;

        /// Non-secure Only. This field governs the behavior of the GPI encoding for NSO.
        const NSO = 1 << 19;

        /// Trace Buffer Granule Protection Check Disabled. Controls whether the Trace Buffer Unit
        /// accepts or rejects when Granule Protection Checks are disabled.
        const TBGPCD = 1 << 18;

        /// Granule Protection Check Priority.
        ///
        /// This control governs behavior of granule protection checks on fetches of stage 2 Table
        /// descriptors.
        const GPCP = 1 << 17;

        /// Granule Protection Check Enable.
        const GPC = 1 << 16;

        /// Secure PA space Disable. This field controls access to the Secure PA space.
        const SPAD = 1 << 7;
        /// Non-secure PA space Disable. This field controls access to the Non-secure PA space.
        const NSPAD = 1 << 6;
        /// Realm PA space Disable. This field controls access to the Realm PA space.
        const RLPAD = 1 << 5;
    }
}

#[allow(dead_code)]
impl GpccEl3 {
    const L0GPTSZ_SHIFT: u64 = 20;
    const L0GPTSZ_MASK: u64 = 0b1111;

    const PGS_SHIFT: u64 = 14;
    const PGS_MASK: u64 = 0b11;

    const SH_SHIFT: u64 = 12;
    const SH_MASK: u64 = 0b11;

    const ORGN_SHIFT: u64 = 10;
    const ORGN_MASK: u64 = 0b11;

    const IRGN_SHIFT: u64 = 8;
    const IRGN_MASK: u64 = 0b11;

    const PPS_SHIFT: u64 = 0;
    const PPS_MASK: u64 = 0b1111;

    /// Level 0 GPT entry size.
    ///
    /// This field advertises the number of least-significant address bits protected by each entry
    /// in the level 0 GPT.
    pub const fn l0gptsz(&self) -> u64 {
        match (self.bits() >> Self::L0GPTSZ_SHIFT) & Self::L0GPTSZ_MASK {
            0b0000 => 30,
            0b0100 => 34,
            0b0110 => 36,
            0b1001 => 39,
            _ => panic!(),
        }
    }

    /// See [Self::l0gptsz].
    pub const fn set_l0gptsz(&mut self, value: u64) {
        let val = match value {
            30 => 0b0000,
            34 => 0b0100,
            36 => 0b0110,
            39 => 0b1001,
            _ => panic!(),
        };

        self.0.0 = (self.bits() & !(Self::L0GPTSZ_MASK << Self::L0GPTSZ_SHIFT))
            | (val << Self::L0GPTSZ_SHIFT);
    }

    /// Physical Granule size.
    pub const fn pgs(&self) -> u64 {
        match (self.bits() >> Self::PGS_SHIFT) & Self::PGS_MASK {
            0b00 => 12,
            0b01 => 16,
            0b10 => 14,
            _ => panic!(),
        }
    }

    /// See [Self::pgs].
    pub const fn set_pgs(&mut self, value: u64) {
        let val = match value {
            12 => 0b00,
            16 => 0b01,
            14 => 0b10,
            _ => panic!(),
        };

        self.0.0 = (self.bits() & !(Self::PGS_MASK << Self::PGS_SHIFT)) | (val << Self::PGS_SHIFT);
    }

    /// GPT fetch Shareability attribute.
    pub const fn sh(&self) -> Shareability {
        match (self.bits() >> Self::SH_SHIFT) & Self::SH_MASK {
            0b00 => Shareability::Non,
            0b10 => Shareability::Outer,
            0b11 => Shareability::Inner,
            _ => panic!(),
        }
    }
    /// See [Self::sh].
    pub const fn set_sh(&mut self, value: Shareability) {
        let val = match value {
            Shareability::Non => 0b00,
            Shareability::Outer => 0b10,
            Shareability::Inner => 0b11,
        };
        self.0.0 = (self.bits() & !(Self::SH_MASK << Self::SH_SHIFT)) | (val << Self::SH_SHIFT);
    }

    /// GPT fetch Outer cacheability attribute.
    pub const fn orgn(&self) -> Cacheability {
        Cacheability::from_bits((self.bits() >> Self::ORGN_SHIFT) & Self::ORGN_MASK)
    }

    /// See [Self::orgn].
    pub const fn set_orgn(&mut self, value: Cacheability) {
        self.0.0 = (self.bits() & !(Self::ORGN_MASK << Self::ORGN_SHIFT)) | (value.to_bits() << 10);
    }

    /// GPT fetch Inner cacheability attribute.
    pub const fn irgn(&self) -> Cacheability {
        Cacheability::from_bits((self.bits() >> Self::IRGN_SHIFT) & Self::IRGN_MASK)
    }

    /// See [Self::irgn].
    pub const fn set_irgn(&mut self, value: Cacheability) {
        self.0.0 = (self.bits() & !(Self::IRGN_MASK << Self::IRGN_SHIFT)) | (value.to_bits() << 10);
    }

    /// Protected Physical Address Size.
    ///
    /// The size of the memory region protected by GPTBR_EL3, in terms of the number of
    /// least-significant address bits.
    pub const fn pps(&self) -> u64 {
        match (self.bits() >> Self::PPS_SHIFT) & Self::PPS_MASK {
            0b000 => 32,
            0b001 => 36,
            0b010 => 40,
            0b011 => 42,
            0b100 => 44,
            0b101 => 48,
            0b110 => 52,
            _ => panic!(),
        }
    }

    /// See [Self::pps].
    pub const fn set_pps(&mut self, value: u64) {
        let val = match value {
            32 => 0b000,
            36 => 0b001,
            40 => 0b010,
            42 => 0b011,
            44 => 0b100,
            48 => 0b101,
            52 => 0b110,
            _ => panic!(),
        };

        self.0.0 = (self.bits() & !(Self::PPS_MASK << Self::PPS_SHIFT)) | (val << Self::PPS_SHIFT);
    }
}
