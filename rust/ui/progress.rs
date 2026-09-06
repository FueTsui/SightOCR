use super::{
    theme::{self, ButtonKind, Icon, Palette},
    App, Config, LANGUAGES, TRANSLATORS,
};
use eframe::egui::{self, Align, Layout, RichText, Sense};
use sightocr::worker::{ProgressStage, Task};

/// A request snapshot keeps the waiting view truthful if settings change mid-request.
pub(super) struct JobProgress {
    pub stage: ProgressStage,
    from_capture: bool,
    pub(super) translate_after_ocr: bool,
    ocr_provider: String,
    translator: String,
    target: String,
}

impl JobProgress {
    pub fn new(config: &Config, task: &Task) -> Self {
        let (from_capture, translate_after_ocr, stage) = match task {
            Task::Recognize { translate, .. } => (true, *translate, ProgressStage::Recognizing),
            Task::Translate(_) => (false, false, ProgressStage::Translating),
        };
        Self {
            stage,
            from_capture,
            translate_after_ocr,
            ocr_provider: super::workspace::ocr_provider_label(&config.last_ocr_selection).into(),
            translator: label(&config.last_translate_selection, TRANSLATORS),
            target: label(&config.target_lang, LANGUAGES),
        }
    }
}

fn label(value: &str, choices: &[(&str, &str)]) -> String {
    choices
        .iter()
        .find(|(id, _)| *id == value)
        .map_or(value, |(_, title)| *title)
        .to_owned()
}

impl App {
    pub(super) fn translating(&self) -> bool {
        self.busy
            && self
                .job
                .as_ref()
                .is_some_and(|job| job.stage == ProgressStage::Translating)
    }

    pub(super) fn recognizing(&self) -> bool {
        self.busy && !self.capturing && !self.translating()
    }

    pub(super) fn cancel_job(&mut self) {
        if self.busy && !self.capturing && !self.cancelled {
            self.cancelled = true;
            self.worker.cancel();
            self.busy = false;
            self.job = None;
            self.pending_image_size = None;
            self.status = "已取消 · 可以开始新任务".into();
        }
    }

    pub(super) fn waiting_content(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let p = Palette::get(ui.ctx());
        let translating = self.translating();
        let title = if translating {
            "正在翻译文字"
        } else {
            "正在识别截图"
        };
        let provider = self
            .job
            .as_ref()
            .map(|job| {
                if translating {
                    format!("{} · 翻译为{}", job.translator, job.target)
                } else if job.stage == ProgressStage::LocalFallback {
                    "本地 OneOCR · 继续识别".into()
                } else {
                    job.ocr_provider.clone()
                }
            })
            .unwrap_or_else(|| "正在处理当前任务".into());
        let steps = self
            .job
            .as_ref()
            .map(|job| {
                if !job.from_capture {
                    "原文已就绪"
                } else if translating {
                    "截图完成   ·   识别完成   ·   翻译中"
                } else if job.translate_after_ocr {
                    "截图完成   ·   识别中   ·   等待翻译"
                } else {
                    "截图完成   ·   识别中"
                }
            })
            .unwrap_or("截图完成   ·   识别中");
        let elapsed = self.started.elapsed().as_secs();
        let hint = if elapsed >= 15 {
            "服务响应较慢，可继续等待或取消任务"
        } else if translating {
            "原文已保留，译文完成后将自动显示"
        } else {
            "正在提取选中区域的内容"
        };
        let tall = rect.height() >= 280.0;
        let content_height = if tall { 268.0_f32 } else { 192.0_f32 }.min(rect.height());
        let content = egui::Rect::from_center_size(
            rect.center(),
            egui::vec2(rect.width().min(480.0), content_height),
        );
        let mut content_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(content)
                .layout(Layout::top_down(Align::Center)),
        );
        {
            let ui = &mut content_ui;
            ui.set_clip_rect(ui.clip_rect().intersect(rect));
            ui.spacing_mut().item_spacing.y = 8.0;
            if tall {
                let (icon_rect, _) = ui.allocate_exact_size(egui::vec2(56.0, 56.0), Sense::hover());
                ui.painter().rect_filled(icon_rect, 12, p.accent_soft);
                let mut spinner_ui =
                    ui.new_child(egui::UiBuilder::new().max_rect(icon_rect.shrink(14.0)));
                spinner_ui.add(egui::Spinner::new().size(28.0).color(p.accent));
                ui.add_space(4.0);
            } else {
                ui.add(egui::Spinner::new().size(22.0).color(p.accent));
            }
            ui.label(
                RichText::new(title)
                    .size(if tall { 20.0 } else { 17.0 })
                    .strong()
                    .color(p.text),
            );
            ui.label(RichText::new(provider).size(13.0).color(p.muted));
            if tall {
                ui.label(RichText::new(steps).size(12.0).color(p.accent));
            }
            ui.label(
                RichText::new(format!("已用 {:02}:{:02}", elapsed / 60, elapsed % 60))
                    .size(12.0)
                    .color(p.muted),
            );
            ui.label(RichText::new(hint).size(12.0).color(p.subtle));
            ui.add_space(4.0);
            if theme::button(ui, "取消任务", Icon::Close, ButtonKind::Secondary).clicked() {
                self.cancel_job();
            }
        }
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(80));
    }

    pub(super) fn waiting_page(&mut self, ui: &mut egui::Ui, height: f32) {
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
        self.waiting_content(ui, rect);
    }
}
