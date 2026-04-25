// SPDX-License-Identifier: GPL-3.0-or-later

use std::{env, path::PathBuf};

fn main() {
    println!("cargo::rerun-if-changed=wrappers/cap_wrapper.h");

    println!("cargo::rustc-link-lib=dylib=cap");

    let target_os = env::var("CARGO_CFG_TARGET_OS").expect("CARGO_CFG_TARGET_OS not set");

    if target_os != "linux" {
        panic!("This build is only intended for Linux!");
    } else {
        let out_path = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));

        bindgen::Builder::default()
            .clang_arg("-fretain-comments-from-system-headers")
            .header("wrappers/cap_wrapper.h")
            .allowlist_function("cap_get_proc")
            .allowlist_function("cap_free")
            .allowlist_function("cap_get_flag")
            .allowlist_var("CAP_NET_BIND_SERVICE")
            .allowlist_var("CAP_NET_ADMIN")
            .rustified_enum(".*")
            .derive_debug(false)
            .derive_default(false)
            .derive_eq(false)
            .derive_hash(false)
            .derive_ord(false)
            .derive_partialeq(false)
            .derive_partialord(false)
            .generate_comments(true)
            .generate()
            .expect("Unable to generate bindings for wrappers/cap_wrapper.h")
            .write_to_file(out_path.join("cap_bindings.rs"))
            .expect("Couldn't write to cap_bindings.rs");
    }
}
