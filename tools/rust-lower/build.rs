//! Bakes the pinned toolchain's library directory into the binary's rpath, so
//! it finds librustc_driver.so without the caller exporting LD_LIBRARY_PATH.
//! The path is discovered from the compiler actually building this crate, so
//! it always matches the rust-toolchain.toml pin.
fn main() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let out = std::process::Command::new(rustc)
        .arg("--print")
        .arg("sysroot")
        .output()
        .expect("rustc --print sysroot");
    let sysroot = String::from_utf8(out.stdout).expect("utf8 sysroot");
    let lib = format!("{}/lib", sysroot.trim());
    println!("cargo:rustc-link-arg=-Wl,-rpath,{lib}");
    println!("cargo:rerun-if-changed=build.rs");
}
