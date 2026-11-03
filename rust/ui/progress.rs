use super::{
    motion,
    theme::{self, ButtonKind, Icon, Palette},
    App, Config, LANGUAGES,
};
use eframe::egui::{self, Align, Layout, RichText, Sense, Stroke};
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
            ocr_provider: super::workspace::ocr_provider_label(config, &config.last_ocr_selection),
            translator: super::workspace::translator_label(
                config,
                &config.last_translate_selection,
            ),
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
                    "本地 OCR · 继续识别".into()
                } else {
                    job.ocr_provider.clone()
                }
            })
            .unwrap_or_else(|| "正在处理当前任务".into());
        // (label, done, active) for each visible step.
        let steps: Vec<(&str, bool, bool)> = match self.job.as_ref() {
            Some(job) if !job.from_capture => vec![("原文", true, false), ("翻译", false, true)],
            Some(_) if translating => vec![
                ("截图", true, false),
                ("识别", true, false),
                ("翻译", false, true),
            ],
            Some(job) if job.translate_after_ocr => vec![
                ("截图", true, false),
                ("识别", false, true),
                ("翻译", false, false),
            ],
            _ => vec![("截图", true, false), ("识别", false, true)],
        };
        let elapsed = self.started.elapsed().as_secs();
        let hint = if elapsed >= 15 {
            "服务响应较慢，可继续等待或取消任务"
        } else if translating {
            "原文已保留，译文完成后将自动显示"
        } else {
            "正在提取选中区域的内容"
        };
        let tall = rect.height() >= 280.0;
        let content_height = if tall { 284.0_f32 } else { 192.0_f32 }.min(rect.height());
        let content = egui::Rect::from_center_size(
            rect.center(),
            egui::vec2(rect.width().min(480.0), content_height),
        );
        let time = ui.input(|input| input.time) as f32;
        let still = motion::reduced_motion();
        // Fade the whole page in when the job starts or changes stage.
        let shown = motion::appear(
            ui.ctx(),
            egui::Id::new("waiting_page").with(tall),
            motion::key((self.request_id, translating)),
        );
        let mut content_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(content.translate(egui::vec2(0.0, (1.0 - shown) * 8.0)))
                .layout(Layout::top_down(Align::Center)),
        );
        {
            let ui = &mut content_ui;
            ui.set_clip_rect(ui.clip_rect().intersect(rect));
            ui.multiply_opacity(shown);
            ui.spacing_mut().item_spacing.y = 8.0;
            if tall {
                let (tile, _) = ui.allocate_exact_size(egui::vec2(56.0, 56.0), Sense::hover());
                scan_tile(ui, &p, tile, translating, time, still);
                ui.add_space(4.0);
            } else {
                let (tile, _) = ui.allocate_exact_size(egui::vec2(36.0, 36.0), Sense::hover());
                scan_tile(ui, &p, tile, translating, time, still);
            }
            ui.label(
                RichText::new(title)
                    .size(if tall { 20.0 } else { 17.0 })
                    .strong()
                    .color(p.text),
            );
            ui.label(RichText::new(provider).size(13.0).color(p.muted));
            if tall {
                ui.add_space(2.0);
                let width = (ui.available_width() - 32.0).clamp(160.0, 300.0);
                let (track, _) = ui.allocate_exact_size(egui::vec2(width, 40.0), Sense::hover());
                stepper(ui, &p, track, &steps, time, still);
            }
            let (bar, _) = ui.allocate_exact_size(egui::vec2(180.0, 3.0), Sense::hover());
            indeterminate_bar(ui, &p, bar, time, still);
            ui.label(
                RichText::new(format!("已用 {:02}:{:02}", elapsed / 60, elapsed % 60))
                    .size(12.0)
                    .color(p.muted),
            );
            ui.label(RichText::new(hint).size(12.0).color(p.subtle));
            ui.add_space(4.0);
            if theme::button(ui, "取消任务", Icon::Close, ButtonKind::Secondary)
                .on_hover_text("丢弃本次结果，保留已显示的原文")
                .clicked()
            {
                self.cancel_job();
            }
        }
        // Smooth motion while visible; a slow tick keeps the timer fresh otherwise.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(if still {
                500
            } else {
                16
            }));
    }

    pub(super) fn waiting_page(&mut self, ui: &mut egui::Ui, height: f32) {
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
        self.waiting_content(ui, rect);
    }
}

/// Icon tile with a scan line sweeping over it (OCR) or a soft pulse (translation).
fn scan_tile(
    ui: &egui::Ui,
    p: &Palette,
    tile: egui::Rect,
    translating: bool,
    time: f32,
    still: bool,
) {
    let painter = ui.painter().with_clip_rect(tile);
    let radius = (tile.width() * 0.22) as u8;
    painter.rect_filled(tile, radius, p.accent_soft);
    let icon = tile.shrink(tile.width() * 0.26);
    if translating {
        if !still {
            // Expanding, fading ring.
            let phase = (time / 1.4).fract();
            painter.rect_stroke(
                tile.shrink(tile.width() * 0.18 * (1.0 - phase)),
                radius,
                Stroke::new(1.5_f32, p.accent.gamma_multiply(0.5 * (1.0 - phase))),
                egui::StrokeKind::Inside,
            );
        }
        theme::draw_icon(ui, Icon::Translate, icon, p.accent);
        return;
    }
    theme::draw_icon(ui, Icon::Scan, icon, p.accent);
    if still {
        return;
    }
    // Ease the sweep in both directions so it reads as scanning, not as a loading bar.
    let phase = (time / 1.6).fract();
    let sweep = 0.5 - 0.5 * (phase * std::f32::consts::TAU).cos();
    let y = egui::lerp((icon.top() - 2.0)..=(icon.bottom() + 2.0), sweep);
    let band = egui::Rect::from_min_max(
        egui::pos2(tile.left() + 6.0, y - 5.0),
        egui::pos2(tile.right() - 6.0, y + 5.0),
    );
    painter.rect_filled(band, 2, p.accent.gamma_multiply(0.10));
    painter.line_segment(
        [
            egui::pos2(tile.left() + 6.0, y),
            egui::pos2(tile.right() - 6.0, y),
        ],
        Stroke::new(1.5_f32, p.accent.gamma_multiply(0.85)),
    );
}

/// Horizontal step indicator: completed steps get a check, the active one pulses.
fn stepper(
    ui: &egui::Ui,
    p: &Palette,
    track: egui::Rect,
    steps: &[(&str, bool, bool)],
    time: f32,
    still: bool,
) {
    let painter = ui.painter();
    let count = steps.len().max(2);
    let y = track.top() + 10.0;
    let x = |index: usize| {
        egui::lerp(
            (track.left() + 14.0)..=(track.right() - 14.0),
            index as f32 / (count - 1) as f32,
        )
    };
    let pulse = if still {
        1.0
    } else {
        0.5 + 0.5 * (time * 4.0).sin()
    };
    for index in 0..steps.len().saturating_sub(1) {
        let (a, b) = (x(index) + 12.0, x(index + 1) - 12.0);
        let done = steps[index].1;
        painter.line_segment(
            [egui::pos2(a, y), egui::pos2(b, y)],
            Stroke::new(2.0_f32, if done { p.accent } else { p.border }),
        );
        // A short highlight travels along the segment that leads into the active step.
        if done && steps[index + 1].2 && !still {
            let t = (time / 1.2).fract();
            let head = egui::lerp(a..=b, t);
            painter.line_segment(
                [egui::pos2((head - 14.0).max(a), y), egui::pos2(head, y)],
                Stroke::new(2.0_f32, p.panel.gamma_multiply(0.7)),
            );
        }
    }
    for (index, (label, done, active)) in steps.iter().enumerate() {
        let center = egui::pos2(x(index), y);
        if *done {
            painter.circle_filled(center, 9.0, p.accent);
            theme::draw_icon(
                ui,
                Icon::Check,
                egui::Rect::from_center_size(center, egui::vec2(11.0, 11.0)),
                p.panel,
            );
        } else if *active {
            painter.circle_filled(center, 9.0 + 3.0 * pulse, p.accent.gamma_multiply(0.12));
            painter.circle_stroke(center, 8.0, Stroke::new(2.0_f32, p.accent));
            painter.circle_filled(center, 3.0 + pulse, p.accent);
        } else {
            painter.circle_stroke(center, 8.0, Stroke::new(1.5_f32, p.border));
        }
        painter.text(
            center + egui::vec2(0.0, 21.0),
            egui::Align2::CENTER_CENTER,
            *label,
            egui::FontId::proportional(12.0),
            if *active {
                p.accent
            } else if *done {
                p.text
            } else {
                p.subtle
            },
        );
    }
}

/// A thin bar with a highlight that glides across; it never claims a percentage.
fn indeterminate_bar(ui: &egui::Ui, p: &Palette, bar: egui::Rect, time: f32, still: bool) {
    let painter = ui.painter().with_clip_rect(bar.expand(1.0));
    painter.rect_filled(bar, 2, p.border.gamma_multiply(0.7));
    let (start, width) = if still {
        (0.0, bar.width())
    } else {
        let t = motion::ease_out((time / 1.3).fract()) * 1.4 - 0.2;
        (t * bar.width(), bar.width() * 0.32)
    };
    let left = (bar.left() + start - width / 2.0).max(bar.left());
    let right = (bar.left() + start + width / 2.0).min(bar.right());
    if right > left {
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(left, bar.top()), egui::pos2(right, bar.bottom())),
            2,
            p.accent,
        );
    }
}
