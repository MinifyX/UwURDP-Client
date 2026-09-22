fn main() {
    // UwURDP runs from a folder the user can write to, and gets started from
    // wherever a shortcut points: linked DLLs come from System32 only, never
    // from the app's own folder. The runtime half is in `lib.rs`.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
    }
    tauri_build::build()
}
