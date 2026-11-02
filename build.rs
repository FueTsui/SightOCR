use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=rust/app.manifest");
    println!("cargo:rerun-if-changed=resources/oneocr/oneocr.onemodel");

    // Tests that need the OneOCR files are reported as ignored, not passed,
    // when the files are missing (for example in public CI).
    println!("cargo:rustc-check-cfg=cfg(has_oneocr)");
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    if root.join("resources/oneocr/oneocr.onemodel").is_file() {
        println!("cargo:rustc-cfg=has_oneocr");
    }

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        // Resolve static imports (dwmapi, uxtheme, opengl32, ...) from System32
        // only. Setup runs elevated from Downloads or %TEMP%, where a planted DLL
        // would otherwise be loaded first. 0x800 = LOAD_LIBRARY_SEARCH_SYSTEM32.
        println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
    }
    let manifest = versioned_manifest(&root);
    winresource::WindowsResource::new()
        .set_icon("assets/icon.ico")
        .set_manifest_file(manifest.to_str().expect("OUT_DIR is not valid UTF-8"))
        .set("ProductName", "SightOCR")
        .set("FileDescription", "SightOCR")
        .set("CompanyName", "FueTsui")
        .set(
            "LegalCopyright",
            "Copyright 2026 FueTsui. Licensed under the MIT License.",
        )
        .compile()
        .expect("failed to compile Windows application resources");
}

/// Writes rust/app.manifest with the package version so the two never drift.
fn versioned_manifest(root: &Path) -> PathBuf {
    let template = std::fs::read_to_string(root.join("rust/app.manifest"))
        .expect("failed to read rust/app.manifest");
    assert!(
        template.contains("@VERSION@"),
        "rust/app.manifest must use the @VERSION@ placeholder"
    );
    let version = format!(
        "{}.{}.{}.0",
        std::env::var("CARGO_PKG_VERSION_MAJOR").unwrap(),
        std::env::var("CARGO_PKG_VERSION_MINOR").unwrap(),
        std::env::var("CARGO_PKG_VERSION_PATCH").unwrap(),
    );
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("app.manifest");
    std::fs::write(&out, template.replace("@VERSION@", &version))
        .expect("failed to write the versioned manifest");
    out
}
