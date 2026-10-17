fn main() {
    // libslirp from Homebrew (brew install libslirp).
    let prefix = std::process::Command::new("brew")
        .args(["--prefix", "libslirp"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/opt/homebrew/opt/libslirp".into());
    println!("cargo:rustc-link-search=native={prefix}/lib");

    // csrc/asyncclose.c: close() interposer (see the file). dyld only honours
    // __interpose tuples from dylibs, not from the main executable, so it is a
    // dylib the binary links (and finds next to itself).
    let out = std::env::var("OUT_DIR").unwrap();
    let profile_dir = std::path::Path::new(&out).ancestors().nth(3).unwrap().to_path_buf();
    let dylib = profile_dir.join("liborbitclose.dylib");
    let ok = std::process::Command::new("cc")
        .args([
            "-O2",
            "-Wall",
            "-dynamiclib",
            "-install_name",
            "@executable_path/liborbitclose.dylib",
            "csrc/asyncclose.c",
            "-o",
        ])
        .arg(&dylib)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(ok, "building liborbitclose.dylib failed");
    println!("cargo:rustc-link-arg-bins={}", dylib.display());
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=csrc/asyncclose.c");
}
