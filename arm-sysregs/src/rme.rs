// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

/// Wrapper type around the GPCC_EL3 register.
pub struct GpccReg(pub u64);

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

macro_rules! cast {
    ($e:expr, bool $(, $rev:tt)?) => {
        cast!($e, {
            0 => false,
            1 => true,
        } $(, $rev)?)
    };
    ($e:expr, share $(, $rev:tt)?) => {
        cast!($e, {
            0b00 => Shareability::Non,
            0b10 => Shareability::Outer,
            0b11 => Shareability::Inner,
        } $(, $rev)?)
    };
    ($e:expr, cache $(, $rev:tt)?) => {
        cast!($e, {
            0b00 => Cacheability::Non,
            0b01 => Cacheability::WriteBackAllocate,
            0b10 => Cacheability::WriteThrough,
            0b11 => Cacheability::WriteBackNoAllocate,
        } $(, $rev)?)
    };
    ($e:expr, {$($bits:pat => $value:expr, )+}) => {
            match $e {
            $($bits => $value), +,
            #[allow(unreachable_patterns)]
            _ => panic!(),
        }
    };
    ($e:expr, {$($bits:expr => $value:pat, )+}, rev) => {
            match $e {
            $($value => $bits), +,
            #[allow(unreachable_patterns)]
            _ => panic!(),
        }
    }
}

macro_rules! field {
    ([$end:literal:$start:literal], $name:ident, $ty:ty, $cast:tt) => {
        #[doc = "Returns the "]
        #[doc = stringify!($name)]
        #[doc = "field of the register."]
        pub const fn $name(&self) -> $ty {
            cast!((self.0 >> $start) & ((1 << ($end - $start)) - 1), $cast)
        }

        paste::paste! {
            #[allow(clippy::identity_op)]
            #[doc = "Updates the "]
            #[doc = stringify!($name)]
            #[doc = "field of the register."]
            pub const fn [<set_ $name>](&mut self, value: $ty) {
                let val = cast!(value, $cast, rev);
                self.0 = (self.0 & !(((1 << ($end - $start)) - 1) << $start)) | (val << $start);
            }
        }
    };
}

#[allow(dead_code)]
impl GpccReg {
    field!([25:24], appsaa, bool, bool);

    field!([24:20], l0gptsz, u64, {
        0b0000 => 30,
        0b0100 => 34,
        0b0110 => 36,
        0b1001 => 39,
    });

    field!([20:19], nso, bool, bool);
    field!([19:18], tbgpcd, bool, bool);
    field!([18:17], gpcp, bool, bool);
    field!([17:16], gpc, bool, bool);

    field!([16:14], pgs, u64, {
        0b00 => 12,
        0b01 => 16,
        0b10 => 14,
    });

    field!([14:12], sh, Shareability, share);

    field!([12:10], orgn, Cacheability, cache);
    field!([10:8], irgn, Cacheability, cache);

    field!([8:7], nspad, bool, bool);
    field!([7:6], rlpad, bool, bool);

    field!([3:0], pps, u64, {
        0b000 => 32,
        0b001 => 36,
        0b010 => 40,
        0b011 => 42,
        0b100 => 44,
        0b101 => 48,
        0b110 => 52,
    });
}
