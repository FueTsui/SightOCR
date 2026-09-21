//! Only obsolete screenshot files from the former desktop implementation.
//! Current capture and OCR use memory and anonymous pipes, never image files.
use std::os::windows::fs::MetadataExt;
use std::{
    fs,
    path::Path,
    sync::mpsc,
    thread,
    time::{Duration, SystemTime},
};

const RETENTION: Duration = Duration::from_secs(72 * 3600);
const INTERVAL: Duration = Duration::from_secs(24 * 3600);

pub struct Cleanup {
    stop: mpsc::Sender<()>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Cleanup {
    pub fn start() -> std::io::Result<Self> {
        let (stop, receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("legacy-cache-cleanup".into())
            .spawn(move || {
                let root = std::env::temp_dir();
                loop {
                    sweep(&root, SystemTime::now());
                    if receiver.recv_timeout(INTERVAL) != Err(mpsc::RecvTimeoutError::Timeout) {
                        break;
                    }
                }
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn plain_directory(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() && m.file_attributes() & 0x400 == 0)
}

fn screenshot_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() == 25
        && bytes.starts_with(b"sight_")
        && bytes.ends_with(b".png")
        && bytes[14] == b'_'
        && bytes[6..14].iter().all(u8::is_ascii_digit)
        && bytes[15..21].iter().all(u8::is_ascii_digit)
}

fn clean_directory(path: &Path, now: SystemTime) -> usize {
    if !plain_directory(path) {
        return 0;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        if !screenshot_name(&entry.file_name().to_string_lossy()) {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if !metadata.is_file() || metadata.file_attributes() & 0x400 != 0 {
            continue;
        }
        let expired = metadata
            .modified()
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > RETENTION);
        if expired && fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

fn sweep(root: &Path, now: SystemTime) -> usize {
    if !plain_directory(root) {
        return 0;
    }
    let mut removed = clean_directory(root, now);
    let owned = root.join("SightOCR");
    if plain_directory(&owned) {
        removed += clean_directory(&owned.join("screenshots"), now);
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn removes_only_expired_legacy_screenshots_in_both_locations() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("SightOCR/screenshots");
        fs::create_dir_all(&nested).unwrap();
        for dir in [root.path(), nested.as_path()] {
            fs::write(dir.join("sight_20260901_120000.png"), b"old").unwrap();
            fs::write(dir.join("my-screenshot.png"), b"keep").unwrap();
        }
        assert_eq!(sweep(root.path(), SystemTime::now()), 0);
        assert_eq!(
            sweep(
                root.path(),
                SystemTime::now() + RETENTION + Duration::from_secs(60)
            ),
            2
        );
        assert!(nested.join("my-screenshot.png").exists());
        assert!(root.path().join("my-screenshot.png").exists());
    }
    #[test]
    fn names_are_strict_and_shutdown_does_not_wait_a_day() {
        assert!(!screenshot_name("sight_user.png"));
        assert!(!screenshot_name("sight_abcdefgh_abcdef.png"));
        assert!(screenshot_name("sight_20260901_120000.png"));
        // Test the stop protocol without sweeping the real user's temp directory.
        let (sender, receiver) = mpsc::channel();
        sender.send(()).unwrap();
        assert_eq!(receiver.recv_timeout(INTERVAL), Ok(()));
    }
}
