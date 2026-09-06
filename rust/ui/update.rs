use super::{wake_ui, App, Event};
use anyhow::{Context, Result};
use eframe::egui::{self, ViewportCommand};
use sightocr::updater::{self, PreparedUpdate, UpdateProgress};
use std::{sync::atomic::Ordering, thread};

#[derive(Default)]
pub(super) struct State {
    pub busy: bool,
    pub message: String,
    pub fraction: Option<f32>,
    pub failed: bool,
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
                    "正在后台下载 v{version}：{:.1} / {:.1} MB",
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
        self.message = format!("更新失败：{error}。可点击“检查更新”重试。");
    }
}

impl App {
    pub(super) fn check_for_updates(&mut self, ctx: &egui::Context) {
        if self.update.busy || self.exiting {
            return;
        }
        self.update = State {
            busy: true,
            message: "正在检查更新…".into(),
            ..State::default()
        };
        let proxy = self.config.proxy.clone();
        let sender = self.sender.clone();
        let context = ctx.clone();
        let hwnd = self.hwnd;
        if let Err(error) = thread::Builder::new()
            .name("sightocr-update".into())
            .spawn(move || {
                let result = updater::check_and_download(&proxy, |progress| {
                    let _ = sender.send(Event::UpdateProgress(progress));
                    wake_ui(&context, hwnd);
                })
                .map_err(|error| format!("{error:#}"));
                let _ = sender.send(Event::UpdateCompleted(result));
                wake_ui(&context, hwnd);
            })
        {
            self.update.fail(format!("无法启动更新任务：{error}"));
        }
    }

    pub(super) fn finish_update(
        &mut self,
        ctx: &egui::Context,
        result: Result<Option<PreparedUpdate>, String>,
    ) {
        if !self.update.busy {
            return;
        }
        self.update.fraction = None;
        match result {
            Ok(None) => {
                self.update.busy = false;
                self.update.message = format!("当前已是最新版本（v{}）", env!("CARGO_PKG_VERSION"));
            }
            Err(error) => self.update.fail(error),
            Ok(Some(update)) => {
                let version = update.version().to_owned();
                let launch = (|| -> Result<()> {
                    self.config.save(&self.path).context("无法保存当前配置")?;
                    let executable = std::env::current_exe().context("无法确定安装位置")?;
                    let directory = executable.parent().context("无法确定安装目录")?;
                    update.launch(directory)
                })();
                match launch {
                    Ok(()) => {
                        self.update.message = format!("正在安装 v{version}，完成后将自动启动…");
                        self.request_exit(ctx, false);
                    }
                    Err(error) => self.update.fail(format!("{error:#}")),
                }
            }
        }
    }

    pub(super) fn request_exit(&mut self, ctx: &egui::Context, restart: bool) {
        self.restart.store(restart, Ordering::Release);
        self.cancelled = true;
        self.worker.cancel();
        self.exiting = true;
        self.startup_hide_after_frames = 0;
        self.raise_after_frame = None;
        self.pending_capture = None;
        ctx.send_viewport_cmd(ViewportCommand::Close);
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
}
