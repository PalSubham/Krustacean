// SPDX-License-Identifier: GPL-3.0-or-later

mod raw_bindings {
    #![allow(non_camel_case_types)]
    include!(concat!(env!("OUT_DIR"), "/cap_bindings.rs"));
}

pub(super) use raw_bindings::{__user_cap_data_struct, __user_cap_header_struct, _LINUX_CAPABILITY_VERSION_3, CAP_NET_ADMIN, CAP_NET_BIND_SERVICE};

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

/// Index of the [`__user_cap_data_struct`] which holds this capability in the 2-element array
pub(super) const fn cap_to_index(x: u32) -> usize {
    (x >> 5u32) as usize
}

/// Mask to find if the capability is enabled in a [`__user_cap_data_struct`] field
pub(super) const fn cap_to_mask(x: u32) -> u32 {
    1u32 << (x & 31u32)
}

#[cfg(test)]
mod tests {
    use super::{cap_to_index, cap_to_mask};

    #[test]
    fn test_cap_to_index() {
        for cap in 0u32..=63u32 {
            if cap <= 31u32 {
                assert_eq!(0usize, cap_to_index(cap));
            } else {
                assert_eq!(1usize, cap_to_index(cap));
            }
        }
    }

    #[test]
    fn test_cap_to_mask() {
        for cap in 0u32..=63u32 {
            assert_eq!(1u32 << (cap % 32u32), cap_to_mask(cap));
        }
    }
}
