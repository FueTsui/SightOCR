fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=rust/app.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/icon.ico")
            .set_manifest_file("rust/app.manifest")
            .set("ProductName", "SightOCR")
            .set("FileDescription", "SightOCR")
            .set("CompanyName", "FueTsui")
            .set(
                "LegalCopyright",
                "Copyright 2026 FueTsui. All rights reserved.",
            )
            .compile()
            .expect("failed to compile Windows application resources");
    }
}
