// Copyright The Rusted Firmware-A Contributors.
//
// SPDX-License-Identifier: BSD-3-Clause

//! Safe abstractions for storing data in DRAM sections.

use crate::{
    context::PerCoreState,
    platform::{Platform, PlatformImpl},
};
use core::{cell::RefCell, mem::MaybeUninit};
use percore::{ExceptionLock, PerCore};
use spin::{Lazy, mutex::SpinMutex};
use zerocopy::FromZeros;

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
///     pub FOO, u64, unsafe(link_section = ".bss.dram")
/// }
/// ```
#[allow(unused_macros)]
macro_rules! zeroed_mut {
    ($(#[$attributes:meta])* $visibility:vis $name:ident, $t:ty $(, $raw_attributes:meta)*) => {
        $(#[$attributes])*
        $visibility static $name: spin::mutex::SpinMutex<&'static mut $t> = spin::mutex::SpinMutex::new({
            $(#[$raw_attributes])*
            static mut RAW: $t = $crate::dram::const_zeroed();
            // SAFETY: This is the only place where we create a reference to the contents of this
            // static mut.
            unsafe { &mut *&raw mut RAW }
        });
    };
}
pub(crate) use zeroed_mut;

zeroed_mut!(FOO, u64, unsafe(link_section = ".bss.dram"));

zeroed_mut! {
    /// Some comment.
    pub BAR, u64, unsafe(link_section = ".bss.dram")
}

/// Declares a static lazily-initialised `$t` which may reside in zero-initialised memory.
///
/// For example:
///
/// ```
/// lazy_indirect!(FOO, u64, 42);
/// ```
///
/// will create
///
/// ```
/// static FOO: Lazy<&u64> = ...;
/// ```
///
/// The indirection (via the reference stored in the `Lazy`) allows the value itself to be stored in
/// a different section of memory. Attributes can optionally be provided both for the underlying
/// static and for the `Lazy` wrapper, e.g.:
///
/// ```
/// lazy_indirect! {
///     /// Rustdoc comment for FOO.
///     pub FOO, u64, 42, unsafe(link_section = ".bss.dram")
/// }
/// ```
#[allow(unused_macros)]
macro_rules! lazy_indirect {
    ($(#[$attributes:meta])* $visibility:vis $name:ident, $t:ty, $init:expr $(, $raw_attributes:meta)*) => {
        $(#[$attributes])*
        $visibility static $name: spin::Lazy<&$t> = spin::Lazy::new(|| {
            $(#[$raw_attributes])*
            static mut RAW: core::mem::MaybeUninit<$t> =
                $crate::dram::const_zeroed();
            // SAFETY: This is the only place where we create a reference to the contents of this
            // static mut, and it only happens once during the initialisation of the `Lazy`.
            unsafe { &mut *&raw mut RAW }.write($init)
        });
    };
}

lazy_indirect!(
    LAZY_PERCORE2,
    PerCoreState<u64>,
    PerCore::new([const { ExceptionLock::new(RefCell::new(0)) }; PlatformImpl::CORE_COUNT]),
    unsafe(link_section = ".bss.dram")
);

lazy_indirect!(LAZY_MUTEX2, SpinMutex<u64>, SpinMutex::new(64));

lazy_indirect! {
    /// Some comment
    pub LAZY_MUTEX3, SpinMutex<u64>, SpinMutex::new(64), unsafe(link_section = ".bss.dram")
}

// Four cases, depending on whether:
// 1. T is FromZeros, or needs to be initialised (and so a Lazy is required)
// 2. We want &T, or SpinMutex<&mut T>. (Or we could put the mutex in DRAM, so T=SpinMutex<U>)

// The mutex is in SRAM, value is zero-initialised in DRAM.
static ZEROED: SpinMutex<&mut u64> = {
    static mut RAW: u64 = const_zeroed();
    SpinMutex::new({
        // SAFETY: This is the only place where we create a reference to the contents of this
        // `SyncUnsafeCell`.
        let r = unsafe { &mut *&raw mut RAW };
        r
    })
};

// The mutex is in SRAM, lazily initialised after contents is initialised in DRAM.
static LAZY_WRAPPER: Lazy<SpinMutex<&mut u64>> = {
    static mut RAW: MaybeUninit<u64> = const_zeroed();
    Lazy::new(|| {
        // SAFETY: This is the only place where we create a reference to the contents of this
        // `SyncUnsafeCell`, and it only happens once during the initialisation of the `Lazy`.
        let maybe_uninit = unsafe { &mut *&raw mut RAW };
        SpinMutex::new(maybe_uninit.write(42))
    })
};

// No need for a mutex. Value is in DRAM, lazily initialised.
static LAZY_PERCORE: Lazy<&PerCoreState<u64>> = {
    static mut RAW: MaybeUninit<PerCoreState<u64>> = const_zeroed();
    Lazy::new(|| {
        // SAFETY: This is the only place where we create a reference to the contents of this
        // `SyncUnsafeCell`, and it only happens once during the initialisation of the `Lazy`.
        let maybe_uninit = unsafe { &mut *&raw mut RAW };
        maybe_uninit.write(PerCore::new(
            [const { ExceptionLock::new(RefCell::new(0)) }; PlatformImpl::CORE_COUNT],
        ))
    })
};

// The mutex is in DRAM, lazily initialised with its contents.
static LAZY_MUTEX: Lazy<&SpinMutex<u64>> = {
    static mut RAW: MaybeUninit<SpinMutex<u64>> = const_zeroed();
    Lazy::new(|| {
        // SAFETY: This is the only place where we create a reference to the contents of this
        // `SyncUnsafeCell`, and it only happens once during the initialisation of the `Lazy`.
        let maybe_uninit = unsafe { &mut *&raw mut RAW };
        maybe_uninit.write(SpinMutex::new(42))
    })
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        context::PerCoreState,
        platform::{Platform, PlatformImpl, exception_free},
    };
    use core::cell::RefCell;
    use percore::{ExceptionLock, PerCore};
    use spin::mutex::SpinMutex;

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

    #[test]
    fn lazy() {
        lazy_indirect!(TEST, u64, 42);
        assert_eq!(**TEST, 42);
    }

    #[test]
    fn lazy_percore() {
        lazy_indirect!(
            TEST_PERCORE,
            PerCoreState<u64>,
            PerCore::new(
                [const { ExceptionLock::new(RefCell::new(42)) }; PlatformImpl::CORE_COUNT]
            )
        );

        exception_free(|token| {
            assert_eq!(*TEST_PERCORE.get().borrow_mut(token), 42);
            *TEST_PERCORE.get().borrow_mut(token) = 55;
            assert_eq!(*TEST_PERCORE.get().borrow_mut(token), 55);
        });
    }

    #[test]
    fn lazy_mutex() {
        lazy_indirect!(TEST_MUTEX, SpinMutex<u64>, SpinMutex::new(42));
        assert_eq!(*TEST_MUTEX.lock(), 42);
        *TEST_MUTEX.lock() = 55;
        assert_eq!(*TEST_MUTEX.lock(), 55);
    }
}
