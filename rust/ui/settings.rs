use super::{
    hotkey_input::{self, Target},
    theme::{self, ButtonKind, Icon, Palette},
    App,
};
use eframe::egui::{self, Align, FontId, Layout, RichText, Sense, Stroke};
use sightocr::config::{Config, ProxyMode};

pub(super) const HOTKEYS_TAB: usize = 4;

const SERVICE_NAMES: &[&str] = &[
    "百度 OCR",
    "腾讯 OCR",
    "百度翻译",
    "腾讯翻译",
    "Mistral OCR",
    "OpenAI",
    "NVIDIA",
];

impl App {
    pub(super) fn settings_page(&mut self, ui: &mut egui::Ui) {
        let palette = Palette::get(ui.ctx());
        let p = &palette;
        ui.spacing_mut().item_spacing = egui::vec2(12.0, 10.0);
        ui.spacing_mut().scroll = egui::style::ScrollStyle {
            floating: true,
            bar_width: 12.0,
            floating_width: 6.0,
            floating_allocated_width: 0.0,
            handle_min_length: 36.0,
            bar_inner_margin: 0.0,
            bar_outer_margin: 2.0,
            foreground_color: true,
            dormant_handle_opacity: 0.56,
            active_handle_opacity: 0.68,
            interact_handle_opacity: 0.85,
            dormant_background_opacity: 0.0,
            active_background_opacity: 0.0,
            interact_background_opacity: 0.65,
        };

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            for (index, title) in [
                (0, "常规"),
                (1, "接口"),
                (HOTKEYS_TAB, "快捷键"),
                (3, "代理"),
                (2, "关于"),
            ] {
                if pivot_tab(ui, p, title, self.settings_tab == index).clicked() {
                    if index != HOTKEYS_TAB {
                        hotkey_input::clear(ui.ctx());
                    }
                    self.settings_tab = index;
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if action_button(ui, p, "返回", false).clicked() {
                    hotkey_input::clear(ui.ctx());
                    self.settings_open = false;
                    self.settings_error.clear();
                }
            });
        });
        ui.add_space(4.0);

        // The main content scrolls independently of the save/cancel controls.
        let bottom = ui.max_rect().bottom();
        let error_height = if self.settings_error.is_empty() {
            0.0
        } else {
            78.0
        };
        let footer_height = 64.0 + error_height;
        let body_height = (bottom - ui.cursor().top() - footer_height - 12.0).max(48.0);
        egui::ScrollArea::vertical()
            .id_salt(("settings_body", self.settings_tab))
            .max_height(body_height)
            .min_scrolled_height(body_height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Keep a stable gutter even when the floating scrollbar appears or expands.
                ui.set_width((ui.available_width() - 18.0).max(0.0));
                match self.settings_tab {
                    1 => self.service_settings(ui, p),
                    2 => self.about_settings(ui, p),
                    3 => self.proxy_settings(ui, p),
                    HOTKEYS_TAB => self.hotkey_settings(ui, p),
                    _ => self.general_settings(ui, p),
                }
                ui.add_space(4.0);
            });

        let footer_top = (bottom - footer_height).max(ui.cursor().top());
        let footer_rect = egui::Rect::from_min_max(
            egui::pos2(ui.max_rect().left(), footer_top),
            egui::pos2(ui.max_rect().right(), bottom),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(footer_rect), |ui| {
            ui.painter().line_segment(
                [footer_rect.left_top(), footer_rect.right_top()],
                Stroke::new(1.0_f32, p.border),
            );
            ui.add_space(12.0);
            if !self.settings_error.is_empty() {
                egui::Frame::new()
                    .fill(p.danger.gamma_multiply(0.10))
                    .corner_radius(4)
                    .inner_margin(10)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        egui::ScrollArea::vertical()
                            .id_salt("settings_save_error")
                            .max_height(46.0)
                            .min_scrolled_height(0.0)
                            .show(ui, |ui| {
                                ui.set_width((ui.available_width() - 18.0).max(0.0));
                                ui.label(
                                    RichText::new(format!("保存未完成：{}", self.settings_error))
                                        .size(12.0)
                                        .color(p.danger),
                                );
                            });
                    });
            }
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if action_button(ui, p, "保存", true).clicked() {
                        match self.save_draft() {
                            Ok(()) => {
                                let preference = self
                                    .config
                                    .extra
                                    .get("appearance")
                                    .and_then(|value| value.as_str())
                                    .unwrap_or("system");
                                ui.ctx().set_theme(match preference {
                                    "light" => egui::ThemePreference::Light,
                                    "dark" => egui::ThemePreference::Dark,
                                    _ => egui::ThemePreference::System,
                                });
                                self.settings_error.clear();
                                self.settings_open = false;
                                hotkey_input::clear(ui.ctx());
                            }
                            Err(error) => self.settings_error = format!("{error:#}"),
                        }
                    }
                    if action_button(ui, p, "取消", false).clicked() {
                        hotkey_input::clear(ui.ctx());
                        self.settings_open = false;
                        self.settings_error.clear();
                    }
                });
            });
        });
    }

    fn general_settings(&mut self, ui: &mut egui::Ui, p: &Palette) {
        appearance_card(ui, p, &mut self.draft);
        ui.add_space(6.0);
        output_card(ui, p, &mut self.draft.replace_newline);
        ui.add_space(6.0);
        startup_card(
            ui,
            p,
            &mut self.draft.autostart,
            &mut self.draft.hide_tray_icon,
        );
    }

    fn hotkey_settings(&mut self, ui: &mut egui::Ui, p: &Palette) {
        hotkey_card(
            ui,
            p,
            &self.draft.hotkey,
            &self.draft.translate_hotkey,
            &self.draft.silent_hotkey,
        );
    }

    fn service_settings(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let selection_id = egui::Id::new("settings_service_provider");
        let mut selected = ui
            .ctx()
            .data_mut(|data| data.get_temp::<usize>(selection_id).unwrap_or(0))
            .min(SERVICE_NAMES.len() - 1);
        card(ui, p, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("配置服务").size(14.0).color(p.text));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    egui::ComboBox::from_id_salt("settings_service_selector")
                        .icon(theme::combo_icon)
                        .width(240.0)
                        .selected_text(SERVICE_NAMES[selected])
                        .show_ui(ui, |ui| {
                            for (index, title) in SERVICE_NAMES.iter().enumerate() {
                                ui.selectable_value(&mut selected, index, *title);
                            }
                        });
                });
            });
        });
        ui.ctx()
            .data_mut(|data| data.insert_temp(selection_id, selected));
        ui.add_space(4.0);

        let defaults = Config::default();
        match selected {
            0 => baidu_ocr_card(ui, p, &mut self.draft.api_key, &mut self.draft.secret_key),
            1 => tencent_ocr_card(
                ui,
                p,
                &mut self.draft.tencent_secret_id,
                &mut self.draft.tencent_secret_key,
            ),
            2 => baidu_translation_card(
                ui,
                p,
                &mut self.draft.baidu_trans_appid,
                &mut self.draft.baidu_trans_appkey,
            ),
            3 => tencent_translation_card(
                ui,
                p,
                &mut self.draft.tencent_trans_secret_id,
                &mut self.draft.tencent_trans_secret_key,
            ),
            4 => model_service_card(
                ui,
                p,
                ModelService {
                    title: "Mistral OCR",
                    description: "截图文字识别",
                    api_key: &mut self.draft.mistral_api_key,
                    base_url: &mut self.draft.mistral_base_url,
                    ocr_model: &mut self.draft.mistral_ocr_model,
                    translate_model: None,
                    default_base_url: &defaults.mistral_base_url,
                    default_ocr_model: &defaults.mistral_ocr_model,
                    default_translate_model: "",
                },
            ),
            5 => model_service_card(
                ui,
                p,
                ModelService {
                    title: "OpenAI",
                    description: "截图文字识别与文本翻译",
                    api_key: &mut self.draft.openai_api_key,
                    base_url: &mut self.draft.openai_base_url,
                    ocr_model: &mut self.draft.openai_ocr_model,
                    translate_model: Some(&mut self.draft.openai_translate_model),
                    default_base_url: &defaults.openai_base_url,
                    default_ocr_model: &defaults.openai_ocr_model,
                    default_translate_model: &defaults.openai_translate_model,
                },
            ),
            _ => model_service_card(
                ui,
                p,
                ModelService {
                    title: "NVIDIA",
                    description: "截图文字识别与文本翻译",
                    api_key: &mut self.draft.nvidia_api_key,
                    base_url: &mut self.draft.nvidia_base_url,
                    ocr_model: &mut self.draft.nvidia_ocr_model,
                    translate_model: Some(&mut self.draft.nvidia_translate_model),
                    default_base_url: &defaults.nvidia_base_url,
                    default_ocr_model: &defaults.nvidia_ocr_model,
                    default_translate_model: &defaults.nvidia_translate_model,
                },
            ),
        }
    }

    fn proxy_settings(&mut self, ui: &mut egui::Ui, p: &Palette) {
        card(ui, p, |ui| {
            let control_width = 210.0_f32.min(ui.available_width() * 0.45);
            let label_width = ui.available_width() - control_width - ui.spacing().item_spacing.x;
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(label_width, 0.0),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.set_width(label_width);
                        setting_description(ui, p, "代理模式", "");
                    },
                );
                let selected = match self.draft.proxy.mode {
                    ProxyMode::System => "跟随系统（默认）",
                    ProxyMode::Direct => "不使用代理",
                    ProxyMode::Manual => "手动代理",
                };
                egui::ComboBox::from_id_salt("settings_proxy_mode")
                    .icon(theme::combo_icon)
                    .width(control_width)
                    .selected_text(selected)
                    .show_ui(ui, |ui| {
                        for (mode, label) in [
                            (ProxyMode::System, "跟随系统（默认）"),
                            (ProxyMode::Direct, "不使用代理"),
                            (ProxyMode::Manual, "手动代理"),
                        ] {
                            ui.selectable_value(&mut self.draft.proxy.mode, mode, label);
                        }
                    });
            });
        });
        ui.add_space(4.0);

        let manual = self.draft.proxy.mode == ProxyMode::Manual;
        ui.add_enabled_ui(manual, |ui| {
            card(ui, p, |ui| {
                card_title(ui, p, "手动代理", "支持 HTTP、HTTPS、SOCKS5 和 SOCKS5H");
                plain_field(
                    ui,
                    p,
                    "代理地址",
                    &mut self.draft.proxy.url,
                    "例如 http://127.0.0.1:7890",
                );
                ui.add_space(4.0);
                if ui.available_width() >= 570.0 {
                    ui.columns(2, |columns| {
                        proxy_username_field(&mut columns[0], p, &mut self.draft.proxy.username);
                        secret_field_with_hint(
                            &mut columns[1],
                            p,
                            "密码（可选）",
                            &mut self.draft.proxy.password,
                            "无需认证时留空",
                        );
                    });
                } else {
                    proxy_username_field(ui, p, &mut self.draft.proxy.username);
                    secret_field_with_hint(
                        ui,
                        p,
                        "密码（可选）",
                        &mut self.draft.proxy.password,
                        "无需认证时留空",
                    );
                }
            });
        });
    }

    fn about_settings(&mut self, ui: &mut egui::Ui, p: &Palette) {
        card(ui, p, |ui| {
            ui.horizontal(|ui| {
                // The original navy logo is transparent; a neutral tile preserves its contrast.
                egui::Frame::new()
                    .fill(egui::Color32::WHITE)
                    .corner_radius(4)
                    .show(ui, |ui| {
                        ui.image((self.logo.id(), egui::vec2(58.0, 58.0)));
                    });
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("SightOCR").size(24.0).strong().color(p.text));
                        badge(ui, p, &format!("v{}", env!("CARGO_PKG_VERSION")), false);
                    });
                    ui.label(
                        RichText::new("从屏幕上的文字，到手边可用的信息。")
                            .size(13.0)
                            .color(p.muted),
                    );
                });
            });
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                ui.hyperlink_to("项目主页", "https://github.com/FueTsui/SightOCR");
                ui.label(RichText::new("/").color(p.subtle));
                if ui
                    .add_enabled(!self.update.busy, egui::Button::new("检查更新"))
                    .clicked()
                {
                    self.check_for_updates(ui.ctx());
                }
            });
            if !self.update.message.is_empty() {
                ui.label(RichText::new(&self.update.message).size(13.0).color(
                    if self.update.failed {
                        ui.visuals().error_fg_color
                    } else {
                        p.muted
                    },
                ));
                if let Some(fraction) = self.update.fraction {
                    ui.add(egui::ProgressBar::new(fraction).show_percentage());
                }
            }
            ui.label(
                RichText::new("© 2026 FueTsui. All rights reserved.")
                    .size(12.0)
                    .color(p.muted),
            );
        });
        ui.add_space(4.0);
        card(ui, p, |ui| {
            card_title(ui, p, "本机信息", "");
            ui.horizontal(|ui| {
                ui.label(RichText::new("运行版本").size(13.0).color(p.muted));
                ui.label(
                    RichText::new(format!("{} · Rust", env!("CARGO_PKG_VERSION")))
                        .size(13.0)
                        .color(p.text),
                );
            });
            egui::CollapsingHeader::new(RichText::new("配置文件位置").size(13.0).color(p.muted))
                .id_salt("configuration_path")
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(self.path.display().to_string())
                            .size(12.0)
                            .color(p.text),
                    );
                    if action_button(ui, p, "复制路径", false).clicked() {
                        self.copy_output(ui.ctx(), self.path.display().to_string());
                    }
                });
        });
    }
}

fn pivot_tab(ui: &mut egui::Ui, p: &Palette, label: &str, selected: bool) -> egui::Response {
    let font = FontId::proportional(14.0);
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        font,
        if selected { p.text } else { p.muted },
    );
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(galley.size().x + 20.0, 40.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            selected,
            label,
        )
    });
    if response.hovered() {
        ui.painter()
            .rect_filled(rect.shrink2(egui::vec2(0.0, 4.0)), 4, p.panel_alt);
    }
    ui.painter().galley(
        egui::pos2(
            rect.center().x - galley.size().x / 2.0,
            rect.center().y - galley.size().y / 2.0 - 1.0,
        ),
        galley,
        if selected { p.text } else { p.muted },
    );
    if selected {
        ui.painter().rect_filled(
            egui::Rect::from_center_size(
                egui::pos2(rect.center().x, rect.bottom() - 3.0),
                egui::vec2(24.0, 3.0),
            ),
            2,
            p.accent,
        );
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            4,
            Stroke::new(1.0_f32, p.accent),
            egui::StrokeKind::Inside,
        );
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn setting_description(ui: &mut egui::Ui, p: &Palette, title: &str, description: &str) {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        ui.label(RichText::new(title).size(14.0).color(p.text));
        if !description.is_empty() {
            ui.label(RichText::new(description).size(12.0).color(p.muted));
        }
    });
}

fn card(ui: &mut egui::Ui, p: &Palette, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(p.panel)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(8)
        .inner_margin(16)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            contents(ui);
        });
}

fn card_title(ui: &mut egui::Ui, p: &Palette, title: &str, description: &str) {
    ui.label(RichText::new(title).size(15.0).strong().color(p.text));
    if !description.is_empty() {
        ui.label(RichText::new(description).size(12.0).color(p.muted));
        ui.add_space(4.0);
    }
}

fn appearance_card(ui: &mut egui::Ui, p: &Palette, config: &mut Config) {
    let initial = config
        .extra
        .get("appearance")
        .and_then(|value| value.as_str())
        .unwrap_or("system")
        .to_owned();
    let mut selected = initial.clone();
    let choices = &[("system", "跟随系统"), ("light", "浅色"), ("dark", "深色")];
    card(ui, p, |ui| {
        let control_width = 180.0_f32.min(ui.available_width() * 0.4);
        let label_width = ui.available_width() - control_width - ui.spacing().item_spacing.x;
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(label_width, 0.0),
                Layout::top_down(Align::Min),
                |ui| {
                    ui.set_width(label_width);
                    setting_description(ui, p, "应用主题", "");
                },
            );
            select_choice(
                ui,
                "settings_appearance",
                &mut selected,
                choices,
                control_width,
            );
        });
    });
    if selected != initial {
        config.extra.insert("appearance".into(), selected.into());
    }
}

fn select_choice(
    ui: &mut egui::Ui,
    id: &str,
    value: &mut String,
    choices: &[(&str, &str)],
    width: f32,
) {
    let selected = choices
        .iter()
        .find(|(code, _)| *code == value.as_str())
        .map(|(_, title)| *title)
        .unwrap_or(value.as_str());
    egui::ComboBox::from_id_salt(id)
        .icon(theme::combo_icon)
        .width(width)
        .selected_text(selected)
        .show_ui(ui, |ui| {
            for (code, title) in choices {
                ui.selectable_value(value, (*code).to_owned(), *title);
            }
        });
}

fn hotkey_card(ui: &mut egui::Ui, p: &Palette, capture: &str, translate: &str, silent: &str) {
    card(ui, p, |ui| {
        hotkey_field(
            ui,
            p,
            "静默识别",
            "提取文本并复制到剪贴板，不弹出主窗口",
            silent,
            Target::SilentCapture,
        );
        ui.separator();
        hotkey_field(
            ui,
            p,
            "截图识别",
            "提取选中区域的文字",
            capture,
            Target::Capture,
        );
        ui.separator();
        hotkey_field(
            ui,
            p,
            "截图翻译",
            "识别后自动翻译到目标语言",
            translate,
            Target::Translate,
        );
    });
}

fn hotkey_field(
    ui: &mut egui::Ui,
    p: &Palette,
    label: &str,
    description: &str,
    value: &str,
    target: Target,
) {
    if ui.available_width() >= 570.0 {
        let input_width = 260.0_f32.min(ui.available_width() * 0.42);
        let text_width = ui.available_width() - input_width - ui.spacing().item_spacing.x;
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(text_width, 0.0),
                Layout::top_down(Align::Min),
                |ui| {
                    ui.set_width(text_width);
                    setting_description(ui, p, label, description);
                },
            );
            ui.allocate_ui_with_layout(
                egui::vec2(input_width, 34.0),
                Layout::top_down(Align::Min),
                |ui| {
                    ui.set_width(input_width);
                    hotkey_input::field(ui, target, value);
                },
            );
        });
    } else {
        setting_description(ui, p, label, description);
        hotkey_input::field(ui, target, value);
    }
}

fn output_card(ui: &mut egui::Ui, p: &Palette, replace_newline: &mut bool) {
    card(ui, p, |ui| {
        toggle_row(
            ui,
            p,
            replace_newline,
            "合并文本换行",
            "将普通文本合并为一行，表格仍保留行列结构。",
        );
    });
}

fn startup_card(ui: &mut egui::Ui, p: &Palette, autostart: &mut bool, hide_tray: &mut bool) {
    card(ui, p, |ui| {
        toggle_row(ui, p, autostart, "开机自动启动", "");
        ui.separator();
        toggle_row(ui, p, hide_tray, "隐藏托盘图标", "");
    });
}

fn toggle_row(ui: &mut egui::Ui, p: &Palette, value: &mut bool, label: &str, help: &str) {
    ui.horizontal(|ui| {
        let text_width = (ui.available_width() - 40.0 - ui.spacing().item_spacing.x).max(100.0);
        ui.allocate_ui_with_layout(
            egui::vec2(text_width, 0.0),
            Layout::top_down(Align::Min),
            |ui| {
                ui.set_width(text_width);
                setting_description(ui, p, label, help);
            },
        );
        let (rect, mut response) = ui.allocate_exact_size(egui::vec2(40.0, 20.0), Sense::click());
        if response.clicked() {
            *value = !*value;
            response.mark_changed();
        }
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *value, label)
        });
        let amount = ui.ctx().animate_bool(response.id, *value);
        ui.painter()
            .rect_filled(rect, 10, if *value { p.accent } else { p.panel_alt });
        if !*value {
            ui.painter().rect_stroke(
                rect,
                10,
                Stroke::new(1.0_f32, p.muted),
                egui::StrokeKind::Inside,
            );
        }
        let center = egui::pos2(
            egui::lerp((rect.left() + 10.0)..=(rect.right() - 10.0), amount),
            rect.center().y,
        );
        ui.painter().circle_filled(
            center,
            if response.hovered() { 6.5 } else { 6.0 },
            if *value { p.panel } else { p.muted },
        );
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect.expand(2.0),
                4,
                Stroke::new(1.0_f32, p.accent),
                egui::StrokeKind::Outside,
            );
        }
        let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
        if !help.is_empty() {
            response.on_hover_text(help);
        }
    });
}

fn action_button(ui: &mut egui::Ui, _p: &Palette, title: &str, primary: bool) -> egui::Response {
    let icon = match title {
        "返回" => Icon::Home,
        "保存" => Icon::Check,
        "复制路径" => Icon::Copy,
        _ => Icon::Close,
    };
    theme::button(
        ui,
        title,
        icon,
        if primary {
            ButtonKind::Primary
        } else {
            ButtonKind::Secondary
        },
    )
}

fn badge(ui: &mut egui::Ui, p: &Palette, text: &str, active: bool) {
    egui::Frame::new()
        .fill(if active { p.accent_soft } else { p.panel_alt })
        .corner_radius(4)
        .inner_margin(egui::Margin::symmetric(7, 4))
        .show(ui, |ui| {
            ui.label(
                RichText::new(text)
                    .size(11.0)
                    .color(if active { p.accent } else { p.muted }),
            );
        });
}

struct Service<'a> {
    title: &'a str,
    description: &'a str,
    first_label: &'a str,
    second_label: &'a str,
    console: &'a str,
}

struct ModelService<'a> {
    title: &'a str,
    description: &'a str,
    api_key: &'a mut String,
    base_url: &'a mut String,
    ocr_model: &'a mut String,
    translate_model: Option<&'a mut String>,
    default_base_url: &'a str,
    default_ocr_model: &'a str,
    default_translate_model: &'a str,
}

fn model_service_card(ui: &mut egui::Ui, p: &Palette, mut service: ModelService<'_>) {
    ui.push_id(service.title, |ui| {
        card(ui, p, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(service.title)
                        .size(15.0)
                        .strong()
                        .color(p.text),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let complete = !service.api_key.trim().is_empty();
                    badge(ui, p, if complete { "已填写" } else { "待填写" }, complete);
                });
            });
            ui.label(RichText::new(service.description).size(12.0).color(p.muted));
            secret_field(ui, p, "API Key", service.api_key);
            plain_field(
                ui,
                p,
                "服务地址（Base URL）",
                service.base_url,
                service.default_base_url,
            );
            if service.translate_model.is_some() && ui.available_width() >= 620.0 {
                ui.columns(2, |columns| {
                    plain_field(
                        &mut columns[0],
                        p,
                        "识别模型（OCR）",
                        service.ocr_model,
                        service.default_ocr_model,
                    );
                    if let Some(model) = service.translate_model.as_mut() {
                        plain_field(
                            &mut columns[1],
                            p,
                            "翻译模型",
                            model,
                            service.default_translate_model,
                        );
                    }
                });
            } else {
                plain_field(
                    ui,
                    p,
                    "识别模型（OCR）",
                    service.ocr_model,
                    service.default_ocr_model,
                );
                if let Some(model) = service.translate_model.as_mut() {
                    plain_field(ui, p, "翻译模型", model, service.default_translate_model);
                }
            }
            ui.label(
                RichText::new("地址与模型留空时使用默认值。识别模型需支持图片输入。")
                    .size(12.0)
                    .color(p.muted),
            );
        });
    });
}

fn plain_field(ui: &mut egui::Ui, p: &Palette, label: &str, value: &mut String, hint: &str) {
    ui.label(RichText::new(label).size(12.0).color(p.text));
    ui.add(
        egui::TextEdit::singleline(value)
            .id_salt(label)
            .font(FontId::proportional(13.0))
            .desired_width(f32::INFINITY)
            .margin(egui::Margin::symmetric(10, 8))
            .hint_text(hint),
    );
}

fn credential_card(
    ui: &mut egui::Ui,
    p: &Palette,
    service: Service<'_>,
    first: &mut String,
    second: &mut String,
) {
    ui.push_id(service.title, |ui| {
        card(ui, p, |ui| {
            let complete = !first.trim().is_empty() && !second.trim().is_empty();
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(service.title)
                        .size(15.0)
                        .strong()
                        .color(p.text),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    badge(ui, p, if complete { "已填写" } else { "待填写" }, complete);
                });
            });
            ui.label(RichText::new(service.description).size(12.0).color(p.muted));
            ui.add_space(2.0);
            secret_field(ui, p, service.first_label, first);
            secret_field(ui, p, service.second_label, second);
            ui.add_space(2.0);
            ui.hyperlink_to(RichText::new("获取密钥").size(12.0), service.console);
        });
    });
}

fn secret_field(ui: &mut egui::Ui, p: &Palette, label: &str, value: &mut String) {
    secret_field_with_hint(ui, p, label, value, &format!("粘贴 {label}"));
}

fn proxy_username_field(ui: &mut egui::Ui, p: &Palette, value: &mut String) {
    ui.horizontal(|ui| {
        ui.set_min_height(20.0);
        ui.label(RichText::new("用户名（可选）").size(12.0).color(p.text));
    });
    ui.add(
        egui::TextEdit::singleline(value)
            .id_salt("settings_proxy_username")
            .font(FontId::proportional(13.0))
            .desired_width(f32::INFINITY)
            .margin(egui::Margin::symmetric(10, 8))
            .hint_text("无需认证时留空"),
    );
}

fn secret_field_with_hint(
    ui: &mut egui::Ui,
    p: &Palette,
    label: &str,
    value: &mut String,
    hint: &str,
) {
    let id = ui.make_persistent_id(("show_credential", label));
    let mut visible = ui
        .ctx()
        .data_mut(|data| data.get_temp::<bool>(id).unwrap_or(false));
    ui.horizontal(|ui| {
        ui.spacing_mut().button_padding = egui::vec2(4.0, 2.0);
        ui.spacing_mut().interact_size.y = 20.0;
        ui.label(RichText::new(label).size(12.0).color(p.text));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add(
                    egui::Button::new(
                        RichText::new(if visible { "隐藏" } else { "显示" })
                            .size(11.0)
                            .color(p.muted),
                    )
                    .frame(false),
                )
                .clicked()
            {
                visible = !visible;
                ui.ctx().data_mut(|data| data.insert_temp(id, visible));
            }
        });
    });
    ui.add(
        egui::TextEdit::singleline(value)
            .id_salt(label)
            .password(!visible)
            .font(FontId::proportional(13.0))
            .desired_width(f32::INFINITY)
            .margin(egui::Margin::symmetric(10, 8))
            .hint_text(hint),
    );
}

fn baidu_ocr_card(ui: &mut egui::Ui, p: &Palette, first: &mut String, second: &mut String) {
    credential_card(
        ui,
        p,
        Service {
            title: "百度 OCR",
            description: "文字识别 / 表格 / 公式",
            first_label: "API Key",
            second_label: "Secret Key",
            console: "https://console.bce.baidu.com/ai/#/ai/ocr/app/list",
        },
        first,
        second,
    );
}

fn tencent_ocr_card(ui: &mut egui::Ui, p: &Palette, first: &mut String, second: &mut String) {
    credential_card(
        ui,
        p,
        Service {
            title: "腾讯 OCR",
            description: "文字识别 / 表格 / 公式",
            first_label: "SecretId",
            second_label: "SecretKey",
            console: "https://console.cloud.tencent.com/cam/capi",
        },
        first,
        second,
    );
}

fn baidu_translation_card(ui: &mut egui::Ui, p: &Palette, first: &mut String, second: &mut String) {
    credential_card(
        ui,
        p,
        Service {
            title: "百度翻译",
            description: "通用文本翻译",
            first_label: "APP ID",
            second_label: "APP Key",
            console: "https://fanyi-api.baidu.com/manage/developer",
        },
        first,
        second,
    );
}

fn tencent_translation_card(
    ui: &mut egui::Ui,
    p: &Palette,
    first: &mut String,
    second: &mut String,
) {
    credential_card(
        ui,
        p,
        Service {
            title: "腾讯翻译",
            description: "通用文本翻译",
            first_label: "SecretId",
            second_label: "SecretKey",
            console: "https://console.cloud.tencent.com/cam/capi",
        },
        first,
        second,
    );
}
