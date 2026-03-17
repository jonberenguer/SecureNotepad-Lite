// build.rs
//
// On Windows release builds, instruct the linker to set the subsystem to
// "windows" so no console window appears when the app is launched.
//
// Debug builds are left unchanged so error output remains visible.
// This approach is safer than #![windows_subsystem = "windows"] in main.rs
// because that attribute silences ALL output including panics and our
// fatal_error() MessageBox calls during startup failures.

fn main() {
    #[cfg(target_os = "windows")]
    if std::env::var("PROFILE").unwrap_or_default() == "release" {
        println!("cargo:rustc-link-arg-bins=/SUBSYSTEM:windows");
        println!("cargo:rustc-link-arg-bins=/ENTRY:mainCRTStartup");
    }
}
