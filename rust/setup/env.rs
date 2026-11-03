//! Where setup reads and writes. Debug builds can redirect every location
//! (registry, shortcuts, folders) below `SIGHTOCR_SETUP_TEST_ROOT`, so install,
//! upgrade and uninstall can be exercised without administrator rights and
//! without touching a real SightOCR installation. Release builds ignore it.

use crate::win::{known_folder, Hive};
use std::path::PathBuf;
use windows_sys::Win32::System::Registry::{KEY_WOW64_32KEY, KEY_WOW64_64KEY};
use windows_sys::Win32::UI::Shell::{
    FOLDERID_CommonPrograms, FOLDERID_Desktop, FOLDERID_LocalAppData, FOLDERID_ProgramFiles,
    FOLDERID_ProgramFilesX86, FOLDERID_Programs, FOLDERID_PublicDesktop, FOLDERID_RoamingAppData,
    FOLDERID_UserProgramFiles,
};

/// An `...\Uninstall` registry key in a specific hive and registry view.
#[derive(Clone, Debug)]
pub struct UninstallKey {
    pub hive: Hive,
    pub view: u32,
    pub base: String,
}

pub struct Env {
    test_root: Option<PathBuf>,
}

const UNINSTALL: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";

impl Env {
    pub fn detect() -> Self {
        #[cfg(debug_assertions)]
        if let Some(root) =
            std::env::var_os("SIGHTOCR_SETUP_TEST_ROOT").filter(|root| !root.is_empty())
        {
            return Self {
                test_root: Some(PathBuf::from(root)),
            };
        }
        Self { test_root: None }
    }

    pub fn is_test(&self) -> bool {
        self.test_root.is_some()
    }

    pub fn needs_elevation(&self) -> bool {
        !self.is_test() && !crate::win::is_elevated()
    }

    fn test(&self, relative: &str) -> Option<PathBuf> {
        self.test_root.as_ref().map(|root| root.join(relative))
    }

    /// Uninstall keys searched for installations; the first is where setup registers.
    pub fn uninstall_keys(&self) -> Vec<UninstallKey> {
        if self.is_test() {
            return [
                r"Software\SightOCR.SetupTest\MachineUninstall",
                r"Software\SightOCR.SetupTest\Uninstall",
            ]
            .into_iter()
            .map(|base| UninstallKey {
                hive: Hive::CurrentUser,
                view: 0,
                base: base.into(),
            })
            .collect();
        }
        [
            (Hive::LocalMachine, KEY_WOW64_64KEY),
            (Hive::LocalMachine, KEY_WOW64_32KEY),
            (Hive::CurrentUser, KEY_WOW64_64KEY),
            (Hive::CurrentUser, KEY_WOW64_32KEY),
        ]
        .into_iter()
        .map(|(hive, view)| UninstallKey {
            hive,
            view,
            base: UNINSTALL.into(),
        })
        .collect()
    }

    pub fn run_key(&self) -> &'static str {
        if self.is_test() {
            r"Software\SightOCR.SetupTest\Run"
        } else {
            r"Software\Microsoft\Windows\CurrentVersion\Run"
        }
    }

    fn folder(&self, test: &str, id: &windows_sys::core::GUID) -> Option<PathBuf> {
        self.test(test).or_else(|| known_folder(id))
    }

    /// Start menu folders: all users (where setup creates the shortcut), then this user.
    pub fn start_menu_dirs(&self) -> Vec<PathBuf> {
        [
            self.folder("StartMenu", &FOLDERID_CommonPrograms),
            self.folder("UserStartMenu", &FOLDERID_Programs),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// Desktop folders: public (where setup creates the shortcut), then this user.
    pub fn desktop_dirs(&self) -> Vec<PathBuf> {
        [
            self.folder("Desktop", &FOLDERID_PublicDesktop),
            self.folder("UserDesktop", &FOLDERID_Desktop),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    pub fn default_dir(&self) -> PathBuf {
        self.folder(r"program-files", &FOLDERID_ProgramFiles)
            .or_else(|| std::env::var_os("ProgramFiles").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"))
            .join("SightOCR")
    }

    /// Default folders of early releases, moved to `default_dir` on update.
    pub fn legacy_dirs(&self) -> Vec<PathBuf> {
        [
            self.folder("legacy-user", &FOLDERID_UserProgramFiles),
            self.folder("legacy-x86", &FOLDERID_ProgramFilesX86),
        ]
        .into_iter()
        .flatten()
        .map(|folder| folder.join("SightOCR"))
        .collect()
    }

    /// SightOCR's settings folders (removed only when the user asks).
    pub fn user_data_dirs(&self) -> Vec<PathBuf> {
        [
            self.folder(r"AppData\Roaming", &FOLDERID_RoamingAppData),
            self.folder(r"AppData\Local", &FOLDERID_LocalAppData),
        ]
        .into_iter()
        .flatten()
        .map(|folder| folder.join("SightOCR"))
        .collect()
    }

    /// Where configuration found in old installation folders is copied.
    pub fn backup_root(&self) -> PathBuf {
        self.folder(r"AppData\Local", &FOLDERID_LocalAppData)
            .unwrap_or_else(std::env::temp_dir)
            .join(r"SightOCR\UpgradeBackup")
    }
}
