fn main() {
    println!("cargo:rerun-if-changed=src/gpod_shim.c");
    println!("cargo:rerun-if-changed=src/gpod_shim.h");

    let libgpod = pkg_config::Config::new()
        .atleast_version("0.8.3")
        .cargo_metadata(false)
        .probe("libgpod-1.0")
        .expect("libgpod development files were not found (Arch: pacman -S libgpod)");
    let glib = pkg_config::Config::new()
        .cargo_metadata(false)
        .probe("glib-2.0")
        .expect("GLib development files were not found");

    let mut build = cc::Build::new();
    build.file("src/gpod_shim.c");
    for include in libgpod.include_paths.iter().chain(&glib.include_paths) {
        build.include(include);
    }
    build.warnings(true).compile("copypod_gpod_shim");

    // Emit dynamic dependencies after the static shim so --as-needed linkers
    // see the shim's libgpod/GLib symbols before considering these libraries.
    pkg_config::Config::new()
        .atleast_version("0.8.3")
        .probe("libgpod-1.0")
        .unwrap();
    pkg_config::Config::new().probe("glib-2.0").unwrap();
}
