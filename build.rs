use std::env;

fn main() {
    // Source checkout root for `hark update` (dev rebuild command). CARGO
    // sets this to the manifest dir on every build.
    if let Some(manifest) = env::var_os("CARGO_MANIFEST_DIR") {
        println!("cargo:rustc-env=HARK_SRC_ROOT={}", manifest.to_string_lossy());
    }
    println!("cargo:rerun-if-changed=build.rs");
}
