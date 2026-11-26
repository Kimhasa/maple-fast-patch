fn main() {
    // The DLLs this program names are looked for in the Windows system folder only, never
    // next to the exe. The portable exe is often started from a Downloads folder, and a DLL
    // someone else dropped there must not be loaded in place of a system one.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
    }
    tauri_build::build()
}
