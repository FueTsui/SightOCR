use super::{
    theme::{self, ButtonKind, Icon, Palette},
    App, LANGUAGES, OCR_MODES, TRANSLATORS,
};
use eframe::egui::{
    self, Align, Color32, FontId, Layout, RichText, Sense, Stroke, ViewportCommand,
};
use sightocr::worker::Task;

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
                            .fill(p.panel_alt)
                            .stroke(Stroke::new(1.0_f32, p.border))
                            .corner_radius(4.0)
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                ui.horizontal(|ui| {
                                    if mode_button(
                                        ui,
                                        Icon::Scan,
                                        "截图识别",
                                        !self.capture_translate,
                                        &p,
                                    )
                                    .clicked()
                                    {
                                        self.capture_translate = false;
                                    }
                                    if mode_button(
                                        ui,
                                        Icon::Translate,
                                        "截图并翻译",
                                        self.capture_translate,
                                        &p,
                                    )
                                    .clicked()
                                    {
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
        let mut provider = provider(&self.config.last_ocr_selection);
        let old_provider = provider;
        let (kind_icon, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), Sense::hover());
        theme::draw_icon(
            ui,
            [Icon::Text, Icon::Table, Icon::Formula][kind],
            kind_icon,
            Palette::get(ui.ctx()).muted,
        );
        egui::ComboBox::from_id_salt("recognition_kind")
            .icon(theme::combo_icon)
            .width(80.0)
            .selected_text(["文字", "表格", "公式"][kind])
            .show_ui(ui, |ui| {
                for (index, label) in ["文字", "表格", "公式"].into_iter().enumerate() {
                    let response = ui
                        .add_enabled_ui(index != 2 || provider != 0, |ui| {
                            ui.selectable_value(&mut kind, index, label)
                        })
                        .inner;
                    if index == 2 && provider == 0 {
                        response.on_hover_text("公式识别需要云端引擎");
                    }
                }
            })
            .response
            .on_hover_text("识别内容类型");
        egui::ComboBox::from_id_salt("recognition_provider")
            .icon(theme::combo_icon)
            .width(134.0)
            .selected_text(OCR_PROVIDERS[provider])
            .show_ui(ui, |ui| {
                for (index, label) in OCR_PROVIDERS.iter().enumerate() {
                    ui.selectable_value(&mut provider, index, *label);
                }
                if matches!(old_provider, 1 | 2) && kind == 0 {
                    ui.separator();
                    ui.label("识别精度");
                    for (id, label) in OCR_MODES.iter().filter(|(id, _)| {
                        provider_matches(id, old_provider) && content_kind(id) == 0
                    }) {
                        if ui
                            .selectable_value(
                                &mut self.config.last_ocr_selection,
                                (*id).into(),
                                *label,
                            )
                            .clicked()
                        {
                            ui.close_menu();
                        }
                    }
                }
            })
            .response
            .on_hover_text(if provider == 0 {
                "本地离线识别"
            } else {
                "云端识别，密钥可在设置 → 接口中配置"
            });
        if provider != old_provider || kind != old_kind {
            if provider == 0 && kind == 2 {
                kind = 0;
                self.status = "本地引擎支持文字与表格，已选择文字识别".into();
            }
            self.config.last_ocr_selection = selection(provider, kind).into();
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
                if segment(ui, Icon::Text, "原文", self.compact_result_tab == 0).clicked() {
                    self.compact_result_tab = 0;
                }
                if segment(ui, Icon::Translate, "译文", self.compact_result_tab == 1).clicked() {
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
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(content)
                    .layout(Layout::top_down(Align::Center)),
                |ui| {
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
                                        &mut self.config.last_translate_selection,
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
                                if theme::button(ui, "复制原文", Icon::Copy, ButtonKind::Ghost)
                                    .clicked()
                                {
                                    self.copy_output(ctx, self.source.clone());
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
                            egui::ComboBox::from_id_salt("translation_provider")
                                .icon(theme::combo_icon)
                                .width(90.0)
                                .selected_text(provider_label(
                                    &self.config.last_translate_selection,
                                    TRANSLATORS,
                                ))
                                .show_ui(ui, |ui| {
                                    for (code, label) in TRANSLATORS {
                                        ui.selectable_value(
                                            &mut self.config.last_translate_selection,
                                            (*code).into(),
                                            *label,
                                        );
                                    }
                                });
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .add_enabled_ui(!self.translation.is_empty(), |ui| {
                                    theme::icon_button(ui, Icon::Copy, "复制译文", false)
                                })
                                .inner
                                .clicked()
                            {
                                self.copy_output(ctx, self.translation.clone());
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
        egui::Frame::new()
            .fill(color.gamma_multiply(0.08))
            .stroke(Stroke::new(1.0_f32, color.gamma_multiply(0.25)))
            .corner_radius(8.0)
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(34.0);
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), Sense::hover());
                    theme::draw_icon(ui, Icon::Info, rect, color);
                    egui::ScrollArea::vertical()
                        .id_salt("workbench_notice")
                        .max_height(34.0)
                        .min_scrolled_height(0.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(if is_error { &self.error } else { &self.warning })
                                    .size(12.0)
                                    .color(color),
                            );
                        });
                });
            });
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
            ui.allocate_ui_with_layout(
                egui::vec2(text_width, 24.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.add(
                        egui::Label::new(RichText::new(status).size(12.0).color(p.muted))
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
    let color = if ui.is_enabled() { p.text } else { p.subtle };
    if selected || response.hovered() {
        ui.painter().rect_filled(
            rect,
            4.0,
            if selected {
                p.panel
            } else {
                p.border.gamma_multiply(0.4)
            },
        );
    }
    theme::draw_icon(
        ui,
        icon,
        egui::Rect::from_center_size(rect.center(), egui::vec2(18.0, 18.0)),
        color,
    );
    if selected {
        ui.painter().rect_filled(
            egui::Rect::from_center_size(
                rect.center_bottom() - egui::vec2(0.0, 2.0),
                egui::vec2(16.0, 3.0),
            ),
            2.0,
            p.accent,
        );
    }
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
    let response = ui.add(
        egui::Button::new(
            RichText::new(format!("     {label}"))
                .size(12.0)
                .color(if selected { p.accent } else { p.muted }),
        )
        .fill(if selected {
            p.panel
        } else {
            Color32::TRANSPARENT
        })
        .stroke(if selected {
            Stroke::new(1.0_f32, p.border)
        } else {
            Stroke::NONE
        })
        .corner_radius(4.0)
        .min_size(egui::vec2(73.0, 28.0)),
    );
    theme::draw_icon(
        ui,
        icon,
        egui::Rect::from_center_size(
            response.rect.left_center() + egui::vec2(17.0, 0.0),
            egui::vec2(15.0, 15.0),
        ),
        if selected { p.accent } else { p.muted },
    );
    response
}

fn empty_state(ui: &egui::Ui, rect: egui::Rect, icon: Icon, title: &str, hint: &str, p: &Palette) {
    let center = rect.center() - egui::vec2(0.0, 10.0);
    let mark = egui::Rect::from_center_size(center - egui::vec2(0.0, 32.0), egui::vec2(54.0, 54.0));
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
        p.muted,
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
    let response = egui::ComboBox::from_id_salt(id)
        .icon(theme::combo_icon)
        .width(120.0)
        .selected_text(provider_label(value, items))
        .show_ui(ui, |ui| {
            for (code, label) in items {
                let choice = ui.selectable_value(value, (*code).into(), *label);
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
        });
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

fn translation_provider_menu(ui: &mut egui::Ui, id: &str, value: &mut String) {
    egui::ComboBox::from_id_salt(id)
        .icon(theme::combo_icon)
        .width(90.0)
        .selected_text(provider_label(value, TRANSLATORS))
        .show_ui(ui, |ui| {
            for (code, label) in TRANSLATORS {
                ui.selectable_value(value, (*code).into(), *label);
            }
        });
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
const OCR_PROVIDERS: &[&str] = &[
    "本地 OneOCR",
    "百度 OCR",
    "腾讯 OCR",
    "Mistral OCR",
    "OpenAI",
    "NVIDIA",
];

pub(super) fn ocr_provider_label(selection: &str) -> &'static str {
    OCR_PROVIDERS[provider(selection)]
}

fn provider(selection: &str) -> usize {
    if selection.starts_with("Baidu_") {
        1
    } else if selection.starts_with("Tencent_") {
        2
    } else if selection.starts_with("Mistral_") {
        3
    } else if selection.starts_with("OpenAI_") {
        4
    } else if selection.starts_with("Nvidia_") {
        5
    } else {
        0
    }
}
fn provider_matches(selection: &str, selected: usize) -> bool {
    provider(selection) == selected
}
fn selection(provider: usize, kind: usize) -> &'static str {
    match (provider, kind) {
        (0, 1) => "默认_table",
        (1, 0) => "Baidu_auto",
        (1, 1) => "Baidu_table",
        (1, 2) => "Baidu_formula",
        (2, 0) => "Tencent_auto",
        (2, 1) => "Tencent_table",
        (2, 2) => "Tencent_formula",
        (3, 0) => "Mistral_auto",
        (3, 1) => "Mistral_table",
        (3, 2) => "Mistral_formula",
        (4, 0) => "OpenAI_auto",
        (4, 1) => "OpenAI_table",
        (4, 2) => "OpenAI_formula",
        (5, 0) => "Nvidia_auto",
        (5, 1) => "Nvidia_table",
        (5, 2) => "Nvidia_formula",
        _ => "默认",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_modes_round_trip_through_toolbar_without_resetting_provider() {
        for engine in 1..OCR_PROVIDERS.len() {
            for kind in 0..=2 {
                let id = selection(engine, kind);
                assert_eq!(provider(id), engine, "{id}");
                assert_eq!(content_kind(id), kind, "{id}");
            }
        }
        assert_eq!(selection(0, 1), "默认_table");
        assert_eq!(provider("Baidu_accurate_basic"), 1);
        assert_eq!(content_kind("Tencent_general_accurate"), 0);
    }
}
