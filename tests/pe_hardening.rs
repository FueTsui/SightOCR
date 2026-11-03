//! Checks the built executables against DLL planting (CWE-427).
//!
//! Setup runs elevated from Downloads or %TEMP%. Windows resolves static imports
//! that are not KnownDLLs (dwmapi, uxtheme, opengl32, VCRUNTIME140, ...) from the
//! executable's own folder first, so every program must (1) ask the loader to use
//! System32 only and (2) not import the non-system C runtime DLLs.
//!
//! The DependentLoadFlags linker option exists only for the MSVC toolchain, so on
//! other toolchains these tests are reported as ignored rather than passed.

use std::path::Path;

const LOAD_LIBRARY_SEARCH_SYSTEM32: u16 = 0x800;

struct Pe {
    bytes: Vec<u8>,
    sections: Vec<(u32, u32, u32)>, // (virtual address, size, raw offset)
    directories: usize,
    pe32_plus: bool,
}

impl Pe {
    fn open(path: &Path) -> Self {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(&bytes[..2], b"MZ", "{} is not a PE file", path.display());
        let pe = u32_at(&bytes, 0x3c) as usize;
        assert_eq!(&bytes[pe..pe + 4], b"PE\0\0");
        let sections = u16_at(&bytes, pe + 6) as usize;
        let optional_size = u16_at(&bytes, pe + 20) as usize;
        let optional = pe + 24;
        let pe32_plus = u16_at(&bytes, optional) == 0x20b;
        let directories = optional + if pe32_plus { 112 } else { 96 };
        let table = optional + optional_size;
        let sections = (0..sections)
            .map(|i| {
                let s = table + 40 * i;
                let virtual_size = u32_at(&bytes, s + 8);
                let raw_size = u32_at(&bytes, s + 16);
                (
                    u32_at(&bytes, s + 12),
                    virtual_size.max(raw_size),
                    u32_at(&bytes, s + 20),
                )
            })
            .collect();
        Self {
            bytes,
            sections,
            directories,
            pe32_plus,
        }
    }

    fn directory(&self, index: usize) -> (u32, u32) {
        let entry = self.directories + 8 * index;
        (u32_at(&self.bytes, entry), u32_at(&self.bytes, entry + 4))
    }

    fn offset(&self, rva: u32) -> usize {
        let (va, _, raw) = self
            .sections
            .iter()
            .find(|(va, size, _)| (*va..va + size).contains(&rva))
            .unwrap_or_else(|| panic!("RVA {rva:#x} is outside every section"));
        (rva - va + raw) as usize
    }

    fn imports(&self) -> Vec<String> {
        let (rva, _) = self.directory(1);
        let mut descriptor = self.offset(rva);
        let mut names = Vec::new();
        loop {
            let name = u32_at(&self.bytes, descriptor + 12);
            if name == 0 {
                break;
            }
            let start = self.offset(name);
            let end = start + self.bytes[start..].iter().position(|&b| b == 0).unwrap();
            names.push(String::from_utf8_lossy(&self.bytes[start..end]).to_ascii_lowercase());
            descriptor += 20;
        }
        names
    }

    fn dependent_load_flags(&self) -> u16 {
        assert!(self.pe32_plus, "only x64 builds are shipped");
        let (rva, _) = self.directory(10);
        assert_ne!(rva, 0, "the executable has no load configuration directory");
        let config = self.offset(rva);
        // IMAGE_LOAD_CONFIG_DIRECTORY64.DependentLoadFlags is the WORD at 0x4E,
        // after ProcessHeapFlags (0x48) and CSDVersion (0x4C).
        assert!(u32_at(&self.bytes, config) as usize >= 0x50);
        u16_at(&self.bytes, config + 0x4e)
    }
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap())
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn executables() -> [&'static str; 3] {
    [
        env!("CARGO_BIN_EXE_SightOCR"),
        env!("CARGO_BIN_EXE_sightocr-cli"),
        env!("CARGO_BIN_EXE_SightOCR-Setup"),
    ]
}

#[test]
#[cfg_attr(
    not(all(windows, target_env = "msvc")),
    ignore = "DependentLoadFlags needs the MSVC linker; build releases with MSVC"
)]
fn executables_load_static_imports_from_system32_only() {
    for exe in executables() {
        let flags = Pe::open(Path::new(exe)).dependent_load_flags();
        assert_eq!(
            flags, LOAD_LIBRARY_SEARCH_SYSTEM32,
            "{exe}: DependentLoadFlags is {flags:#x}"
        );
    }
}

#[test]
#[cfg_attr(
    not(all(windows, target_env = "msvc")),
    ignore = "checks the MSVC static C runtime"
)]
fn executables_do_not_import_the_redistributable_c_runtime() {
    for exe in executables() {
        let imports = Pe::open(Path::new(exe)).imports();
        let runtime: Vec<_> = imports
            .iter()
            .filter(|name| {
                name.starts_with("vcruntime")
                    || name.starts_with("msvcp")
                    || name.starts_with("ucrtbase")
                    || name.starts_with("api-ms-win-crt-")
            })
            .collect();
        assert!(runtime.is_empty(), "{exe} imports {runtime:?}");
    }
}
