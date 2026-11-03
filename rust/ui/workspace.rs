use super::{
    motion,
    theme::{self, ButtonKind, Icon, Palette},
    App, LANGUAGES, OCR_MODES,
};
use eframe::egui::{
    self, Align, Color32, FontId, Layout, RichText, Sense, Stroke, ViewportCommand,
};
use sightocr::config::{model_ocr_selection, model_translator, Config, MODEL_SELECTION_PREFIX};
use sightocr::worker::Task;
use std::time::Duration;

/// How long copy buttons show their confirmation.
const COPIED_FOR: Duration = Duration::from_millis(1400);

impl App {
    pub(super) fn toolbar(&mut self, ctx: &egui::Context) {
        let p = Palette::get(ctx);
        let panel = egui::TopBottomPanel::top("command_bar")
            .exact_height(68.0)
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(p.sidebar)
                    .inner_margin(egui::Margin::symmetric(16, 16)),
            )
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(12.0, 0.0);
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(!self.busy, |ui| {
                        if theme::button(ui, "新建", Icon::Plus, ButtonKind::Secondary)
                            .on_hover_text(if self.capture_translate {
                                format!("截图并翻译 · {}", self.config.translate_hotkey)
                            } else {
                                format!("截图识别 · {}", self.config.hotkey)
                            })
                            .clicked()
                        {
                            self.capture(ctx, self.capture_translate);
                        }
                        egui::Frame::new()
                            .fill(if ui.visuals().dark_mode {
                                Color32::from_white_alpha(10)
                            } else {
                                Color32::from_black_alpha(8)
                            })
                            .corner_radius(4.0)
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                ui.horizontal(|ui| {
                                    // Reserve the pill below the icons, then place it once
                                    // both buttons have been laid out.
                                    let pill = ui.painter().add(egui::Shape::Noop);
                                    let scan = mode_button(
                                        ui,
                                        Icon::Scan,
                                        "截图识别",
                                        !self.capture_translate,
                                        &p,
                                    );
                                    let translate = mode_button(
                                        ui,
                                        Icon::Translate,
                                        "截图并翻译",
                                        self.capture_translate,
                                        &p,
                                    );
                                    let target = if self.capture_translate {
                                        translate.rect
                                    } else {
                                        scan.rect
                                    };
                                    let dark = ui.visuals().dark_mode;
                                    let tile = if dark { p.panel_alt } else { p.panel };
                                    sliding_pill(ui, pill, "mode_pill", target, |rect| {
                                        theme::raised_tile(rect, dark, tile, 1.0)
                                    });
                                    theme::sliding_underline(
                                        ui,
                                        "mode_underline",
                                        target.translate(egui::vec2(0.0, 1.0)),
                                        16.0,
                                        p.accent,
                                    );
                                    if scan.clicked() {
                                        self.capture_translate = false;
                                    }
                                    if translate.clicked() {
                                        self.capture_translate = true;
                                    }
                                });
                            });
                        toolbar_separator(ui, p.border);
                        self.recognition_options(ui);
                    });
                    if theme::icon_button(
                        ui,
                        Icon::Pin,
                        if self.always_on_top {
                            "取消窗口置顶"
                        } else {
                            "窗口置顶"
                        },
                        self.always_on_top,
                    )
                    .clicked()
                    {
                        self.always_on_top = !self.always_on_top;
                        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(
                            if self.always_on_top {
                                egui::WindowLevel::AlwaysOnTop
                            } else {
                                egui::WindowLevel::Normal
                            },
                        ));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if theme::icon_button(ui, Icon::Settings, "设置", self.settings_open)
                            .clicked()
                        {
                            self.open_preferences(0);
                        }
                        if let Some(version) = self.update.new_version().map(str::to_owned) {
                            if update_pill(ui, &p, &version, self.update.ready.is_some()).clicked()
                            {
                                self.open_preferences(2);
                            }
                        }
                    });
                });
            });
        ctx.layer_painter(panel.response.layer_id).hline(
            panel.response.rect.x_range(),
            panel.response.rect.bottom() - 0.5,
            Stroke::new(1.0_f32, p.border),
        );
    }

    fn recognition_options(&mut self, ui: &mut egui::Ui) {
        let mut kind = content_kind(&self.config.last_ocr_selection);
        let old_kind = kind;
        let mut engine = Engine::of(&self.config.last_ocr_selection);
        let old_engine = engine.clone();
        let local = engine == Engine::Local;
        let (kind_icon, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), Sense::hover());
        theme::draw_icon(
            ui,
            [Icon::Text, Icon::Table, Icon::Formula][kind],
            kind_icon,
            Palette::get(ui.ctx()).muted,
        );
        let kind_label = ["文字", "表格", "公式"][kind];
        theme::dropdown(
            ui,
            "recognition_kind",
            true,
            |combo| combo.width(0.0).selected_text(kind_label),
            |ui| {
                for (index, label) in ["文字", "表格", "公式"].into_iter().enumerate() {
                    let response = ui
                        .add_enabled_ui(index != 2 || !local, |ui| {
                            theme::menu_option(ui, &mut kind, index, label)
                        })
                        .inner;
                    if index == 2 && local {
                        response.on_hover_text("公式识别需要云端引擎或模型接口");
                    }
                }
            },
        )
        .response
        .on_hover_text("识别内容类型");
        let mut open_services = false;
        let engine_name = engine.label(&self.config).to_string();
        let engine_label = theme::elide(ui, &engine_name, ENGINE_LABEL_WIDTH);
        let engine_elided = engine_label != engine_name;
        theme::dropdown(
            ui,
            "recognition_provider",
            true,
            |combo| combo.width(0.0).selected_text(engine_label),
            |ui| {
                for builtin in [Engine::Local, Engine::Baidu, Engine::Tencent] {
                    let label = builtin.label(&self.config);
                    theme::menu_option(ui, &mut engine, builtin, label);
                }
                let models: Vec<_> = self
                    .config
                    .model_profiles
                    .iter()
                    .filter(|profile| profile.can_recognize())
                    .map(|profile| (profile.id.clone(), profile.name.clone()))
                    .collect();
                ui.separator();
                for (id, name) in models {
                    theme::menu_option(ui, &mut engine, Engine::Model(id), name);
                }
                if theme::menu_item(ui, false, "＋ 接口")
                    .on_hover_text("在「设置 → 接口」中添加模型接口")
                    .changed()
                {
                    open_services = true;
                }
                if matches!(old_engine, Engine::Baidu | Engine::Tencent) && kind == 0 {
                    ui.separator();
                    theme::menu_heading(ui, "识别精度");
                    for (id, label) in OCR_MODES
                        .iter()
                        .filter(|(id, _)| Engine::of(id) == old_engine && content_kind(id) == 0)
                    {
                        if theme::menu_option(
                            ui,
                            &mut self.config.last_ocr_selection,
                            (*id).into(),
                            *label,
                        )
                        .changed()
                        {
                            ui.close_menu();
                        }
                    }
                }
            },
        )
        .response
        .on_hover_text({
            let hint = match &engine {
                Engine::Local => "本地离线识别",
                Engine::Model(_) => "模型接口识别，可在设置 → 接口中修改",
                _ => "云端识别，密钥可在设置 → 接口中配置",
            };
            // A shortened name is shown in full on hover.
            if engine_elided {
                format!("{engine_name}\n{hint}")
            } else {
                hint.to_owned()
            }
        });
        if open_services {
            self.open_preferences(1);
        }
        if engine != old_engine || kind != old_kind {
            if engine == Engine::Local && kind == 2 {
                kind = 0;
                self.status = "本地引擎支持文字与表格，已选择文字识别".into();
            }
            self.config.last_ocr_selection = engine.selection(kind);
        }
    }

    pub(super) fn open_preferences(&mut self, tab: usize) {
        if !self.settings_open {
            self.draft = self.config.clone();
            self.settings_error.clear();
        }
        self.settings_tab = tab;
        self.settings_open = true;
    }

    pub(super) fn workbench(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let p = Palette::get(ctx);
        ui.spacing_mut().item_spacing.y = 0.0;
        let footer_height = 27.0;
        let notice_height = if !self.error.is_empty() || !self.warning.is_empty() {
            63.0
        } else {
            0.0
        };
        let compact = ui.available_width() < 800.0;
        let empty = !self.editing_text
            && self.source.is_empty()
            && self.translation.is_empty()
            && !self.native_editors.is_focused(0)
            && !self.native_editors.is_focused(1);
        let recognizing = self.recognizing();
        if compact && !empty && !recognizing {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let pill = ui.painter().add(egui::Shape::Noop);
                let source = segment(ui, Icon::Text, "原文", self.compact_result_tab == 0);
                let translation =
                    segment(ui, Icon::Translate, "译文", self.compact_result_tab == 1);
                let target = if self.compact_result_tab == 0 {
                    source.rect
                } else {
                    translation.rect
                };
                let dark = ui.visuals().dark_mode;
                let tile = if dark { p.panel_alt } else { p.panel };
                sliding_pill(ui, pill, "compact_result_pill", target, |rect| {
                    theme::raised_tile(rect, dark, tile, 1.0)
                });
                if source.clicked() {
                    self.compact_result_tab = 0;
                }
                if translation.clicked() {
                    self.compact_result_tab = 1;
                }
            });
            ui.add_space(12.0);
        }
        let height = (ui.available_height() - footer_height - notice_height - 10.0).max(140.0);
        if recognizing {
            self.waiting_page(ui, height);
        } else if empty {
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
            let content = egui::Rect::from_center_size(
                rect.center(),
                egui::vec2(
                    rect.width().min(600.0),
                    if self.capture_translate { 168.0 } else { 122.0 },
                ),
            );
            let shown = motion::appear(
                ctx,
                egui::Id::new("empty_workbench"),
                u64::from(self.capture_translate),
            );
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(content.translate(egui::vec2(0.0, (1.0 - shown) * 6.0)))
                    .layout(Layout::top_down(Align::Center)),
                |ui| {
                    ui.multiply_opacity(shown);
                    ui.spacing_mut().item_spacing.y = 12.0;
                    if self.busy {
                        ui.add(egui::Spinner::new().size(24.0).color(p.accent));
                        ui.label(
                            RichText::new("正在提取文字，请稍候…")
                                .size(16.0)
                                .color(p.text),
                        );
                    } else {
                        let hotkey = if self.capture_translate {
                            &self.config.translate_hotkey
                        } else {
                            &self.config.hotkey
                        };
                        ui.label(
                            RichText::new(format!(
                                "按 {hotkey} 开始{}",
                                if self.capture_translate {
                                    "截图并翻译"
                                } else {
                                    "截图识别"
                                }
                            ))
                            .size(18.0)
                            .color(p.text),
                        );
                        if self.capture_translate {
                            ui.allocate_ui_with_layout(
                                egui::vec2(290.0, 32.0),
                                Layout::left_to_right(Align::Center),
                                |ui| {
                                    ui.label(RichText::new("翻译为").size(13.0).color(p.muted));
                                    language_menu(
                                        ui,
                                        "empty_target_language",
                                        &mut self.config.target_lang,
                                        &LANGUAGES[1..],
                                    );
                                    translation_provider_menu(
                                        ui,
                                        "empty_translator",
                                        &mut self.config,
                                    );
                                },
                            );
                        }
                        ui.label(
                            RichText::new("也可以点击「新建」截图，或输入文字进行翻译")
                                .size(13.0)
                                .color(p.muted),
                        );
                        if theme::button(ui, "输入文字", Icon::Text, ButtonKind::Ghost).clicked()
                        {
                            self.editing_text = true;
                            self.focus_source = true;
                            self.compact_result_tab = 0;
                            ctx.request_repaint();
                        }
                    }
                },
            );
        } else if compact {
            if self.compact_result_tab == 0 {
                self.source_card(ui, ctx, height);
            } else {
                self.translation_card(ui, ctx, height);
            }
        } else {
            ui.spacing_mut().item_spacing.x = 12.0;
            ui.columns(2, |columns| {
                self.source_card(&mut columns[0], ctx, height);
                self.translation_card(&mut columns[1], ctx, height);
            });
        }
        ui.add_space(10.0);
        if notice_height > 0.0 {
            self.notice(ui);
        }
        if !empty || self.busy || notice_height > 0.0 {
            self.status_line(ui);
        }
    }

    fn source_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, height: f32) {
        let p = Palette::get(ctx);
        let width = ui.available_width();
        egui::Frame::new()
            .fill(p.panel)
            .stroke(Stroke::new(1.0_f32, p.border))
            .corner_radius(8.0)
            .show(ui, |ui| {
                ui.set_width(width - 2.0);
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 0.0);
                padded(ui, egui::Margin::symmetric(16, 12), |ui| {
                    ui.horizontal(|ui| {
                        ui.set_height(32.0);
                        let (icon, _) =
                            ui.allocate_exact_size(egui::vec2(21.0, 22.0), Sense::hover());
                        theme::draw_icon(ui, Icon::Text, icon.shrink(2.0), p.muted);
                        ui.label(RichText::new("原文").size(16.0).strong().color(p.text));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_enabled_ui(!self.busy, |ui| {
                                language_menu(
                                    ui,
                                    "source_language",
                                    &mut self.config.source_lang,
                                    LANGUAGES,
                                )
                            });
                        });
                    });
                });
                horizontal_rule(ui, p.border);
                // Header 56 + editor padding 32 + footer 48 + rules/border 4.
                let body_height = (height - 140.0).max(30.0);
                let enabled = !self.busy;
                padded(ui, egui::Margin::same(16), |ui| {
                    let size = egui::vec2(ui.available_width(), body_height);
                    if self.source.is_empty()
                        && !self.editing_text
                        && !self.focus_source
                        && !self.native_editors.is_focused(0)
                    {
                        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
                        #[cfg(debug_assertions)]
                        ui.ctx().data_mut(|data| {
                            data.insert_temp(egui::Id::new("smoke_source_body_rect"), rect)
                        });
                        empty_state(
                            ui,
                            rect,
                            Icon::Scan,
                            "开始识别文字",
                            "新建截图，或点击这里输入文字",
                            &p,
                            &response,
                            enabled,
                        );
                        if enabled && response.clicked() {
                            self.editing_text = true;
                            self.native_editors
                                .show_at(0, ui, &self.source, rect, true, true);
                            self.focus_source = false;
                        }
                    } else {
                        let response = self.native_editors.show(
                            0,
                            ui,
                            &self.source,
                            size,
                            enabled,
                            self.focus_source,
                        );
                        #[cfg(debug_assertions)]
                        ui.ctx().data_mut(|data| {
                            data.insert_temp(egui::Id::new("smoke_source_body_rect"), response.rect)
                        });
                        #[cfg(not(debug_assertions))]
                        let _ = response;
                        self.focus_source = false;
                    }
                });
                horizontal_rule(ui, p.border);
                padded(ui, egui::Margin::symmetric(12, 8), |ui| {
                    ui.horizontal(|ui| {
                        ui.set_height(32.0);
                        let details = if let Some((w, h)) = self.source_image_size {
                            format!("{} 字 · {w} × {h}", self.source.chars().count())
                        } else {
                            format!("{} 字", self.source.chars().count())
                        };
                        ui.label(RichText::new(details).size(11.0).color(p.subtle));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_enabled_ui(!self.source.is_empty(), |ui| {
                                let copied_id = egui::Id::new("copied_source");
                                let copied = motion::flashing(ctx, copied_id, COPIED_FOR);
                                if theme::button_with_feedback(
                                    ui,
                                    "复制原文",
                                    Icon::Copy,
                                    ButtonKind::Ghost,
                                    copied.then_some("已复制"),
                                )
                                .clicked()
                                {
                                    self.copy_output(ctx, self.source.clone());
                                    motion::flash(ctx, copied_id);
                                    self.status = "原文已复制到剪贴板".into();
                                }
                            });
                            if ui
                                .add_enabled_ui(
                                    !self.busy
                                        && (self.editing_text
                                            || !self.source.is_empty()
                                            || !self.translation.is_empty()),
                                    |ui| {
                                        theme::icon_button(ui, Icon::Close, "清空原文和译文", false)
                                    },
                                )
                                .inner
                                .clicked()
                            {
                                self.source.clear();
                                self.translation.clear();
                                self.native_editors.blur();
                                self.editing_text = false;
                                self.source_image_size = None;
                                self.error.clear();
                                self.warning.clear();
                                self.status = "就绪".into();
                            }
                        });
                    });
                });
            });
    }

    fn translation_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, height: f32) {
        let p = Palette::get(ctx);
        let width = ui.available_width();
        egui::Frame::new()
            .fill(p.panel)
            .stroke(Stroke::new(1.0_f32, p.border))
            .corner_radius(8.0)
            .show(ui, |ui| {
                ui.set_width(width - 2.0);
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 0.0);
                padded(ui, egui::Margin::symmetric(16, 12), |ui| {
                    ui.horizontal(|ui| {
                        ui.set_height(32.0);
                        let (icon, _) =
                            ui.allocate_exact_size(egui::vec2(21.0, 22.0), Sense::hover());
                        theme::draw_icon(ui, Icon::Translate, icon.shrink(2.0), p.muted);
                        ui.label(RichText::new("译文").size(16.0).strong().color(p.text));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_enabled_ui(!self.busy, |ui| {
                                language_menu(
                                    ui,
                                    "target_language",
                                    &mut self.config.target_lang,
                                    &LANGUAGES[1..],
                                )
                            });
                            if ui
                                .add_enabled_ui(
                                    !self.busy && self.config.source_lang != "auto",
                                    |ui| {
                                        theme::icon_button(
                                            ui,
                                            Icon::Swap,
                                            "交换源语言与目标语言",
                                            false,
                                        )
                                    },
                                )
                                .inner
                                .clicked()
                            {
                                std::mem::swap(
                                    &mut self.config.source_lang,
                                    &mut self.config.target_lang,
                                );
                            }
                        });
                    });
                });
                horizontal_rule(ui, p.border);
                let body_height = (height - 140.0).max(30.0);
                padded(ui, egui::Margin::same(16), |ui| {
                    if self.translating() {
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), body_height),
                            Sense::hover(),
                        );
                        self.waiting_content(ui, rect);
                        return;
                    }
                    let size = egui::vec2(ui.available_width(), body_height);
                    if self.translation.is_empty()
                        && !self.editing_text
                        && !self.focus_translation
                        && !self.native_editors.is_focused(1)
                    {
                        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
                        #[cfg(debug_assertions)]
                        ui.ctx().data_mut(|data| {
                            data.insert_temp(egui::Id::new("smoke_translation_body_rect"), rect)
                        });
                        empty_state(
                            ui,
                            rect,
                            Icon::Translate,
                            "译文将显示在这里",
                            "点击这里输入文字，或点击「翻译原文」",
                            &p,
                            &response,
                            !self.busy,
                        );
                        if !self.busy && response.clicked() {
                            self.editing_text = true;
                            self.native_editors
                                .show_at(1, ui, &self.translation, rect, true, true);
                            self.focus_translation = false;
                        }
                    } else {
                        let response = self.native_editors.show(
                            1,
                            ui,
                            &self.translation,
                            size,
                            !self.busy,
                            self.focus_translation,
                        );
                        #[cfg(debug_assertions)]
                        ui.ctx().data_mut(|data| {
                            data.insert_temp(
                                egui::Id::new("smoke_translation_body_rect"),
                                response.rect,
                            );
                        });
                        #[cfg(not(debug_assertions))]
                        let _ = response;
                        self.focus_translation = false;
                    }
                });
                horizontal_rule(ui, p.border);
                padded(ui, egui::Margin::symmetric(12, 8), |ui| {
                    ui.horizontal(|ui| {
                        ui.set_height(32.0);
                        ui.add_enabled_ui(!self.busy, |ui| {
                            let provider = theme::elide(
                                ui,
                                &translator_label(
                                    &self.config,
                                    &self.config.last_translate_selection,
                                ),
                                PROVIDER_LABEL_WIDTH,
                            );
                            let choices = translators(&self.config);
                            theme::dropdown(
                                ui,
                                "translation_provider",
                                true,
                                |combo| combo.width(0.0).selected_text(provider),
                                |ui| {
                                    for (code, label) in choices {
                                        theme::menu_option(
                                            ui,
                                            &mut self.config.last_translate_selection,
                                            code,
                                            label,
                                        );
                                    }
                                },
                            );
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let copied_id = egui::Id::new("copied_translation");
                            let copied = motion::flashing(ctx, copied_id, COPIED_FOR);
                            if ui
                                .add_enabled_ui(!self.translation.is_empty(), |ui| {
                                    theme::icon_button_with_feedback(
                                        ui,
                                        Icon::Copy,
                                        "复制译文",
                                        false,
                                        copied,
                                    )
                                })
                                .inner
                                .clicked()
                            {
                                self.copy_output(ctx, self.translation.clone());
                                motion::flash(ctx, copied_id);
                                self.status = "译文已复制到剪贴板".into();
                            }
                            if ui
                                .add_enabled_ui(
                                    !self.busy && !self.source.trim().is_empty(),
                                    |ui| {
                                        theme::button(
                                            ui,
                                            "翻译原文",
                                            Icon::ArrowRight,
                                            ButtonKind::Primary,
                                        )
                                    },
                                )
                                .inner
                                .clicked()
                                && self.begin()
                            {
                                self.submit(Task::Translate(self.source.clone()));
                            }
                        });
                    });
                });
            });
    }

    fn notice(&mut self, ui: &mut egui::Ui) {
        let p = Palette::get(ui.ctx());
        let is_error = !self.error.is_empty();
        let color = if is_error { p.danger } else { p.warning };
        let message = if is_error { &self.error } else { &self.warning };
        let shown = motion::appear(
            ui.ctx(),
            egui::Id::new("workbench_notice"),
            motion::key(message),
        );
        let mut dismissed = false;
        ui.scope(|ui| {
            ui.multiply_opacity(shown);
            egui::Frame::new()
                .fill(color.gamma_multiply(0.08))
                .stroke(Stroke::new(1.0_f32, color.gamma_multiply(0.25)))
                .corner_radius(8.0)
                .inner_margin(10.0)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.set_min_height(34.0);
                    ui.horizontal(|ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(18.0, 18.0), Sense::hover());
                        theme::draw_icon(ui, Icon::Info, rect, color);
                        let text_width = (ui.available_width() - 40.0).max(0.0);
                        ui.allocate_ui_with_layout(
                            egui::vec2(text_width, 34.0),
                            Layout::top_down(Align::Min),
                            |ui| {
                                egui::ScrollArea::vertical()
                                    .id_salt("workbench_notice")
                                    .max_height(34.0)
                                    .min_scrolled_height(0.0)
                                    .auto_shrink([false, true])
                                    .show(ui, |ui| {
                                        ui.label(RichText::new(message).size(12.0).color(color));
                                    });
                            },
                        );
                        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                            ui.spacing_mut().interact_size = egui::vec2(24.0, 24.0);
                            if theme::icon_button(ui, Icon::Close, "关闭提示", false).clicked()
                            {
                                dismissed = true;
                            }
                        });
                    });
                });
        });
        if dismissed {
            self.error.clear();
            self.warning.clear();
        }
        ui.add_space(6.0);
    }

    fn status_line(&mut self, ui: &mut egui::Ui) {
        let p = Palette::get(ui.ctx());
        ui.horizontal(|ui| {
            if self.busy {
                ui.add(egui::Spinner::new().size(12.0).color(p.accent));
            } else {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), Sense::hover());
                ui.painter().circle_filled(
                    rect.center(),
                    3.0,
                    if self.error.is_empty() {
                        p.success
                    } else {
                        p.danger
                    },
                );
            }
            let status = if self.status.starts_with("就绪") {
                "就绪 · 新建截图，或在原文区粘贴文字"
            } else {
                self.status.as_str()
            };
            let cancel_available = self.translating()
                && self.compact_result_tab == 0
                && ui.available_width() < 800.0
                && !self.cancelled;
            let text_width =
                (ui.available_width() - if cancel_available { 88.0 } else { 0.0 }).max(0.0);
            let shown = motion::appear(ui.ctx(), egui::Id::new("status_line"), motion::key(status));
            ui.allocate_ui_with_layout(
                egui::vec2(text_width, 24.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.add_space((1.0 - shown) * 6.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(status)
                                .size(12.0)
                                .color(p.muted.gamma_multiply(0.35 + 0.65 * shown)),
                        )
                        .truncate(),
                    );
                },
            );
            if cancel_available && ui.small_button("取消任务").clicked() {
                self.cancel_job();
            }
        });
    }
}

fn mode_button(
    ui: &mut egui::Ui,
    icon: Icon,
    label: &str,
    selected: bool,
    p: &Palette,
) -> egui::Response {
    // The enclosing one-pixel frame brings the total height to the 32px command row.
    let (rect, response) = ui.allocate_exact_size(egui::vec2(36.0, 30.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::RadioButton,
            ui.is_enabled(),
            selected,
            label,
        )
    });
    let hover = motion::toggle(
        ui.ctx(),
        response.id.with("hover"),
        response.hovered() && !selected && ui.is_enabled(),
        motion::FAST,
    );
    let active = motion::toggle(ui.ctx(), response.id.with("active"), selected, motion::FAST);
    let color = if ui.is_enabled() {
        motion::mix(p.muted, p.text, active.max(hover))
    } else {
        p.subtle
    };
    if hover > 0.0 {
        ui.painter()
            .rect_filled(rect, 4.0, p.border.gamma_multiply(0.4 * hover));
    }
    theme::draw_icon(
        ui,
        icon,
        egui::Rect::from_center_size(rect.center(), egui::vec2(18.0, 18.0)),
        color,
    );
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect,
            4.0,
            Stroke::new(1.0_f32, p.accent),
            egui::StrokeKind::Inside,
        );
    }

    response.on_hover_text(label)
}

/// Fill a reserved painter slot with a pill that slides to `target`.
fn sliding_pill(
    ui: &egui::Ui,
    slot: egui::layers::ShapeIdx,
    id: &str,
    target: egui::Rect,
    tile: impl FnOnce(egui::Rect) -> Vec<egui::Shape>,
) {
    let x = motion::value(ui.ctx(), egui::Id::new(id), target.left(), motion::SLIDE);
    let rect = egui::Rect::from_min_size(egui::pos2(x, target.top()), target.size());
    ui.painter().set(slot, egui::Shape::Vec(tile(rect)));
}

/// A small accent pill announcing a new version; opens the About page.
fn update_pill(ui: &mut egui::Ui, p: &Palette, version: &str, ready: bool) -> egui::Response {
    let text = if ready {
        format!("v{version} 可安装")
    } else {
        format!("新版本 v{version}")
    };
    let galley = ui
        .painter()
        .layout_no_wrap(text, FontId::proportional(12.0), p.accent);
    let size = egui::vec2(galley.size().x + 26.0, 26.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let hover = motion::toggle(
        ui.ctx(),
        response.id.with("hover"),
        response.hovered(),
        motion::FAST,
    );
    ui.painter().rect(
        rect,
        13.0,
        motion::mix(p.accent_soft, p.accent_soft.gamma_multiply(0.7), hover),
        Stroke::new(1.0_f32, p.accent.gamma_multiply(0.35 + 0.4 * hover)),
        egui::StrokeKind::Inside,
    );
    ui.painter()
        .circle_filled(rect.left_center() + egui::vec2(11.0, 0.0), 3.0, p.accent);
    ui.painter().galley(
        egui::pos2(rect.left() + 19.0, rect.center().y - galley.size().y / 2.0),
        galley,
        p.accent,
    );
    response.on_hover_text("查看更新")
}

fn toolbar_separator(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(1.0, 24.0), Sense::hover());
    ui.painter()
        .vline(rect.center().x, rect.y_range(), Stroke::new(1.0_f32, color));
}

fn padded<R>(
    ui: &mut egui::Ui,
    padding: egui::Margin,
    content: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    egui::Frame::new()
        .inner_margin(padding)
        .show(ui, content)
        .inner
}

fn horizontal_rule(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter()
        .hline(rect.x_range(), rect.center().y, Stroke::new(1.0_f32, color));
}

fn segment(ui: &mut egui::Ui, icon: Icon, label: &str, selected: bool) -> egui::Response {
    let p = Palette::get(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(egui::vec2(73.0, 28.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, selected, label)
    });
    let hover = motion::toggle(
        ui.ctx(),
        response.id.with("hover"),
        response.hovered() && !selected,
        motion::FAST,
    );
    let active = motion::toggle(ui.ctx(), response.id.with("active"), selected, motion::FAST);
    if hover > 0.0 {
        ui.painter()
            .rect_filled(rect, 4.0, p.border.gamma_multiply(0.35 * hover));
    }
    let color = motion::mix(p.muted, p.accent, active);
    theme::draw_icon(
        ui,
        icon,
        egui::Rect::from_center_size(
            rect.left_center() + egui::vec2(17.0, 0.0),
            egui::vec2(15.0, 15.0),
        ),
        color,
    );
    ui.painter().text(
        rect.left_center() + egui::vec2(31.0, 0.0),
        egui::Align2::LEFT_CENTER,
        label,
        FontId::proportional(12.0),
        color,
    );
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect,
            4.0,
            Stroke::new(1.0_f32, p.accent),
            egui::StrokeKind::Inside,
        );
    }
    response
}

#[allow(clippy::too_many_arguments)]
fn empty_state(
    ui: &egui::Ui,
    rect: egui::Rect,
    icon: Icon,
    title: &str,
    hint: &str,
    p: &Palette,
    response: &egui::Response,
    enabled: bool,
) {
    // The whole area is clickable; say so on hover with a soft frame and a lifted mark.
    let hover = motion::toggle(
        ui.ctx(),
        response.id.with("hover"),
        enabled && response.hovered(),
        motion::FAST,
    );
    if enabled && response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
    }
    if hover > 0.0 {
        ui.painter().rect(
            rect.shrink(2.0),
            6.0,
            p.accent_soft.gamma_multiply(0.35 * hover),
            Stroke::new(1.0_f32, p.accent.gamma_multiply(0.35 * hover)),
            egui::StrokeKind::Inside,
        );
    }
    let center = rect.center() - egui::vec2(0.0, 10.0);
    let mark = egui::Rect::from_center_size(
        center - egui::vec2(0.0, 32.0 + 2.0 * hover),
        egui::vec2(54.0, 54.0),
    );
    if rect.height() < 120.0 {
        return;
    }
    ui.painter().rect_filled(mark, 8.0, p.accent_soft);
    theme::draw_icon(ui, icon, mark.shrink(15.0), p.accent);
    ui.painter().text(
        center + egui::vec2(0.0, 14.0),
        egui::Align2::CENTER_CENTER,
        title,
        FontId::proportional(14.0),
        motion::mix(p.muted, p.text, hover),
    );
    ui.painter().text(
        center + egui::vec2(0.0, 40.0),
        egui::Align2::CENTER_CENTER,
        hint,
        FontId::proportional(11.0),
        p.subtle,
    );
}

fn language_menu(ui: &mut egui::Ui, id: &str, value: &mut String, items: &[(&str, &str)]) {
    let current = provider_label(value, items).to_string();
    let response = theme::dropdown(
        ui,
        id,
        true,
        |combo| combo.width(120.0).selected_text(current),
        |ui| {
            for (code, label) in items {
                let choice = theme::menu_option(ui, value, (*code).into(), *label);
                #[cfg(debug_assertions)]
                if id == "source_language" && *code == "en" {
                    ui.ctx().data_mut(|data| {
                        data.insert_temp(
                            egui::Id::new("smoke_source_language_en_rect"),
                            choice.rect,
                        )
                    });
                }
                #[cfg(not(debug_assertions))]
                let _ = choice;
            }
        },
    );
    #[cfg(debug_assertions)]
    if id == "source_language" {
        ui.ctx().data_mut(|data| {
            data.insert_temp(
                egui::Id::new("smoke_source_language_rect"),
                response.response.rect,
            )
        });
    }
    #[cfg(not(debug_assertions))]
    let _ = response;
}

/// Longest engine / translator name shown on a dropdown button before "…".
const ENGINE_LABEL_WIDTH: f32 = 120.0;
const PROVIDER_LABEL_WIDTH: f32 = 110.0;

fn translation_provider_menu(ui: &mut egui::Ui, id: &str, config: &mut Config) {
    let choices = translators(config);
    let current = theme::elide(
        ui,
        &translator_label(config, &config.last_translate_selection),
        PROVIDER_LABEL_WIDTH,
    );
    theme::dropdown(
        ui,
        id,
        true,
        |combo| combo.width(0.0).selected_text(current),
        |ui| {
            for (code, label) in choices {
                theme::menu_option(ui, &mut config.last_translate_selection, code, label);
            }
        },
    );
}

/// Built-in translators followed by every model interface that can translate.
pub(super) fn translators(config: &Config) -> Vec<(String, String)> {
    [("默认", "Bing"), ("Baidu", "百度"), ("Tencent", "腾讯")]
        .into_iter()
        .map(|(code, label)| (code.to_owned(), label.to_owned()))
        .chain(
            config
                .model_profiles
                .iter()
                .filter(|profile| profile.can_translate())
                .map(|profile| {
                    (
                        format!("{MODEL_SELECTION_PREFIX}{}", profile.id),
                        profile.name.clone(),
                    )
                }),
        )
        .collect()
}

pub(super) fn translator_label(config: &Config, selection: &str) -> String {
    if let Some(id) = model_translator(selection) {
        return config
            .profile(id)
            .map_or_else(|| "模型接口".into(), |profile| profile.name.clone());
    }
    match selection {
        "Baidu" => "百度",
        "Tencent" => "腾讯",
        _ => "Bing",
    }
    .into()
}

fn provider_label<'a>(value: &'a str, items: &'a [(&str, &str)]) -> &'a str {
    items
        .iter()
        .find(|(code, _)| *code == value)
        .map(|(_, label)| *label)
        .unwrap_or(value)
}

fn content_kind(selection: &str) -> usize {
    if selection.ends_with("_table") {
        1
    } else if selection.ends_with("_formula") {
        2
    } else {
        0
    }
}
/// The recognition engine part of an OCR selection.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Engine {
    Local,
    Baidu,
    Tencent,
    Model(String),
}

impl Engine {
    fn of(selection: &str) -> Self {
        if selection.starts_with("Baidu_") {
            Self::Baidu
        } else if selection.starts_with("Tencent_") {
            Self::Tencent
        } else if let Some((id, _)) = model_ocr_selection(selection) {
            Self::Model(id.to_owned())
        } else {
            Self::Local
        }
    }

    fn label(&self, config: &Config) -> String {
        match self {
            Self::Local => "本地 OCR".into(),
            Self::Baidu => "百度 OCR".into(),
            Self::Tencent => "腾讯 OCR".into(),
            Self::Model(id) => config
                .profile(id)
                .map_or_else(|| "模型接口".into(), |profile| profile.name.clone()),
        }
    }

    /// The saved selection for this engine and content kind (0 text, 1 table, 2 formula).
    fn selection(&self, kind: usize) -> String {
        let mode = ["auto", "table", "formula"][kind.min(2)];
        match self {
            Self::Local if kind == 1 => "默认_table".into(),
            Self::Local => "默认".into(),
            Self::Baidu => format!("Baidu_{mode}"),
            Self::Tencent => format!("Tencent_{mode}"),
            Self::Model(id) => format!("{MODEL_SELECTION_PREFIX}{id}_{mode}"),
        }
    }
}

pub(super) fn ocr_provider_label(config: &Config, selection: &str) -> String {
    Engine::of(selection).label(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engines_round_trip_through_toolbar_selections_without_resetting_provider() {
        for engine in [
            Engine::Baidu,
            Engine::Tencent,
            Engine::Model("tokenhub".into()),
            Engine::Model("local-1".into()),
        ] {
            for kind in 0..=2 {
                let id = engine.selection(kind);
                assert_eq!(Engine::of(&id), engine, "{id}");
                assert_eq!(content_kind(&id), kind, "{id}");
            }
        }
        assert_eq!(Engine::Local.selection(1), "默认_table");
        assert_eq!(Engine::of("默认"), Engine::Local);
        assert_eq!(Engine::of("Baidu_accurate_basic"), Engine::Baidu);
        assert_eq!(content_kind("Tencent_general_accurate"), 0);
        let config = Config::default();
        assert_eq!(translator_label(&config, "Model.removed"), "模型接口");
        assert_eq!(translators(&config).len(), 3);
    }
}
