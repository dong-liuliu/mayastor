use std::env;

fn main() {
    let spdk_rpath = env::var("DEP_SPDK_BUNDLE_ROOT").unwrap();
    println!("cargo:rustc-link-search=native={}", spdk_rpath);
    println!("cargo:rustc-link-arg=-Wl,-rpath={}", spdk_rpath);
    println!("cargo:rustc-link-lib=dylib=spdk-bundle");
    let spdk_bundle_libs ="-luring -luuid -lIPSec_MB -laio -lcrypto -ldl -lm -lnuma -lrt";
    println!("cargo:rustc-flags={}", spdk_bundle_libs);
}
