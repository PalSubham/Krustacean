// SPDX-License-Identifier: GPL-3.0-or-later

mod raw_bindings {
    #![allow(non_camel_case_types, dead_code)]
    include!(concat!(env!("OUT_DIR"), "/cap_bindings.rs"));
}

use std::io::{Error, Result};

use raw_bindings::*;

pub(super) type CapFlag = cap_flag_t;

pub(super) fn check_cap<const N: usize>(required_caps: &[u32; N], flag: CapFlag) -> Result<bool> {
    let caps = unsafe { cap_get_proc() };

    if caps.is_null() {
        Err(Error::last_os_error())
    } else {
        let res = required_caps.iter().try_fold(true, |acc, &cap| {
            let mut value = cap_flag_value_t::CAP_CLEAR; // Dummy initialization, otherwise the compiler complains

            match unsafe { cap_get_flag(caps, cap as _, flag, &mut value as *mut _) } {
                0 => Ok(acc && (value == cap_flag_value_t::CAP_SET)),
                -1 => Err(Error::last_os_error()),
                _ => unreachable!("Impossible return values from cap_get_flag"),
            }
        });

        unsafe {
            cap_free(caps as *mut _);
        };
        res
    }
}

pub(super) use raw_bindings::{CAP_NET_ADMIN, CAP_NET_BIND_SERVICE};
