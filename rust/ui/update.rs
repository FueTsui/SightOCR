//! In-app updates: a quiet daily check, an explicit choice to download, a
//! cancellable verified download, and installing now or when SightOCR exits.

use super::{wake_ui, App, Event};
use anyhow::{Context, Result};
use eframe::egui::{self, ViewportCommand};
use sightocr::updater::{self, PreparedUpdate, UpdateInfo, UpdateProgress};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
        Arc,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Background checks run at most this often (also across restarts).
pub(super) const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// Let startup, hotkeys and the first capture settle before the first check.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(20);

#[derive(Default)]
pub(super) struct State {
    pub busy: bool,
    pub message: String,
    pub fraction: Option<f32>,
    pub failed: bool,
    /// A newer release found by a check, not yet downloaded.
    pub available: Option<UpdateInfo>,
    /// A downloaded, verified installer waiting for the user's choice.
    pub ready: Option<PreparedUpdate>,
    /// Install the ready update when SightOCR exits from the tray.
    pub install_on_exit: bool,
    /// Whether the running check was started by the user.
    manual: bool,
    cancel: Option<Arc<AtomicBool>>,
}

impl State {
    pub fn progress(&mut self, progress: UpdateProgress) {
        if !self.busy {
            return;
        }
        self.failed = false;
        self.fraction = None;
        self.message = match progress {
            UpdateProgress::Checking => "正在检查更新…".into(),
            UpdateProgress::Downloading {
                version,
                downloaded,
                total,
            } => {
                if total > 0 {
                    self.fraction = Some((downloaded as f64 / total as f64).clamp(0.0, 1.0) as f32);
                }
                format!(
                    "正在下载 v{version}：{:.1} / {:.1} MB",
                    downloaded as f64 / 1_048_576.0,
                    total as f64 / 1_048_576.0
                )
            }
            UpdateProgress::Verifying { version } => format!("正在校验 v{version} 安装包…"),
        };
    }

    fn fail(&mut self, error: String) {
        self.busy = false;
        self.failed = true;
        self.fraction = None;
        self.cancel = None;
        self.message = format!("更新失败：{error}。可稍后重试。");
    }

    /// Something worth a badge outside the About page.
    pub fn has_news(&self) -> bool {
        self.available.is_some() || self.ready.is_some()
    }

    pub fn downloading(&self) -> bool {
        self.busy && (self.cancel.is_some() || self.fraction.is_some())
    }

    /// The version a badge or the About page should name.
    pub fn new_version(&self) -> Option<&str> {
        self.ready
            .as_ref()
            .map(PreparedUpdate::version)
            .or_else(|| self.available.as_ref().map(UpdateInfo::version))
    }
}

/// Wake the UI when a background check is due, even while it is hidden in the
/// tray. The UI decides whether to check (setting, busy state, smoke tests).
pub(super) fn start_scheduler(
    sender: Sender<Event>,
    ctx: egui::Context,
    hwnd: usize,
    last_check: u64,
) {
    let elapsed = now_seconds().saturating_sub(last_check);
    let first = CHECK_INTERVAL
        .saturating_sub(Duration::from_secs(elapsed))
        .max(FIRST_CHECK_DELAY);
    let _ = thread::Builder::new()
        .name("sightocr-update-schedule".into())
        .spawn(move || {
            let mut wait = first;
            loop {
                thread::sleep(wait);
                if sender.send(Event::UpdateDue).is_err() {
                    break;
                }
                wake_ui(&ctx, hwnd);
                wait = CHECK_INTERVAL;
            }
        });
}

pub(super) fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

impl App {
    /// A scheduled background check. Quiet: only a newer, unskipped release is shown.
    pub(super) fn background_update_check(&mut self, ctx: &egui::Context) {
        #[cfg(debug_assertions)]
        if self.smoke_copies.is_some() {
            return;
        }
        if !self.config.check_updates || self.update.busy || self.update.has_news() {
            return;
        }
        self.config.last_update_check = now_seconds();
        if let Err(error) = self.config.save(&self.path) {
            self.warning = format!("无法保存更新检查时间：{error:#}");
        }
        self.check_for_updates(ctx, false);
    }

    pub(super) fn check_for_updates(&mut self, ctx: &egui::Context, manual: bool) {
        if self.update.busy || self.exiting {
            return;
        }
        self.update.busy = true;
        self.update.manual = manual;
        self.update.failed = false;
        self.update.fraction = None;
        if manual {
            self.update.message = "正在检查更新…".into();
        }
        let proxy = self.config.proxy.clone();
        let sender = self.sender.clone();
        let context = ctx.clone();
        let hwnd = self.hwnd;
        if let Err(error) = thread::Builder::new()
            .name("sightocr-update-check".into())
            .spawn(move || {
                let result = updater::check(&proxy)
                    .map(|found| found.map(Box::new))
                    .map_err(|error| format!("{error:#}"));
                let _ = sender.send(Event::UpdateChecked(result));
                wake_ui(&context, hwnd);
            })
        {
            self.update.fail(format!("无法启动更新检查：{error}"));
        }
    }

    pub(super) fn finish_update_check(&mut self, result: Result<Option<Box<UpdateInfo>>, String>) {
        if !self.update.busy || self.update.cancel.is_some() {
            return;
        }
        self.update.busy = false;
        let manual = std::mem::take(&mut self.update.manual);
        match result {
            Ok(Some(info)) => {
                if !manual && info.version() == self.config.skipped_update_version {
                    return;
                }
                self.update.message = format!(
                    "发现新版本 v{}（约 {:.0} MB）",
                    info.version(),
                    info.size() as f64 / 1_048_576.0
                );
                self.update.available = Some(*info);
            }
            Ok(None) => {
                self.update.available = None;
                if manual {
                    self.update.message =
                        format!("当前已是最新版本（v{}）", env!("CARGO_PKG_VERSION"));
                }
            }
            // A failed background check is not worth interrupting anyone; the
            // next scheduled or manual check will report its own result.
            Err(error) if manual => self.update.fail(error),
            Err(_) => {}
        }
    }

    pub(super) fn download_update(&mut self, ctx: &egui::Context) {
        let Some(info) = self.update.available.clone() else {
            return;
        };
        if self.update.busy || self.exiting {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.update.busy = true;
        self.update.failed = false;
        self.update.cancel = Some(cancel.clone());
        self.update.message = format!("正在准备下载 v{}…", info.version());
        let proxy = self.config.proxy.clone();
        let sender = self.sender.clone();
        let context = ctx.clone();
        let hwnd = self.hwnd;
        if let Err(error) = thread::Builder::new()
            .name("sightocr-update-download".into())
            .spawn(move || {
                let result = updater::download(&proxy, &info, &cancel, |progress| {
                    let _ = sender.send(Event::UpdateProgress(progress));
                    wake_ui(&context, hwnd);
                })
                .map(Some)
                .map_err(|error| format!("{error:#}"));
                let _ = sender.send(Event::UpdateCompleted(result));
                wake_ui(&context, hwnd);
            })
        {
            self.update.fail(format!("无法启动更新下载：{error}"));
        }
    }

    pub(super) fn cancel_update_download(&mut self) {
        if let Some(cancel) = &self.update.cancel {
            cancel.store(true, Ordering::Release);
            self.update.message = "正在取消下载…".into();
        }
    }

    pub(super) fn skip_update(&mut self) {
        let Some(info) = self.update.available.take() else {
            return;
        };
        self.config.skipped_update_version = info.version().to_owned();
        self.update.message = format!("已跳过 v{}，有更新的版本时会再提醒", info.version());
        if let Err(error) = self.config.save(&self.path) {
            self.update.fail(format!("无法保存跳过的版本：{error:#}"));
        }
    }

    /// The end of a download (or, for older callers, of a combined check).
    pub(super) fn finish_update(
        &mut self,
        _ctx: &egui::Context,
        result: Result<Option<PreparedUpdate>, String>,
    ) {
        if !self.update.busy {
            return;
        }
        let cancelled = self
            .update
            .cancel
            .take()
            .is_some_and(|cancel| cancel.load(Ordering::Acquire));
        self.update.busy = false;
        self.update.fraction = None;
        match result {
            Ok(None) => {
                self.update.available = None;
                self.update.message = format!("当前已是最新版本（v{}）", env!("CARGO_PKG_VERSION"));
            }
            Err(_) if cancelled => {
                self.update.failed = false;
                self.update.message = "已取消下载，可随时重新下载".into();
            }
            Err(error) => self.update.fail(error),
            Ok(Some(update)) => {
                self.update.message = format!(
                    "v{} 已下载并通过校验，可以立即安装，或在退出 SightOCR 时自动安装",
                    update.version()
                );
                self.update.available = None;
                self.update.install_on_exit = true;
                self.update.ready = Some(update);
            }
        }
    }

    /// Close SightOCR, install the verified update, and start it again.
    pub(super) fn install_update_now(&mut self, ctx: &egui::Context) {
        if self.busy {
            return;
        }
        let Some(update) = self.update.ready.take() else {
            return;
        };
        let version = update.version().to_owned();
        match self.launch_update(update, true) {
            Ok(()) => {
                self.update.message = format!("正在安装 v{version}，完成后将自动启动…");
                self.request_exit(ctx, false);
            }
            Err(error) => self.update.fail(format!("{error:#}")),
        }
    }

    fn launch_update(&mut self, update: PreparedUpdate, relaunch: bool) -> Result<()> {
        self.config.save(&self.path).context("无法保存当前配置")?;
        let executable = std::env::current_exe().context("无法确定安装位置")?;
        let directory = executable.parent().context("无法确定安装目录")?;
        update.launch(directory, relaunch)
    }

    pub(super) fn request_exit(&mut self, ctx: &egui::Context, restart: bool) {
        // An exit (not a restart) installs a downloaded update the user deferred.
        if !restart && self.update.install_on_exit {
            if let Some(update) = self.update.ready.take() {
                if let Err(error) = self.launch_update(update, false) {
                    // The app is closing; there is nowhere left to show this.
                    eprintln!("SightOCR update was not started: {error:#}");
                }
            }
        }
        self.restart.store(restart, Ordering::Release);
        self.cancelled = true;
        self.worker.cancel();
        self.exiting = true;
        self.startup_hide_after_frames = 0;
        self.raise_after_frame = None;
        self.pending_capture = None;
        if let Some(cancel) = &self.update.cancel {
            cancel.store(true, Ordering::Release);
        }
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }

    /// After an update, say so once; record the running version either way.
    pub(super) fn note_version_change(&mut self) {
        let current = env!("CARGO_PKG_VERSION");
        if self.config.last_run_version == current {
            return;
        }
        if !self.config.last_run_version.is_empty() {
            self.status = format!("已更新到 v{current}");
        }
        self.config.last_run_version = current.into();
        if self.config.skipped_update_version == current {
            self.config.skipped_update_version.clear();
        }
        let _ = self.config.save(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_progress_is_bounded_and_failures_allow_retry() {
        let mut state = State {
            busy: true,
            ..State::default()
        };
        state.progress(UpdateProgress::Downloading {
            version: "2.1.0".into(),
            downloaded: 12,
            total: 10,
        });
        assert_eq!(state.fraction, Some(1.0));
        state.progress(UpdateProgress::Verifying {
            version: "2.1.0".into(),
        });
        assert_eq!(state.fraction, None);
        state.fail("下载中断".into());
        assert!(!state.busy && state.failed);
        let error = state.message.clone();
        state.progress(UpdateProgress::Checking);
        assert_eq!(state.message, error, "late progress must not hide failure");
    }

    #[test]
    fn idle_state_has_no_news_and_is_not_downloading() {
        let state = State::default();
        assert!(!state.has_news() && !state.downloading());
        assert!(state.new_version().is_none());
    }
}
