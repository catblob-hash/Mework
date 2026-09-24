//! `Mework Helper`: the executable a bundled Mework launches Chromium's renderer, GPU and utility
//! processes from on macOS. It is copied into the `Mework Helper*.app` bundles next to the
//! framework (scripts/stage-macos-cef.mjs); development builds use the application executable
//! instead. Nothing else of Mework runs here.

#[cfg(target_os = "macos")]
fn main() {
    std::process::exit(mework_lib::cef_helper_main());
}

#[cfg(not(target_os = "macos"))]
fn main() {}
