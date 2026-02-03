// SPDX-License-Identifier: GPL-3.0-or-later

mod raw_bindings {
    #![allow(non_camel_case_types)]
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

use std::ffi::c_int;

use raw_bindings::_LINUX_CAPABILITY_VERSION_3;
pub(super) use raw_bindings::{__user_cap_data_struct, __user_cap_header_struct, CAP_NET_ADMIN, CAP_NET_BIND_SERVICE};

use super::constants::PID;

impl Default for __user_cap_header_struct {
    #[inline(always)]
    fn default() -> Self {
        Self {
            version: _LINUX_CAPABILITY_VERSION_3,
            pid: *PID as c_int,
        }
    }
}

impl Default for __user_cap_data_struct {
    #[inline(always)]
    fn default() -> Self {
        Self {
            effective: u32::default(),
            permitted: u32::default(),
            inheritable: u32::default(),
        }
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    #![allow(non_snake_case)]

    use std::process::id as pid;

    use super::*;

    #[test]
    fn test__user_cap_header_struct_default() {
        let header = __user_cap_header_struct::default();
        assert_eq!(pid(), header.pid as u32);
        assert_eq!(_LINUX_CAPABILITY_VERSION_3, header.version);
    }

    #[test]
    fn test__user_cap_data_struct_default() {
        let data = __user_cap_data_struct::default();
        assert_eq!(0u32, data.effective);
        assert_eq!(0u32, data.permitted);
        assert_eq!(0u32, data.inheritable);
    }
}
