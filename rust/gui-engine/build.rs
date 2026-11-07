use std::env;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // `free_unix` names the systems whose desktop is X11 or Wayland with
    // D-Bus services, which this crate drives the same way on each.
    println!("cargo::rustc-check-cfg=cfg(free_unix)");
    if matches!(
        env::var("CARGO_CFG_TARGET_OS").as_deref(),
        Ok("linux" | "freebsd")
    ) {
        println!("cargo::rustc-cfg=free_unix");
    }
}
