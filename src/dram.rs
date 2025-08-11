// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

//! Safe abstractions for storing data in DRAM sections.

use core::cell::UnsafeCell;
use zerocopy::{FromBytes, FromZeros};

// TODO: Use `core::cell::SyncUnsafeCell` instead when it is stabilised.
/// Like [`UnsafeCell`] but [`Sync`] if `T` implements `Sync`.
///
/// This is a minimal copy of the experimental type from the core library.
#[derive(FromBytes)]
#[repr(transparent)]
pub struct SyncUnsafeCell<T> {
    value: UnsafeCell<T>,
}

#[allow(dead_code)]
impl<T> SyncUnsafeCell<T> {
    /// Gets a pointer to the wrapped value.
    ///
    /// The caller is responsible for avoiding concurrent mutable access through this.
    pub const fn get(&self) -> *mut T {
        self.value.get()
    }
}

// SAFETY: `SyncUnsafeCell::get` allows access to a mutable pointer from a shared reference to
// `SyncUnsafeCell`, but it is up to whoever dereferences the pointer to enforce aliasing rules.
unsafe impl<T: Sync> Sync for SyncUnsafeCell<T> {}

/// Creates a zeroed instance of the given type.
///
/// This is equivalent to `FromZeroes::new_zeroed()` but const.
pub const fn const_zeroed<T: FromZeros>() -> T {
    // SAFETY: T implements `FromZeros` so it must be safe to initialise with zeros.
    unsafe { core::mem::zeroed() }
}

/// Declares a static zero-initialised `$t`, and a SpinMutex initialised with a mutable reference to
/// it. E.g.,
///
/// ```
/// zeroed_mut!(FOO, u64);
/// ```
///
/// will create
///
/// ```
/// static FOO: SpinMutex<&'static mut u64> = ...;
/// ```
///
/// For this to work, `$t` must implement `zerocopy::FromZeroes`.
///
/// Attributes can optionally be provided both for the underlying static and for the `SpinMutex`
/// wrapper, e.g.:
///
/// ```
/// zeroed_mut! {
///     /// Rustdoc comment for FOO.
///     pub FOO, u64, unsafe(link_section = ".dram_zeroed")
/// }
/// ```
#[allow(unused_macros)]
macro_rules! zeroed_mut {
    ($(#[$attributes:meta])* $name:ident, $t:ty $(, $raw_attributes:meta)*) => {
        $(#[$attributes])*
        static $name: spin::mutex::SpinMutex<&'static mut $t> = spin::mutex::SpinMutex::new({
            $(#[$raw_attributes])*
            static RAW: $crate::dram::SyncUnsafeCell<$t> = $crate::dram::const_zeroed();
            // SAFETY: This is the only place where we create a reference to the contents of this
            // `SyncUnsafeCell`.
            unsafe { &mut *RAW.get() }
        });
    };
    ($(#[$attributes:meta])* pub $name:ident, $t:ty $(, $raw_attributes:meta)*) => {
        $(#[$attributes])*
        pub static $name: spin::mutex::SpinMutex<&'static mut $t> = spin::mutex::SpinMutex::new({
            $(#[$raw_attributes])*
            static RAW: $crate::dram::SyncUnsafeCell<$t> = $crate::dram::const_zeroed();
            // SAFETY: This is the only place where we create a reference to the contents of this
            // `SyncUnsafeCell`.
            unsafe { &mut *RAW.get() }
        });
    };
}

zeroed_mut!(FOO, u64, unsafe(link_section = ".dram_zeroed"));

zeroed_mut! {
    /// Some comment.
    pub BAR, u64, unsafe(link_section = ".dram_zeroed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn use_zeroed() {
        static TEST_ZEROED: [u8; 100] = const_zeroed();

        assert_eq!(TEST_ZEROED[0], 0);
        assert_eq!(TEST_ZEROED[99], 0);
    }

    #[test]
    fn mutable_macro() {
        zeroed_mut!(TEST, u64);

        let mut test_ref = TEST.lock();
        assert_eq!(**test_ref, 0);
        **test_ref = 42;
        assert_eq!(**test_ref, 42);
    }
}
