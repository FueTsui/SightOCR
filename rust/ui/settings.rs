use super::{
    hotkey_input::{self, Target},
    motion,
    theme::{self, ButtonKind, Icon, Palette},
    App, Event,
};
use eframe::egui::{self, Align, FontId, Layout, RichText, Sense, Stroke};
use sightocr::config::{
    generic_endpoint, generic_url_is_loopback, Config, ModelProfile, ModelProtocol, ProxyMode,
    MISTRAL_BASE_URL, MISTRAL_OCR_MODEL, MODEL_SELECTION_PREFIX, NVIDIA_BASE_URL, NVIDIA_OCR_MODEL,
    NVIDIA_TRANSLATE_MODEL, OPENAI_BASE_URL, OPENAI_MODEL,
};
use std::time::Instant;

pub(super) const HOTKEYS_TAB: usize = 4;

/// Credential services with fixed vendor APIs. Everything else is a model interface.
const BUILTIN_SERVICES: &[(&str, &str)] = &[
    ("baidu_ocr", "百度 OCR"),
    ("tencent_ocr", "腾讯 OCR"),
    ("baidu_translate", "百度翻译"),
    ("tencent_translate", "腾讯翻译"),
];
const SERVICE_SELECTION: &str = "settings_service_provider";
const NEW_INTERFACE_NAME: &str = "新模型接口";

/// Quick-fill values for a model interface. The name is a suggestion the user can
/// change; keys are never filled.
struct Preset {
    name: &'static str,
    protocol: ModelProtocol,
    base: &'static str,
    ocr: &'static str,
    translate: &'static str,
}

const MODEL_PRESETS: &[Preset] = &[
    Preset {
        name: "OpenAI",
        protocol: ModelProtocol::OpenaiChat,
        base: OPENAI_BASE_URL,
        ocr: OPENAI_MODEL,
        translate: OPENAI_MODEL,
    },
    Preset {
        name: "Azure OpenAI",
        protocol: ModelProtocol::OpenaiChat,
        base: "",
        ocr: "",
        translate: "",
    },
    Preset {
        name: "Anthropic Claude",
        protocol: ModelProtocol::Anthropic,
        base: "https://api.anthropic.com/v1",
        ocr: "claude-opus-5-5",
        translate: "claude-opus-5-5",
    },
    Preset {
        name: "Google Gemini",
        protocol: ModelProtocol::Gemini,
        base: "https://generativelanguage.googleapis.com/v1beta",
        ocr: "",
        translate: "",
    },
    Preset {
        name: "Mistral OCR",
        protocol: ModelProtocol::MistralOcr,
        base: MISTRAL_BASE_URL,
        ocr: MISTRAL_OCR_MODEL,
        translate: "",
    },
    Preset {
        name: "NVIDIA",
        protocol: ModelProtocol::OpenaiChat,
        base: NVIDIA_BASE_URL,
        ocr: NVIDIA_OCR_MODEL,
        translate: NVIDIA_TRANSLATE_MODEL,
    },
    Preset {
        name: "腾讯混元",
        protocol: ModelProtocol::OpenaiChat,
        base: "https://tokenhub.tencentmaas.com/v1",
        ocr: "hy-vision-2.0-instruct",
        translate: "hy-mt2-pro",
    },
    Preset {
        name: "DeepSeek",
        protocol: ModelProtocol::OpenaiChat,
        base: "https://api.deepseek.com/v1",
        ocr: "",
        translate: "",
    },
    Preset {
        name: "通义千问",
        protocol: ModelProtocol::OpenaiChat,
        base: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        ocr: "",
        translate: "",
    },
    Preset {
        name: "豆包",
        protocol: ModelProtocol::OpenaiChat,
        base: "https://ark.cn-beijing.volces.com/api/v3",
        ocr: "",
        translate: "",
    },
    Preset {
        name: "智谱 GLM",
        protocol: ModelProtocol::OpenaiChat,
        base: "https://open.bigmodel.cn/api/paas/v4",
        ocr: "",
        translate: "",
    },
    Preset {
        name: "Moonshot Kimi",
        protocol: ModelProtocol::OpenaiChat,
        base: "https://api.moonshot.cn/v1",
        ocr: "",
        translate: "",
    },
    Preset {
        name: "硅基流动",
        protocol: ModelProtocol::OpenaiChat,
        base: "https://api.siliconflow.cn/v1",
        ocr: "",
        translate: "",
    },
    Preset {
        name: "OpenRouter",
        protocol: ModelProtocol::OpenaiChat,
        base: "https://openrouter.ai/api/v1",
        ocr: "",
        translate: "",
    },
    Preset {
        name: "Ollama（本机）",
        protocol: ModelProtocol::OpenaiChat,
        base: "http://localhost:11434/v1",
        ocr: "",
        translate: "",
    },
    Preset {
        name: "LM Studio（本机）",
        protocol: ModelProtocol::OpenaiChat,
        base: "http://localhost:1234/v1",
        ocr: "",
        translate: "",
    },
];

/// A small table image used to test interfaces that can only recognize.
const TEST_IMAGE: &[u8] = include_bytes!("../../tests/fixtures/basic.png");

/// Connection test for a model interface, run off the UI thread.
#[derive(Default)]
pub(super) enum ServiceTest {
    #[default]
    Idle,
    Running {
        id: u64,
        profile: String,
        started: Instant,
    },
    Done {
        profile: String,
        ok: bool,
        message: String,
    },
}

/// A short unique id for a new interface, derived from its name where possible.
fn new_profile_id(existing: &[ModelProfile], name: &str) -> String {
    let slug: String = name
        .chars()
        .filter_map(|c| {
            if c.is_ascii_alphanumeric() {
                Some(c.to_ascii_lowercase())
            } else if c == ' ' || c == '-' {
                Some('-')
            } else {
                None
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let stem: String = if slug.is_empty() {
        "model".into()
    } else {
        slug
    };
    let stem: String = stem.chars().take(24).collect();
    (0..)
        .map(|n| {
            if n == 0 {
                stem.clone()
            } else {
                format!("{stem}-{n}")
            }
        })
        .find(|id| existing.iter().all(|profile| &profile.id != id))
        .unwrap_or(stem)
}

impl App {
    /// Whether the settings draft differs from the saved configuration. Selections
    /// made in the workbench while settings are open are not settings edits.
    pub(super) fn settings_dirty(&self) -> bool {
        let mut draft = self.draft.clone();
        draft.last_ocr_selection = self.config.last_ocr_selection.clone();
        draft.last_translate_selection = self.config.last_translate_selection.clone();
        draft.source_lang = self.config.source_lang.clone();
        draft.target_lang = self.config.target_lang.clone();
        draft.skipped_update_version = self.config.skipped_update_version.clone();
        draft.last_update_check = self.config.last_update_check;
        draft.last_run_version = self.config.last_run_version.clone();
        draft.run_as_admin != self.config.run_as_admin
            || serde_json::to_value(&draft).ok() != serde_json::to_value(&self.config).ok()
    }

    fn close_settings(&mut self, ctx: &egui::Context) {
        hotkey_input::clear(ctx);
        // A dropdown left open on the settings page would otherwise outlive its
        // widget and keep the native result editors hidden.
        ctx.memory_mut(|memory| memory.close_popup());
        self.settings_open = false;
        self.settings_error.clear();
        self.service_test = ServiceTest::Idle;
    }

    fn save_settings(&mut self, ctx: &egui::Context) {
        match self.save_draft() {
            Ok(()) => {
                let preference = self
                    .config
                    .extra
                    .get("appearance")
                    .and_then(|value| value.as_str())
                    .unwrap_or("system");
                ctx.set_theme(match preference {
                    "light" => egui::ThemePreference::Light,
                    "dark" => egui::ThemePreference::Dark,
                    _ => egui::ThemePreference::System,
                });
                self.close_settings(ctx);
            }
            Err(error) => self.settings_error = format!("{error:#}"),
        }
    }

    /// Test one model interface from the draft: translate a short English sentence when
    /// it can translate, otherwise recognize a bundled table image.
    fn start_service_test(&mut self, ctx: &egui::Context, profile_id: &str) {
        if matches!(self.service_test, ServiceTest::Running { .. }) {
            return;
        }
        let profile_id = profile_id.to_owned();
        let mut config = self.draft.clone();
        let fail = |message: String| ServiceTest::Done {
            profile: profile_id.clone(),
            ok: false,
            message,
        };
        if let Err(error) = config.normalize() {
            self.service_test = fail(format!("{error:#}"));
            return;
        }
        let Some(profile) = config.profile(&profile_id).cloned() else {
            self.service_test = fail("请先填写服务地址和模型".into());
            return;
        };
        let translate = profile.can_translate();
        let selection = format!("{MODEL_SELECTION_PREFIX}{profile_id}");
        config.last_translate_selection = selection.clone();
        config.source_lang = "en".into();
        config.target_lang = "zh-Hans".into();
        self.service_test_id = self.service_test_id.wrapping_add(1);
        let id = self.service_test_id;
        self.service_test = ServiceTest::Running {
            id,
            profile: profile_id.clone(),
            started: Instant::now(),
        };
        let sender = self.sender.clone();
        let ctx = ctx.clone();
        let hwnd = self.hwnd;
        let spawned = std::thread::Builder::new()
            .name("sightocr-service-test".into())
            .spawn(move || {
                let started = Instant::now();
                let result = sightocr::services::Services::new()
                    .and_then(|mut services| {
                        if translate {
                            services
                                .translate("Hello, world.", &config)
                                .map(|text| format!("译文：{text}"))
                        } else {
                            services
                                .ocr(TEST_IMAGE, &format!("{selection}_auto"), &config)
                                .map(|text| format!("识别：{}", text.replace(['\n', '\t'], " ")))
                        }
                    })
                    .map_err(|error| format!("{error:#}"));
                let _ = sender.send(Event::ServiceTest {
                    id,
                    result,
                    seconds: started.elapsed().as_secs_f32(),
                });
                super::wake_ui(&ctx, hwnd);
            });
        if spawned.is_err() {
            self.service_test = fail("无法启动测试线程".into());
        }
    }

    pub(super) fn finish_service_test(
        &mut self,
        id: u64,
        result: Result<String, String>,
        seconds: f32,
    ) {
        let ServiceTest::Running {
            id: running,
            profile,
            ..
        } = &self.service_test
        else {
            return;
        };
        if *running != id {
            return;
        }
        self.service_test = ServiceTest::Done {
            profile: profile.clone(),
            ok: result.is_ok(),
            message: match result {
                Ok(text) => {
                    let preview: String = text.chars().take(48).collect();
                    let ellipsis = if text.chars().count() > 48 { "…" } else { "" };
                    format!("连接成功 · {seconds:.1} 秒 · {preview}{ellipsis}")
                }
                Err(error) => error,
            },
        };
    }

    fn service_test_card(&mut self, ui: &mut egui::Ui, p: &Palette, profile: &ModelProfile) {
        let translate = profile.can_translate();
        card(ui, p, |ui| {
            setting_description(
                ui,
                p,
                "连接测试",
                if translate {
                    "用当前填写的内容翻译一句英文，验证地址、密钥和翻译模型。会产生一次很小的调用。"
                } else {
                    "用当前填写的内容识别一张示例表格图片，验证地址、密钥和识别模型。会产生一次很小的调用。"
                },
            );
            let running = matches!(
                &self.service_test,
                ServiceTest::Running { profile: current, .. } if current == &profile.id
            );
            ui.horizontal(|ui| {
                ui.add_enabled_ui(
                    !matches!(self.service_test, ServiceTest::Running { .. }),
                    |ui| {
                        if theme::button(
                            ui,
                            if translate {
                                "测试翻译"
                            } else {
                                "测试识别"
                            },
                            Icon::ArrowRight,
                            ButtonKind::Secondary,
                        )
                        .clicked()
                        {
                            self.start_service_test(ui.ctx(), &profile.id);
                        }
                    },
                );
                if running {
                    ui.add(egui::Spinner::new().size(14.0).color(p.accent));
                    if let ServiceTest::Running { started, .. } = &self.service_test {
                        ui.label(
                            RichText::new(format!(
                                "正在请求… {:.0} 秒",
                                started.elapsed().as_secs_f32()
                            ))
                            .size(12.0)
                            .color(p.muted),
                        );
                    }
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(250));
                }
            });
            if let ServiceTest::Done {
                profile: current,
                ok,
                message,
            } = &self.service_test
            {
                if current == &profile.id {
                    let t = motion::appear(
                        ui.ctx(),
                        egui::Id::new("service_test_result"),
                        motion::key(message),
                    );
                    let color = if *ok { p.success } else { p.danger };
                    ui.scope(|ui| {
                        ui.multiply_opacity(t);
                        egui::Frame::new()
                            .fill(color.gamma_multiply(0.08))
                            .corner_radius(4)
                            .inner_margin(egui::Margin::symmetric(10, 8))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal_wrapped(|ui| {
                                    let (rect, _) = ui.allocate_exact_size(
                                        egui::vec2(16.0, 16.0),
                                        Sense::hover(),
                                    );
                                    theme::draw_icon(
                                        ui,
                                        if *ok { Icon::Check } else { Icon::Info },
                                        rect,
                                        color,
                                    );
                                    ui.label(RichText::new(message).size(12.0).color(color));
                                });
                            });
                    });
                }
            }
        });
    }
}

impl App {
    fn service_settings(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let selection_id = egui::Id::new(SERVICE_SELECTION);
        let mut selected = ui
            .ctx()
            .data_mut(|data| data.get_temp::<String>(selection_id))
            .unwrap_or_else(|| BUILTIN_SERVICES[0].0.into());
        // A removed interface falls back to the first service.
        if let Some(id) = selected.strip_prefix("model:") {
            if self.draft.profile(id).is_none() {
                selected = BUILTIN_SERVICES[0].0.into();
            }
        }
        let builtin_ready = builtin_configured(&self.draft);
        let ready_count = builtin_ready.iter().filter(|ready| **ready).count()
            + self
                .draft
                .model_profiles
                .iter()
                .filter(|profile| profile_ready(profile))
                .count();
        let mut add_interface = false;
        card(ui, p, |ui| {
            let control_width = form_selector_width(ui);
            ui.horizontal(|ui| {
                setting_description(
                    ui,
                    p,
                    "配置服务",
                    &format!(
                        "已配置 {ready_count} 项 · 模型接口 {} 个",
                        self.draft.model_profiles.len()
                    ),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let current =
                        theme::elide(ui, &service_title(&self.draft, &selected), control_width - 40.0);
                    theme::dropdown(
                        ui,
                        "settings_service_selector",
                        false,
                        |combo| combo.width(control_width).selected_text(current),
                        |ui| {
                            for (index, (key, title)) in BUILTIN_SERVICES.iter().enumerate() {
                                let label = ready_label(title, builtin_ready[index]);
                                theme::menu_option(ui, &mut selected, (*key).into(), label);
                            }
                            ui.separator();
                            theme::menu_heading(ui, "模型接口");
                            for profile in &self.draft.model_profiles {
                                let label = ready_label(&profile_title(profile), profile_ready(profile));
                                theme::menu_option(
                                    ui,
                                    &mut selected,
                                    format!("model:{}", profile.id),
                                    label,
                                );
                            }
                            if theme::menu_item(ui, false, "＋ 接口")
                                .on_hover_text("可接入 OpenAI 兼容、Responses、Anthropic、Gemini 或 Mistral OCR 协议的服务")
                                .changed()
                            {
                                add_interface = true;
                            }
                        },
                    );
                });
            });
        });
        if add_interface {
            let id = new_profile_id(&self.draft.model_profiles, "model");
            self.draft.model_profiles.push(ModelProfile {
                id: id.clone(),
                name: NEW_INTERFACE_NAME.into(),
                ..ModelProfile::default()
            });
            selected = format!("model:{id}");
        }
        ui.add_space(4.0);

        match selected.as_str() {
            "baidu_ocr" => {
                baidu_ocr_card(ui, p, &mut self.draft.api_key, &mut self.draft.secret_key)
            }
            "tencent_ocr" => tencent_ocr_card(
                ui,
                p,
                &mut self.draft.tencent_secret_id,
                &mut self.draft.tencent_secret_key,
            ),
            "baidu_translate" => baidu_translation_card(
                ui,
                p,
                &mut self.draft.baidu_trans_appid,
                &mut self.draft.baidu_trans_appkey,
            ),
            "tencent_translate" => tencent_translation_card(
                ui,
                p,
                &mut self.draft.tencent_trans_secret_id,
                &mut self.draft.tencent_trans_secret_key,
            ),
            key => {
                let id = key.strip_prefix("model:").unwrap_or_default().to_owned();
                let Some(index) = self.draft.model_profiles.iter().position(|m| m.id == id) else {
                    return;
                };
                let removed = model_profile_card(ui, p, &mut self.draft.model_profiles[index]);
                if removed {
                    self.draft.model_profiles.remove(index);
                    selected = BUILTIN_SERVICES[0].0.into();
                    if matches!(&self.service_test, ServiceTest::Done { profile, .. } if profile == &id)
                    {
                        self.service_test = ServiceTest::Idle;
                    }
                } else {
                    let profile = self.draft.model_profiles[index].clone();
                    if profile.can_recognize() || profile.can_translate() {
                        ui.add_space(4.0);
                        self.service_test_card(ui, p, &profile);
                    }
                }
            }
        }
        ui.ctx()
            .data_mut(|data| data.insert_temp(selection_id, selected));
    }
}

fn ready_label(title: &str, ready: bool) -> String {
    if ready {
        format!("{title}    ✓ 已配置")
    } else {
        title.to_owned()
    }
}

fn service_title(config: &Config, key: &str) -> String {
    if let Some(id) = key.strip_prefix("model:") {
        return config
            .profile(id)
            .map(profile_title)
            .unwrap_or_else(|| "模型接口".into());
    }
    BUILTIN_SERVICES
        .iter()
        .find(|(builtin, _)| *builtin == key)
        .map_or_else(
            || BUILTIN_SERVICES[0].1.into(),
            |(_, title)| (*title).into(),
        )
}

fn profile_title(profile: &ModelProfile) -> String {
    let name = profile.name.trim();
    if name.is_empty() {
        NEW_INTERFACE_NAME.into()
    } else {
        name.into()
    }
}

fn builtin_configured(c: &Config) -> [bool; 4] {
    let filled = |a: &str, b: &str| !a.trim().is_empty() && !b.trim().is_empty();
    [
        filled(&c.api_key, &c.secret_key),
        filled(&c.tencent_secret_id, &c.tencent_secret_key),
        filled(&c.baidu_trans_appid, &c.baidu_trans_appkey),
        filled(&c.tencent_trans_secret_id, &c.tencent_trans_secret_key),
    ]
}

fn profile_ready(profile: &ModelProfile) -> bool {
    !profile.base_url.trim().is_empty()
        && (generic_url_is_loopback(&profile.base_url) || !profile.api_key.trim().is_empty())
        && (profile.can_recognize() || profile.can_translate())
}

/// Edit one model interface. Returns true when the user removes it.
fn model_profile_card(ui: &mut egui::Ui, p: &Palette, profile: &mut ModelProfile) -> bool {
    let mut removed = false;
    ui.push_id(("model_profile", profile.id.clone()), |ui| {
        card(ui, p, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(profile_title(profile))
                        .size(15.0)
                        .strong()
                        .color(p.text),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let complete = profile_ready(profile);
                    badge(ui, p, if complete { "已填写" } else { "待填写" }, complete);
                    let uses: Vec<&str> = [
                        profile.can_recognize().then_some("识别"),
                        profile.can_translate().then_some("翻译"),
                    ]
                    .into_iter()
                    .flatten()
                    .collect();
                    if !uses.is_empty() {
                        badge(ui, p, &uses.join(" · "), false);
                    }
                });
            });
            // Vendor preset and wire protocol side by side: both describe "what this is".
            ui.columns(2, |columns| {
                columns[0].label(RichText::new("快速填入").size(12.0).color(p.text));
                let preset_width = columns[0].available_width();
                let current_preset = MODEL_PRESETS.iter().find(|preset| {
                    preset.protocol == profile.protocol
                        && !preset.base.is_empty()
                        && preset.base == profile.base_url
                });
                let preset_label =
                    current_preset.map_or("选择服务商…", |preset| preset.name);
                let current_name = current_preset.map(|preset| preset.name);
                theme::dropdown(
                    &mut columns[0],
                    "model_preset",
                    false,
                    |combo| combo.width(preset_width).selected_text(preset_label),
                    |ui| {
                        for preset in MODEL_PRESETS {
                            if theme::menu_item(ui, current_name == Some(preset.name), preset.name)
                                .changed()
                            {
                                apply_preset(profile, preset);
                            }
                        }
                    },
                )
                .response
                .on_hover_text("填入协议、服务地址、默认模型和建议名称，不会填入或发送密钥");
                columns[1].label(RichText::new("接口协议").size(12.0).color(p.text));
                let protocol_width = columns[1].available_width();
                let protocol_label = profile.protocol.label().to_owned();
                theme::dropdown(
                    &mut columns[1],
                    "model_protocol",
                    false,
                    |combo| combo.width(protocol_width).selected_text(protocol_label),
                    |ui| {
                        for protocol in ModelProtocol::ALL {
                            theme::menu_option(
                                ui,
                                &mut profile.protocol,
                                protocol,
                                protocol.label(),
                            );
                        }
                    },
                );
            });
            plain_field(
                ui,
                p,
                "名称",
                &mut profile.name,
                "显示在工具栏和译文服务菜单中，例如「公司网关」",
            );
            let local = generic_url_is_loopback(&profile.base_url);
            secret_field_with_hint(
                ui,
                p,
                "API Key",
                &mut profile.api_key,
                if local {
                    "本机服务通常无需密钥，可留空"
                } else {
                    "粘贴 API Key"
                },
            );
            plain_field(
                ui,
                p,
                "服务地址（Base URL）",
                &mut profile.base_url,
                "例如 https://api.openai.com/v1",
            );
            endpoint_preview(ui, p, profile);
            let hint = match profile.protocol {
                ModelProtocol::OpenaiChat | ModelProtocol::OpenaiResponses => "服务商的模型 ID",
                ModelProtocol::Anthropic => "例如 claude-opus-5-5",
                ModelProtocol::Gemini => "Gemini 模型 ID，可带 models/ 前缀",
                ModelProtocol::MistralOcr => "例如 mistral-ocr-latest",
            };
            let translates = profile.protocol.translates();
            if translates && ui.available_width() >= 620.0 {
                ui.columns(2, |columns| {
                    plain_field(
                        &mut columns[0],
                        p,
                        "识别模型（OCR）",
                        &mut profile.ocr_model,
                        hint,
                    );
                    plain_field(
                        &mut columns[1],
                        p,
                        "翻译模型",
                        &mut profile.translate_model,
                        hint,
                    );
                });
            } else {
                plain_field(ui, p, "识别模型（OCR）", &mut profile.ocr_model, hint);
                if translates {
                    plain_field(ui, p, "翻译模型", &mut profile.translate_model, hint);
                }
            }
            if !translates {
                ui.label(
                    RichText::new("Mistral OCR 协议只用于截图识别，支持文字、表格与公式。")
                        .size(12.0)
                        .color(p.muted),
                );
            }
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let delete_id = ui.make_persistent_id("confirm_delete");
                    let confirming = ui
                        .ctx()
                        .data(|data| data.get_temp::<bool>(delete_id))
                        .unwrap_or(false);
                    if confirming {
                        if theme::button(ui, "确认删除", Icon::Close, ButtonKind::Secondary)
                            .on_hover_text("保存设置后生效；取消设置可撤销")
                            .clicked()
                        {
                            removed = true;
                            ui.ctx().data_mut(|data| data.remove::<bool>(delete_id));
                        }
                        if ui.button("保留").clicked() {
                            ui.ctx().data_mut(|data| data.remove::<bool>(delete_id));
                        }
                    } else if theme::button(ui, "删除此接口", Icon::Close, ButtonKind::Ghost)
                        .clicked()
                    {
                        ui.ctx().data_mut(|data| data.insert_temp(delete_id, true));
                    }
                });
            });
        });
    });
    removed
}

fn apply_preset(profile: &mut ModelProfile, preset: &Preset) {
    // Keep a name the user typed; replace an empty, default or earlier preset name.
    let name = profile.name.trim();
    if name.is_empty()
        || name == NEW_INTERFACE_NAME
        || MODEL_PRESETS.iter().any(|known| known.name == name)
    {
        profile.name = preset.name.into();
    }
    profile.protocol = preset.protocol;
    if !preset.base.is_empty() {
        profile.base_url = preset.base.into();
    }
    profile.ocr_model = preset.ocr.into();
    profile.translate_model = preset.translate.into();
}

/// Live, as-you-type validation that also shows exactly which URL will be called.
fn endpoint_preview(ui: &mut egui::Ui, p: &Palette, profile: &ModelProfile) {
    let base = profile.base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return;
    }
    let model = [&profile.translate_model, &profile.ocr_model]
        .into_iter()
        .map(|model| model.trim())
        .find(|model| !model.is_empty())
        .unwrap_or("{model}");
    let path = profile.protocol.path(model);
    let (text, color) = match generic_endpoint(base, &path) {
        Ok(url) => (format!("请求地址：{url}"), p.subtle),
        Err(error) => (format!("{error:#}"), p.danger),
    };
    ui.add(egui::Label::new(RichText::new(text).size(11.0).color(color)).truncate());
}

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

        // Esc cancels and Ctrl+S saves, unless a menu, text field or hotkey recorder
        // is using the keyboard.
        let ctx = ui.ctx().clone();
        let keyboard_free = !hotkey_input::is_recording(&ctx)
            && !ctx.memory(|memory| memory.any_popup_open() || memory.focused().is_some());
        if keyboard_free && ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.close_settings(&ctx);
            return;
        }
        if !hotkey_input::is_recording(&ctx)
            && ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::S))
        {
            self.save_settings(&ctx);
            if !self.settings_open {
                return;
            }
        }

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            let mut selected_rect = None;
            for (index, title) in [
                (0, "常规"),
                (1, "接口"),
                (HOTKEYS_TAB, "快捷键"),
                (3, "代理"),
                (2, "关于"),
            ] {
                let response = pivot_tab(ui, p, title, self.settings_tab == index);
                if self.settings_tab == index {
                    selected_rect = Some(response.rect);
                }
                if response.clicked() {
                    if index != HOTKEYS_TAB {
                        hotkey_input::clear(ui.ctx());
                    }
                    self.settings_tab = index;
                }
            }
            if let Some(rect) = selected_rect {
                theme::sliding_underline(ui, "settings_pivot", rect, 24.0, p.accent);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if action_button(ui, p, "返回", false)
                    .on_hover_text("Esc")
                    .clicked()
                {
                    self.close_settings(ui.ctx());
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
                let shown = motion::appear(
                    ui.ctx(),
                    egui::Id::new("settings_tab_appear"),
                    self.settings_tab as u64,
                );
                ui.multiply_opacity(shown);
                ui.add_space((1.0 - shown) * 10.0);
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
            let dirty = self.settings_dirty();
            ui.horizontal(|ui| {
                let shown = motion::toggle(
                    ui.ctx(),
                    egui::Id::new("settings_dirty"),
                    dirty,
                    motion::APPEAR,
                );
                if shown > 0.0 {
                    let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), Sense::hover());
                    ui.painter()
                        .circle_filled(dot.center(), 3.5 * shown, p.accent);
                    ui.label(
                        RichText::new("有未保存的更改")
                            .size(12.0)
                            .color(p.muted.gamma_multiply(shown)),
                    );
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if action_button(ui, p, "保存", true)
                        .on_hover_text("Ctrl+S")
                        .clicked()
                    {
                        self.save_settings(ui.ctx());
                    }
                    if action_button(ui, p, "取消", false)
                        .on_hover_text(if dirty {
                            "放弃更改（Esc）"
                        } else {
                            "Esc"
                        })
                        .clicked()
                    {
                        self.close_settings(ui.ctx());
                    }
                });
            });
        });
    }

    fn general_settings(&mut self, ui: &mut egui::Ui, p: &Palette) {
        appearance_card(ui, p, &mut self.draft);
        ui.add_space(6.0);
        output_card(ui, p, &mut self.draft.layout_analysis);
        ui.add_space(6.0);
        startup_card(
            ui,
            p,
            &mut self.draft.run_as_admin,
            &mut self.draft.autostart,
            &mut self.draft.hide_tray_icon,
        );
        ui.add_space(6.0);
        card(ui, p, |ui| {
            toggle_row(ui, p, &mut self.draft.check_updates, "自动检查更新", "");
        });
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

    fn proxy_settings(&mut self, ui: &mut egui::Ui, p: &Palette) {
        card(ui, p, |ui| {
            let control_width = form_selector_width(ui);
            ui.horizontal(|ui| {
                ui.label(RichText::new("代理模式").size(14.0).color(p.text));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let selected = match self.draft.proxy.mode {
                        ProxyMode::System => "跟随系统",
                        ProxyMode::Direct => "不使用代理",
                        ProxyMode::Manual => "手动代理",
                    };
                    theme::dropdown(
                        ui,
                        "settings_proxy_mode",
                        false,
                        |combo| combo.width(control_width).selected_text(selected),
                        |ui| {
                            for (mode, label) in [
                                (ProxyMode::System, "跟随系统"),
                                (ProxyMode::Direct, "不使用代理"),
                                (ProxyMode::Manual, "手动代理"),
                            ] {
                                theme::menu_option(ui, &mut self.draft.proxy.mode, mode, label);
                            }
                        },
                    );
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
                ui.label(RichText::new("·").color(p.subtle));
                ui.label(
                    RichText::new("© 2026 FueTsui. All rights reserved.")
                        .size(12.0)
                        .color(p.muted),
                );
            });
        });
        ui.add_space(4.0);
        self.update_card(ui, p);
        ui.add_space(4.0);
        card(ui, p, |ui| {
            card_title(ui, p, "配置文件", "");
            egui::CollapsingHeader::new(RichText::new("文件路径").size(13.0).color(p.muted))
                .id_salt("configuration_path")
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(self.path.display().to_string())
                            .size(12.0)
                            .color(p.text),
                    );
                    ui.horizontal(|ui| {
                        if action_button(ui, p, "打开文件位置", false).clicked() {
                            reveal_in_explorer(&self.path);
                        }
                        if action_button(ui, p, "复制路径", false).clicked() {
                            self.copy_output(ui.ctx(), self.path.display().to_string());
                        }
                    });
                });
        });
    }
}

impl App {
    /// Software update: what is happening, and one clear next step.
    fn update_card(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let ctx = ui.ctx().clone();
        card(ui, p, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("软件更新").size(15.0).strong().color(p.text));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    match self.update.new_version() {
                        Some(version) => badge(ui, p, &format!("新版本 v{version}"), true),
                        None => badge(
                            ui,
                            p,
                            &format!("当前 v{}", env!("CARGO_PKG_VERSION")),
                            false,
                        ),
                    }
                });
            });
            let message_color = if self.update.failed {
                p.danger
            } else {
                p.muted
            };
            if self.update.downloading() {
                ui.label(
                    RichText::new(&self.update.message)
                        .size(13.0)
                        .color(p.muted),
                );
                if let Some(fraction) = self.update.fraction {
                    ui.add(
                        egui::ProgressBar::new(fraction)
                            .desired_height(6.0)
                            .corner_radius(3.0)
                            .fill(p.accent),
                    );
                }
                if theme::button(ui, "取消下载", Icon::Close, ButtonKind::Secondary).clicked() {
                    self.cancel_update_download();
                }
            } else if let Some(version) = self.update.ready.as_ref().map(|u| u.version().to_owned())
            {
                ui.label(
                    RichText::new(format!("v{version} 已下载并通过校验。"))
                        .size(13.0)
                        .color(p.text),
                );
                ui.horizontal(|ui| {
                    let idle = !self.busy;
                    let response = ui
                        .add_enabled_ui(idle, |ui| {
                            theme::button(
                                ui,
                                "立即重启并安装",
                                Icon::ArrowRight,
                                ButtonKind::Primary,
                            )
                        })
                        .inner;
                    let response = if idle {
                        response.on_hover_text(
                            "关闭 SightOCR 后安装，完成后自动重新打开；设置和密钥会保留",
                        )
                    } else {
                        response.on_disabled_hover_text("当前识别或翻译完成后即可安装")
                    };
                    if response.clicked() {
                        self.install_update_now(&ctx);
                    }
                });
                toggle_row(
                    ui,
                    p,
                    &mut self.update.install_on_exit,
                    "退出 SightOCR 时自动安装",
                    "从托盘退出时安装，安装后不会自动重新打开",
                );
            } else if self.update.busy {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(14.0).color(p.accent));
                    ui.label(RichText::new("正在检查更新…").size(13.0).color(p.muted));
                });
            } else if let Some(info) = self.update.available.clone() {
                ui.label(
                    RichText::new(format!(
                        "发现新版本 v{}（约 {:.0} MB）",
                        info.version(),
                        info.size() as f64 / 1_048_576.0
                    ))
                    .size(13.0)
                    .color(p.text),
                );
                if !info.notes().is_empty() {
                    egui::Frame::new()
                        .fill(p.panel_alt)
                        .corner_radius(4)
                        .inner_margin(egui::Margin::symmetric(10, 8))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            egui::ScrollArea::vertical()
                                .id_salt("update_release_notes")
                                .max_height(150.0)
                                .show(ui, |ui| {
                                    ui.label(RichText::new(info.notes()).size(12.0).color(p.text));
                                });
                        });
                }
                ui.horizontal(|ui| {
                    if theme::button(ui, "下载并安装", Icon::ArrowRight, ButtonKind::Primary)
                        .on_hover_text("下载并校验后再由你决定何时安装")
                        .clicked()
                    {
                        self.download_update(&ctx);
                    }
                    if theme::button(ui, "跳过此版本", Icon::Close, ButtonKind::Secondary).clicked()
                    {
                        self.skip_update();
                    }
                    if let Some(url) = info.page_url() {
                        ui.hyperlink_to(RichText::new("发布页面").size(12.0), url);
                    }
                });
            } else {
                if !self.update.message.is_empty() {
                    ui.label(
                        RichText::new(&self.update.message)
                            .size(13.0)
                            .color(message_color),
                    );
                }
                ui.horizontal(|ui| {
                    if theme::button(ui, "检查更新", Icon::ArrowRight, ButtonKind::Secondary)
                        .clicked()
                    {
                        self.check_for_updates(&ctx, true);
                    }
                    ui.label(
                        RichText::new(if self.config.check_updates {
                            "每天自动检查一次"
                        } else {
                            "自动检查已关闭（可在「常规」中开启）"
                        })
                        .size(12.0)
                        .color(p.subtle),
                    );
                });
            }
        });
    }
}

fn pivot_tab(ui: &mut egui::Ui, p: &Palette, label: &str, selected: bool) -> egui::Response {
    let font = FontId::proportional(14.0);
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, p.text);
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(galley.size().x + 20.0, 40.0), Sense::click());
    let hover = motion::toggle(
        ui.ctx(),
        response.id.with("hover"),
        response.hovered(),
        motion::FAST,
    );
    let active = motion::toggle(ui.ctx(), response.id.with("active"), selected, motion::FAST);
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            selected,
            label,
        )
    });
    if hover > 0.0 {
        ui.painter().rect_filled(
            rect.shrink2(egui::vec2(0.0, 4.0)),
            4,
            p.panel_alt.gamma_multiply(hover),
        );
    }
    ui.painter().galley(
        egui::pos2(
            rect.center().x - galley.size().x / 2.0,
            rect.center().y - galley.size().y / 2.0 - 1.0,
        ),
        galley,
        motion::mix(p.muted, p.text, active.max(hover * 0.6)),
    );
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            4,
            Stroke::new(1.0_f32, p.accent),
            egui::StrokeKind::Inside,
        );
    }
    response
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
        let control_width = form_selector_width(ui);
        ui.horizontal(|ui| {
            ui.label(RichText::new("应用主题").size(14.0).color(p.text));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                select_choice(
                    ui,
                    "settings_appearance",
                    &mut selected,
                    choices,
                    control_width,
                );
            });
        });
    });
    if selected != initial {
        config.extra.insert("appearance".into(), selected.into());
    }
}

/// Width of a dropdown on a settings row (all settings selectors match).
fn form_selector_width(ui: &egui::Ui) -> f32 {
    180.0_f32.min(ui.available_width() * 0.4)
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
        .map(|(_, title)| (*title).to_owned())
        .unwrap_or_else(|| value.clone());
    theme::dropdown(
        ui,
        id,
        false,
        |combo| combo.width(width).selected_text(selected),
        |ui| {
            for (code, title) in choices {
                theme::menu_option(ui, value, (*code).to_owned(), *title);
            }
        },
    );
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

fn output_card(ui: &mut egui::Ui, p: &Palette, layout_analysis: &mut bool) {
    card(ui, p, |ui| {
        toggle_row(ui, p, layout_analysis, "版面分析", "");
    });
}

fn startup_card(
    ui: &mut egui::Ui,
    p: &Palette,
    run_as_admin: &mut bool,
    autostart: &mut bool,
    hide_tray: &mut bool,
) {
    card(ui, p, |ui| {
        toggle_row(ui, p, run_as_admin, "以管理员运行", "");
        ui.separator();
        toggle_row(ui, p, autostart, "开机自动启动", "");
        ui.separator();
        toggle_row(ui, p, hide_tray, "隐藏托盘图标", "");
    });
}

fn toggle_row(ui: &mut egui::Ui, p: &Palette, value: &mut bool, label: &str, help: &str) {
    ui.horizontal(|ui| {
        let text_width = (ui.available_width() - 40.0 - ui.spacing().item_spacing.x).max(100.0);
        if help.is_empty() {
            ui.label(RichText::new(label).size(14.0).color(p.text));
        } else {
            ui.allocate_ui_with_layout(
                egui::vec2(text_width, 0.0),
                Layout::top_down(Align::Min),
                |ui| {
                    ui.set_width(text_width);
                    setting_description(ui, p, label, help);
                },
            );
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (rect, mut response) =
                ui.allocate_exact_size(egui::vec2(40.0, 20.0), Sense::click());
            if response.clicked() {
                *value = !*value;
                response.mark_changed();
            }
            response.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::Checkbox,
                    ui.is_enabled(),
                    *value,
                    label,
                )
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
            if !help.is_empty() {
                response.on_hover_text(help);
            }
        });
    });
}

/// Show the configuration file in Explorer, selected. Before the first save
/// the file does not exist yet, so its folder is opened instead.
fn reveal_in_explorer(path: &std::path::Path) {
    use std::os::windows::process::CommandExt;
    let mut explorer = std::process::Command::new("explorer.exe");
    if path.is_file() {
        // Explorer parses its own command line: /select,"<path>".
        explorer.raw_arg(format!("/select,\"{}\"", path.display()));
    } else if let Some(folder) = path.parent() {
        let _ = std::fs::create_dir_all(folder);
        explorer.arg(folder);
    } else {
        return;
    }
    let _ = explorer.spawn();
}

fn action_button(ui: &mut egui::Ui, _p: &Palette, title: &str, primary: bool) -> egui::Response {
    let icon = match title {
        "返回" => Icon::Home,
        "保存" => Icon::Check,
        "复制路径" => Icon::Copy,
        "打开文件位置" => Icon::Folder,
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
