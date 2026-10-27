//! Plain-text setup log. Always written (to the user's temp folder unless
//! `/LOG=<file>` names one) so a failed in-app update can point to it.

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

static LOG: Mutex<Option<(File, PathBuf)>> = Mutex::new(None);

pub fn init(target: Option<Option<PathBuf>>, role: &str) {
    let path = match target {
        Some(Some(path)) => path,
        _ => std::env::temp_dir().join(format!(
            "SightOCR-{role}-{}.log",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        )),
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(file) = File::options().create(true).append(true).open(&path) {
        if let Ok(mut log) = LOG.lock() {
            *log = Some((file, path));
        }
    }
}

pub fn path() -> Option<PathBuf> {
    LOG.lock().ok()?.as_ref().map(|(_, path)| path.clone())
}

pub fn write(line: &str) {
    if let Ok(mut log) = LOG.lock() {
        if let Some((file, _)) = log.as_mut() {
            let _ = writeln!(
                file,
                "{} {line}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f")
            );
        }
    }
    #[cfg(debug_assertions)]
    eprintln!("{line}");
}

#[macro_export]
macro_rules! logln {
    ($($argument:tt)*) => {
        $crate::log::write(&format!($($argument)*))
    };
}
