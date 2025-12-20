// SPDX-License-Identifier: GPL-3.0-or-later

#![allow(non_camel_case_types, unused)]
include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

impl Default for __user_cap_data_struct {
    fn default() -> Self {
        Self {
            effective: 0,
            permitted: 0,
            inheritable: 0,
        }
    }
}
