// SPDX-License-Identifier: GPL-3.0-or-later

mod raw_bindings {
    #![allow(non_camel_case_types)]
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

use raw_bindings::_LINUX_CAPABILITY_VERSION_3;
pub(super) use raw_bindings::{__user_cap_data_struct, __user_cap_header_struct, CAP_NET_ADMIN, CAP_NET_BIND_SERVICE};
use std::{os::raw::c_int, process::id as pid};

impl Default for __user_cap_header_struct {
    fn default() -> Self {
        Self {
            version: _LINUX_CAPABILITY_VERSION_3,
            pid: pid() as c_int,
        }
    }
}

impl Default for __user_cap_data_struct {
    fn default() -> Self {
        Self {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        }
    }
}
