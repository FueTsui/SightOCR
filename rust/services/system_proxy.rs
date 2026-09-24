//! Detect changes to the inputs reqwest uses for automatic proxy selection.
//! No Debug/logging: environment variables may contain proxy credentials.

use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::ptr::{null, null_mut};
use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS},
    System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE,
    },
};

pub(super) const SETTINGS_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";

const ENVIRONMENT_KEYS: [&str; 9] = [
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
    "NO_PROXY",
    "no_proxy",
    "REQUEST_METHOD",
];

#[derive(PartialEq, Eq)]
pub(super) struct Snapshot {
    environment: [Option<OsString>; 9],
    // Compare the actual inputs, not registry timestamps (rapid toggles must
    // be detected too). Retain only their digest, not any proxy credentials.
    registry_revision: Option<[u8; 32]>,
}

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: The wrapper owns this successfully opened key.
        unsafe { RegCloseKey(self.0) };
    }
}

impl Snapshot {
    // On a read failure, do not treat stale settings as unchanged: the caller
    // rebuilds the client and lets reqwest resolve its current system inputs.
    pub(super) fn capture() -> Option<Self> {
        let environment = ENVIRONMENT_KEYS.map(std::env::var_os);
        let path: Vec<u16> = SETTINGS_KEY.encode_utf16().chain(Some(0)).collect();
        let mut raw = null_mut();
        // SAFETY: path is terminated and raw is a valid writable out-parameter.
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                0,
                KEY_QUERY_VALUE,
                &mut raw,
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Some(Self {
                environment,
                registry_revision: None,
            });
        }
        if status != ERROR_SUCCESS {
            return None;
        }
        let key = Key(raw);
        let mut hash = Sha256::new();
        let mut bytes = vec![0u8; 64 * 1024];
        for name in ["ProxyEnable", "ProxyServer", "ProxyOverride"] {
            let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
            let mut length = bytes.len() as u32;
            let mut kind = 0;
            // SAFETY: key is live; name is terminated; the byte buffer is
            // writable for length bytes and all output parameters are valid.
            let status = unsafe {
                RegQueryValueExW(
                    key.0,
                    name.as_ptr(),
                    null(),
                    &mut kind,
                    bytes.as_mut_ptr(),
                    &mut length,
                )
            };
            hash.update(status.to_le_bytes());
            if status == ERROR_FILE_NOT_FOUND {
                continue;
            }
            if status != ERROR_SUCCESS {
                return None;
            }
            hash.update(kind.to_le_bytes());
            hash.update(length.to_le_bytes());
            hash.update(&bytes[..length as usize]);
        }
        Some(Self {
            environment,
            registry_revision: Some(hash.finalize().into()),
        })
    }
}
