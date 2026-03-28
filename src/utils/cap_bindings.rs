// SPDX-License-Identifier: GPL-3.0-or-later

mod raw_bindings {
    #![allow(non_camel_case_types)]
    include!(concat!(env!("OUT_DIR"), "/cap_bindings.rs"));
}

use std::ffi::c_int;

pub use raw_bindings::_LINUX_CAPABILITY_VERSION_3;
pub(super) use raw_bindings::{__user_cap_data_struct, __user_cap_header_struct, CAP_NET_ADMIN, CAP_NET_BIND_SERVICE};

use super::constants::PID;

impl Default for __user_cap_data_struct {
    #[inline(always)]
    fn default() -> Self {
        Self {
            effective: Default::default(),
            permitted: Default::default(),
            inheritable: Default::default(),
        }
    }
}

impl Default for __user_cap_header_struct {
    #[inline(always)]
    fn default() -> Self {
        Self {
            version: _LINUX_CAPABILITY_VERSION_3,
            pid: *PID as c_int,
        }
    }
}

/// Index of the [`__user_cap_data_struct`] which holds this capability in the 2-element array
macro_rules! cap_to_index {
    ($x:expr) => {{ (($x as u32) >> 5u32) as usize }};
}

/// Mask to find if the capability is enabled in a [`__user_cap_data_struct`] field
macro_rules! cap_to_mask {
    ($x:expr) => {{ 1u32 << (($x as u32) & 31u32) }};
}

pub(super) use {cap_to_index, cap_to_mask};

#[cfg(test)]
mod tests {
    use super::{cap_to_index, cap_to_mask};

    #[test]
    fn test_cap_to_index() {
        for cap in 0u32..=63u32 {
            if cap <= 31u32 {
                assert_eq!(0usize, cap_to_index!(cap));
            } else {
                assert_eq!(1usize, cap_to_index!(cap));
            }
        }
    }

    #[test]
    fn test_cap_to_mask() {
        for cap in 0u32..=63u32 {
            assert_eq!(1u32 << (cap % 32u32), cap_to_mask!(cap));
        }
    }
}
