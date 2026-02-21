// SPDX-License-Identifier: GPL-3.0-or-later

use std::{env, path::PathBuf};

fn main() {
    println!("cargo::rerun-if-changed=wrapper.h");

    let target_os = env::var("CARGO_CFG_TARGET_OS").expect("CARGO_CFG_TARGET_OS not set");

    if target_os != "linux" {
        panic!("This build is only intended for Linux!");
    } else {
        let bindings = bindgen::Builder::default()
            .clang_arg("-fretain-comments-from-system-headers")
            .header("wrapper.h")
            .allowlist_type("__user_cap_header_struct")
            .allowlist_type("__user_cap_data_struct")
            .allowlist_var("_LINUX_CAPABILITY_VERSION_3")
            .allowlist_var("CAP_NET_BIND_SERVICE")
            .allowlist_var("CAP_NET_ADMIN")
            .derive_copy(false)
            .derive_debug(false)
            .derive_default(false)
            .generate_comments(true)
            .generate()
            .expect("Unable to generate bindings for wrapper.h");

        let out_path = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));

        bindings
            .write_to_file(out_path.join("bindings.rs"))
            .expect("Couldn't write to bindings.rs");
    }
}
