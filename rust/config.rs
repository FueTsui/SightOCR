//! Durable configuration and non-destructive migration from the Python version.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

pub const MISTRAL_BASE_URL: &str = "https://api.mistral.ai/v1";
pub const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
pub const NVIDIA_BASE_URL: &str = "https://integrate.api.nvidia.com/v1";
pub const MISTRAL_OCR_MODEL: &str = "mistral-ocr-latest";
pub const OPENAI_MODEL: &str = "gpt-4.1-mini";
pub const NVIDIA_OCR_MODEL: &str = "moonshotai/kimi-k3";
pub const NVIDIA_TRANSLATE_MODEL: &str = "moonshotai/kimi-k3";

/// Prefix of selections that refer to a user-defined model interface:
/// OCR `Model.{id}_{auto|table|formula}`, translation `Model.{id}`.
pub const MODEL_SELECTION_PREFIX: &str = "Model.";
const MAX_MODEL_PROFILES: usize = 32;

/// Wire protocol of a user-defined model interface.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelProtocol {
    /// `POST {base}/chat/completions`: OpenAI and most compatible gateways,
    /// including DeepSeek, Qwen, GLM, Kimi, SiliconFlow, OpenRouter, Ollama and LM Studio.
    #[default]
    OpenaiChat,
    /// `POST {base}/responses`: the OpenAI Responses API.
    OpenaiResponses,
    /// `POST {base}/messages`: the Anthropic Messages API.
    Anthropic,
    /// `POST {base}/models/{model}:generateContent`: the Google Gemini API.
    Gemini,
    /// `POST {base}/ocr`: the Mistral document OCR API (recognition only).
    MistralOcr,
}

impl ModelProtocol {
    pub const ALL: [Self; 5] = [
        Self::OpenaiChat,
        Self::OpenaiResponses,
        Self::Anthropic,
        Self::Gemini,
        Self::MistralOcr,
    ];

    /// Whether the protocol can translate text (Mistral OCR only recognizes images).
    pub fn translates(self) -> bool {
        self != Self::MistralOcr
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::OpenaiChat => "OpenAI Chat Completions 兼容",
            Self::OpenaiResponses => "OpenAI Responses",
            Self::Anthropic => "Anthropic Messages",
            Self::Gemini => "Google Gemini",
            Self::MistralOcr => "Mistral OCR",
        }
    }

    /// Endpoint path relative to the base URL, shown in settings and used for requests.
    pub fn path(self, model: &str) -> String {
        match self {
            Self::OpenaiChat => "chat/completions".into(),
            Self::OpenaiResponses => "responses".into(),
            Self::Anthropic => "messages".into(),
            Self::Gemini => format!("models/{}:generateContent", gemini_model(model)),
            Self::MistralOcr => "ocr".into(),
        }
    }
}

/// Gemini puts the model in the URL path. Accept an optional `models/` prefix but
/// never characters that could change the path, query or host.
pub fn gemini_model(model: &str) -> &str {
    model.trim().strip_prefix("models/").unwrap_or(model.trim())
}

/// One user-defined model interface. Deliberately no Debug: it holds an API key.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelProfile {
    /// Stable identifier used in selections: 1-32 of `a-z`, `0-9` and `-`.
    pub id: String,
    pub name: String,
    pub protocol: ModelProtocol,
    pub base_url: String,
    pub api_key: String,
    /// Empty when the interface is not used for recognition.
    pub ocr_model: String,
    /// Empty when the interface is not used for translation.
    pub translate_model: String,
}

impl ModelProfile {
    pub fn can_recognize(&self) -> bool {
        !self.ocr_model.trim().is_empty()
    }

    pub fn can_translate(&self) -> bool {
        self.protocol.translates() && !self.translate_model.trim().is_empty()
    }

    fn normalize(&mut self) -> Result<()> {
        self.id = self.id.trim().to_owned();
        anyhow::ensure!(
            (1..=32).contains(&self.id.len())
                && self
                    .id
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-'),
            "模型接口编号无效"
        );
        self.name = self.name.trim().to_owned();
        if self.name.is_empty() {
            self.name = self.protocol.label().into();
        }
        anyhow::ensure!(
            self.name.chars().count() <= 40 && !self.name.chars().any(char::is_control),
            "模型接口名称过长或含有控制字符"
        );
        let name = self.name.clone();
        self.api_key = self.api_key.trim().to_owned();
        self.base_url = self.base_url.trim().trim_end_matches('/').to_owned();
        anyhow::ensure!(!self.base_url.is_empty(), "模型接口「{name}」缺少服务地址");
        generic_endpoint(&self.base_url, "").with_context(|| format!("模型接口「{name}」"))?;
        if !self.protocol.translates() {
            self.translate_model.clear();
        }
        for model in [&mut self.ocr_model, &mut self.translate_model] {
            *model = model.trim().to_owned();
            validate_model(model, self.protocol).with_context(|| format!("模型接口「{name}」"))?;
        }
        anyhow::ensure!(
            self.can_recognize() || self.can_translate(),
            "模型接口「{name}」需要至少填写识别模型或翻译模型"
        );
        Ok(())
    }
}

/// `Model.{id}_{mode}` → (`id`, `mode`).
pub fn model_ocr_selection(selection: &str) -> Option<(&str, &str)> {
    selection
        .strip_prefix(MODEL_SELECTION_PREFIX)?
        .split_once('_')
}

/// `Model.{id}` → `id`.
pub fn model_translator(selection: &str) -> Option<&str> {
    selection
        .strip_prefix(MODEL_SELECTION_PREFIX)
        .filter(|id| !id.contains('_'))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyMode {
    #[default]
    System,
    Direct,
    Manual,
}

// No Debug: a proxy can contain authentication credentials.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxyConfig {
    pub mode: ProxyMode,
    pub url: String,
    pub username: String,
    pub password: String,
}

impl ProxyConfig {
    pub fn normalize(&mut self) -> Result<()> {
        // Inactive manual settings are a draft and do not prevent choosing System/Direct.
        if self.mode != ProxyMode::Manual {
            return Ok(());
        }
        self.url = self.url.trim().to_owned();
        self.manual_url()?;
        Ok(())
    }

    pub(crate) fn manual_url(&self) -> Result<reqwest::Url> {
        let raw = self.url.trim();
        anyhow::ensure!(
            !raw.is_empty() && raw.len() <= 2048 && !raw.chars().any(char::is_control),
            "请填写有效的手动代理地址"
        );
        let url = reqwest::Url::parse(raw).map_err(|_| anyhow!("手动代理地址格式无效"))?;
        anyhow::ensure!(
            matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h"),
            "代理协议仅支持 HTTP、HTTPS、SOCKS5 和 SOCKS5H"
        );
        anyhow::ensure!(
            url.host_str().is_some_and(|host| !host.is_empty()) && url.port() != Some(0),
            "代理地址缺少有效主机或端口"
        );
        let authority = raw
            .split_once("://")
            .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or_default())
            .unwrap_or_default();
        anyhow::ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && !authority.contains('@')
                && url.query().is_none()
                && url.fragment().is_none()
                && matches!(url.path(), "" | "/"),
            "代理地址不能包含账号、密码、路径或查询参数；请在独立字段填写凭据"
        );
        anyhow::ensure!(
            self.username.len() <= 1024
                && self.password.len() <= 4096
                && !self.username.chars().any(char::is_control)
                && !self.password.chars().any(char::is_control),
            "代理凭据过长或包含控制字符"
        );
        if matches!(url.scheme(), "socks5" | "socks5h") {
            let authenticated = !self.username.is_empty() || !self.password.is_empty();
            anyhow::ensure!(
                !authenticated
                    || (!self.username.is_empty()
                        && !self.password.is_empty()
                        && self.username.len() <= 255
                        && self.password.len() <= 255),
                "SOCKS5 认证须同时填写用户名和密码，且各不超过 255 字节"
            );
        } else {
            anyhow::ensure!(!self.username.contains(':'), "HTTP 代理用户名不能包含冒号");
        }
        Ok(url)
    }
}

pub const LANGUAGES: &[(&str, &str)] = &[
    ("auto", "自动识别"),
    ("zh-Hans", "简体中文"),
    ("zh-Hant", "繁体中文"),
    ("yue", "中文粤语"),
    ("en", "英语"),
    ("ja", "日语"),
    ("ko", "韩语"),
    ("fr", "法语"),
    ("es", "西班牙语"),
    ("ru", "俄语"),
    ("de", "德语"),
    ("it", "意大利语"),
    ("tr", "土耳其语"),
    ("pt-PT", "葡萄牙语"),
    ("pt", "巴西葡萄牙语"),
    ("vi", "越南语"),
    ("id", "印度尼西亚语"),
    ("th", "泰语"),
    ("ms", "马来语"),
    ("ar", "阿拉伯语"),
    ("hi", "印地语"),
    ("mn-Cyrl", "蒙古语(西里尔)"),
    ("mn-Mong", "蒙古语"),
    ("km", "高棉语"),
    ("nb", "书面挪威语"),
    ("nn", "新挪威语"),
    ("fa", "波斯语"),
    ("sv", "瑞典语"),
    ("pl", "波兰语"),
    ("nl", "荷兰语"),
    ("uk", "乌克兰语"),
    ("uz", "乌兹别克语"),
];

// Deliberately no Debug: formatting a configuration must not expose credentials.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub api_key: String,
    pub secret_key: String,
    pub baidu_trans_appid: String,
    pub baidu_trans_appkey: String,
    pub tencent_secret_id: String,
    pub tencent_secret_key: String,
    pub tencent_trans_secret_id: String,
    pub tencent_trans_secret_key: String,
    /// User-defined model interfaces (OpenAI-compatible, Responses, Anthropic,
    /// Gemini, Mistral OCR). Replaces the former fixed Mistral/OpenAI/NVIDIA fields.
    pub model_profiles: Vec<ModelProfile>,
    pub proxy: ProxyConfig,
    pub hotkey: String,
    pub translate_hotkey: String,
    pub silent_hotkey: String,
    pub last_ocr_selection: String,
    pub last_translate_selection: String,
    #[serde(alias = "last_source_lang")]
    pub source_lang: String,
    #[serde(alias = "last_target_lang")]
    pub target_lang: String,
    pub hide_tray_icon: bool,
    /// Layout analysis for text OCR: reading order and paragraph merging.
    /// Formerly "合并文本换行" (join every line), stored as `replace_newline`.
    #[serde(alias = "replace_newline")]
    pub layout_analysis: bool,
    pub autostart: bool,
    /// Look for a new release in the background about once a day.
    pub check_updates: bool,
    /// A release the user chose to skip; background checks stay quiet about it.
    pub skipped_update_version: String,
    /// Unix seconds of the last background check, so restarts do not re-check.
    pub last_update_check: u64,
    /// The version that last ran, to say "updated to vX" once after an update.
    pub last_run_version: String,
    // The installation marker is authoritative; never persist a per-user override.
    #[serde(skip_serializing)]
    pub run_as_admin: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            secret_key: String::new(),
            baidu_trans_appid: String::new(),
            baidu_trans_appkey: String::new(),
            tencent_secret_id: String::new(),
            tencent_secret_key: String::new(),
            tencent_trans_secret_id: String::new(),
            tencent_trans_secret_key: String::new(),
            model_profiles: Vec::new(),
            proxy: ProxyConfig::default(),
            hotkey: "F5".into(),
            translate_hotkey: "F2".into(),
            silent_hotkey: "F4".into(),
            last_ocr_selection: "默认".into(),
            last_translate_selection: "默认".into(),
            source_lang: "auto".into(),
            target_lang: "zh-Hans".into(),
            hide_tray_icon: false,
            layout_analysis: false,
            autostart: false,
            check_updates: true,
            skipped_update_version: String::new(),
            last_update_check: 0,
            last_run_version: String::new(),
            run_as_admin: false,
            extra: BTreeMap::new(),
        }
    }
}

impl Config {
    /// Headless commands read current settings without migrating or writing files.
    pub fn load_read_only() -> Result<Self> {
        let path = match std::env::var_os("SIGHTOCR_CONFIG") {
            Some(path) => {
                anyhow::ensure!(!path.is_empty(), "SIGHTOCR_CONFIG 不能为空");
                PathBuf::from(path)
            }
            None => default_path()?,
        };
        if path.try_exists().context("无法检查配置文件")? {
            Self::read(&path)
        } else {
            Ok(Self::default())
        }
    }

    pub fn load() -> Result<(Self, PathBuf)> {
        if let Some(override_path) = std::env::var_os("SIGHTOCR_CONFIG") {
            if override_path.is_empty() {
                bail!("SIGHTOCR_CONFIG 不能为空");
            }
            let path = PathBuf::from(override_path);
            // An explicit path provides an isolated configuration; do not import secrets.
            return Self::load_from(&path, &[]).map(|config| (config, path));
        }
        let path = default_path()?;
        let mut roots = Vec::new();
        if let Ok(exe) = std::env::current_exe() {
            if let Some(parent) = exe.parent() {
                roots.push(parent.to_owned());
            }
        }
        // Only inspect a working/source directory that actually belongs to SightOCR.
        for root in [
            std::env::current_dir().ok(),
            Some(PathBuf::from(env!("CARGO_MANIFEST_DIR"))),
        ]
        .into_iter()
        .flatten()
        {
            if root.join("Cargo.toml").is_file()
                && root.join("rust/main.rs").is_file()
                && !roots.contains(&root)
            {
                roots.push(root);
            }
        }
        Self::load_from(&path, &roots).map(|config| (config, path))
    }

    fn load_from(path: &Path, roots: &[PathBuf]) -> Result<Self> {
        if path.try_exists().context("无法检查配置文件")? {
            return Self::read(path);
        }
        for root in roots {
            let paths_file = root.join("paths.json");
            let candidate = if paths_file.try_exists().context("无法检查旧版路径配置")? {
                let paths = read_json(&paths_file)?;
                let object = paths
                    .as_object()
                    .context("旧版 paths.json 必须为 JSON 对象")?;
                match object.get("config_dir") {
                    Some(Value::String(dir)) if !dir.is_empty() => {
                        let dir = PathBuf::from(dir);
                        if dir.is_absolute() {
                            dir
                        } else {
                            root.join(dir)
                        }
                        .join("config.json")
                    }
                    None => root.join("config.json"),
                    _ => bail!("旧版 paths.json 的 config_dir 必须为非空目录路径"),
                }
            } else {
                root.join("config.json")
            };
            if candidate == path || !candidate.try_exists().context("无法检查旧版配置文件")?
            {
                continue;
            }
            let config = Self::read_with_legacy_names(&candidate, true)
                .context("旧版配置读取失败；原文件未修改")?;
            // A concurrent first launch may have created the new file while importing.
            config.write_atomic(path, false)?;
            return Self::read(path);
        }
        Ok(Self::default())
    }

    fn read(path: &Path) -> Result<Self> {
        Self::read_with_legacy_names(path, false)
    }

    fn read_with_legacy_names(path: &Path, legacy: bool) -> Result<Self> {
        let mut value = read_json(path)?;
        if let Some(object) = value.as_object_mut() {
            migrate_silent_hotkey(object)?;
            migrate_model_profiles(object);
            for (old, new) in [
                ("last_source_lang", "source_lang"),
                ("last_target_lang", "target_lang"),
                // 2.0.x keeps unknown keys when it saves, so a file written by
                // 2.1 and then by 2.0.x holds both names; both would be rejected.
                ("replace_newline", "layout_analysis"),
            ] {
                // Python's UI persisted last_*; the Rust format uses source/target_lang.
                if legacy && object.contains_key(old) {
                    object.remove(new);
                } else if object.contains_key(new) {
                    object.remove(old);
                }
            }
        }
        let mut config: Self = serde_json::from_value(value)
            .map_err(|_| anyhow!("配置字段类型无效；请修复配置文件后重试"))?;
        config.normalize()?;
        Ok(config)
    }

    pub fn normalize(&mut self) -> Result<()> {
        self.proxy.normalize()?;
        self.source_lang = normalize_language(&self.source_lang)?.into();
        self.target_lang = normalize_language(&self.target_lang)?.into();
        if self.target_lang == "auto" {
            self.target_lang = "zh-Hans".into();
        }
        for secret in [
            &mut self.api_key,
            &mut self.secret_key,
            &mut self.baidu_trans_appid,
            &mut self.baidu_trans_appkey,
            &mut self.tencent_secret_id,
            &mut self.tencent_secret_key,
            &mut self.tencent_trans_secret_id,
            &mut self.tencent_trans_secret_key,
        ] {
            *secret = secret.trim().to_owned();
        }
        anyhow::ensure!(
            self.model_profiles.len() <= MAX_MODEL_PROFILES,
            "模型接口最多 {MAX_MODEL_PROFILES} 个"
        );
        let mut ids = std::collections::BTreeSet::new();
        for profile in &mut self.model_profiles {
            profile.normalize()?;
            anyhow::ensure!(ids.insert(profile.id.clone()), "模型接口编号重复");
        }
        // A selection that points at a removed (or no longer capable) interface falls
        // back to the built-in engine instead of failing every later task.
        if let Some((id, _)) = model_ocr_selection(&self.last_ocr_selection) {
            if !self.profile(id).is_some_and(ModelProfile::can_recognize) {
                self.last_ocr_selection = "默认".into();
            }
        }
        if let Some(id) = model_translator(&self.last_translate_selection) {
            if !self.profile(id).is_some_and(ModelProfile::can_translate) {
                self.last_translate_selection = "默认".into();
            }
        }
        self.hotkey = self.hotkey.trim().to_owned();
        self.translate_hotkey = self.translate_hotkey.trim().to_owned();
        self.silent_hotkey = self.silent_hotkey.trim().to_owned();
        crate::platform::validate_hotkeys(
            &self.hotkey,
            &self.translate_hotkey,
            &self.silent_hotkey,
        )?;
        Ok(())
    }

    pub fn profile(&self, id: &str) -> Option<&ModelProfile> {
        self.model_profiles.iter().find(|profile| profile.id == id)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        // Do not replace a corrupt existing file with defaults after a failed load.
        if path.try_exists().context("无法检查配置文件")? {
            Self::read(path).context("现有配置文件损坏，拒绝覆盖；请先备份并修复")?;
        }
        let mut normalized = self.clone();
        normalized.normalize()?;
        normalized.write_atomic(path, true)
    }

    fn write_atomic(&self, path: &Path, replace: bool) -> Result<()> {
        let mut bytes = serde_json::to_vec_pretty(self).context("配置序列化失败")?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            bail!("保存后的配置超过 1 MiB 限制");
        }
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent).context("无法创建配置目录")?;
        let mut temporary =
            tempfile::NamedTempFile::new_in(parent).context("无法创建临时配置文件")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            temporary
                .as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all().context("配置写入失败")?;
        if replace {
            temporary
                .persist(path)
                .map_err(|e| e.error)
                .context("无法原子替换配置文件")?;
        } else {
            match temporary.persist_noclobber(path) {
                Ok(_) => {}
                Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.error).context("无法导入旧版配置"),
            }
        }
        Ok(())
    }
}

/// Former fixed providers: (id, name, protocol, field prefix, selection prefix,
/// default base URL, default OCR model, default translation model).
type LegacyProvider = (
    &'static str,
    &'static str,
    ModelProtocol,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
);
const LEGACY_PROVIDERS: &[LegacyProvider] = &[
    (
        "openai",
        "OpenAI",
        ModelProtocol::OpenaiChat,
        "openai",
        "OpenAI",
        OPENAI_BASE_URL,
        OPENAI_MODEL,
        OPENAI_MODEL,
    ),
    (
        "nvidia",
        "NVIDIA",
        ModelProtocol::OpenaiChat,
        "nvidia",
        "Nvidia",
        NVIDIA_BASE_URL,
        NVIDIA_OCR_MODEL,
        NVIDIA_TRANSLATE_MODEL,
    ),
    (
        "mistral",
        "Mistral OCR",
        ModelProtocol::MistralOcr,
        "mistral",
        "Mistral",
        MISTRAL_BASE_URL,
        MISTRAL_OCR_MODEL,
        "",
    ),
    (
        "generic",
        "通用模型",
        ModelProtocol::OpenaiChat,
        "generic",
        "Generic",
        "",
        "",
        "",
    ),
];

/// Turn the former fixed Mistral/OpenAI/NVIDIA/generic fields into model interfaces.
/// Only configured or selected providers become interfaces; their legacy fields
/// (including keys) are removed so secrets are not kept twice. Runs in memory only:
/// the file changes when the user next saves.
fn migrate_model_profiles(object: &mut serde_json::Map<String, Value>) {
    let mut profiles = Vec::new();
    for &(id, name, protocol, field, selection, base, ocr, translate) in LEGACY_PROVIDERS {
        let mut take = |suffix: &str| -> String {
            object
                .remove(&format!("{field}_{suffix}"))
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default()
        };
        let api_key = take("api_key");
        let base_url = take("base_url");
        let ocr_model = take("ocr_model");
        let translate_model = take("translate_model");
        let protocol = object
            .remove(&format!("{field}_protocol"))
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or(protocol);
        let selected = |key: &str, exact: bool| {
            object
                .get(key)
                .and_then(Value::as_str)
                .is_some_and(|value| {
                    if exact {
                        value == selection
                    } else {
                        value.starts_with(&format!("{selection}_"))
                    }
                })
        };
        let in_use =
            selected("last_ocr_selection", false) || selected("last_translate_selection", true);
        let configured = if id == "generic" {
            !base_url.trim().is_empty()
        } else {
            !api_key.trim().is_empty()
        };
        if object.contains_key("model_profiles") || !(configured || in_use) {
            continue;
        }
        let or = |value: String, default: &str| {
            if value.trim().is_empty() {
                default.to_owned()
            } else {
                value
            }
        };
        let profile = ModelProfile {
            id: id.into(),
            name: name.into(),
            protocol,
            base_url: or(base_url, base),
            api_key,
            ocr_model: or(ocr_model, ocr),
            translate_model: or(translate_model, translate),
        };
        if !profile.base_url.trim().is_empty() {
            profiles.push(profile);
        }
    }
    if object.contains_key("model_profiles") {
        return;
    }
    let ids: Vec<_> = profiles.iter().map(|profile| profile.id.clone()).collect();
    for key in ["last_ocr_selection", "last_translate_selection"] {
        let Some(value) = object.get(key).and_then(Value::as_str) else {
            continue;
        };
        let remapped = LEGACY_PROVIDERS
            .iter()
            .find_map(|&(id, _, _, _, selection, ..)| {
                let rest = value.strip_prefix(selection)?;
                (rest.is_empty() || rest.starts_with('_')).then(|| (id, rest.to_owned()))
            })
            .map(|(id, rest)| {
                if ids.iter().any(|known| known == id) {
                    format!("{MODEL_SELECTION_PREFIX}{id}{rest}")
                } else {
                    "默认".into()
                }
            });
        if let Some(remapped) = remapped {
            object.insert(key.into(), Value::String(remapped));
        }
    }
    if let Ok(value) = serde_json::to_value(profiles) {
        object.insert("model_profiles".into(), value);
    }
}

fn migrate_silent_hotkey(object: &mut serde_json::Map<String, Value>) -> Result<()> {
    // A saved third binding marks migration complete, even if the user later
    // chooses F4 for ordinary OCR. Existing explicit values are never reset.
    if object.contains_key("silent_hotkey") {
        return Ok(());
    }
    let read_key = |field: &str, default: &str| -> Result<String> {
        match object.get(field) {
            Some(Value::String(text)) => Ok(text.clone()),
            None => Ok(default.to_owned()),
            _ => bail!("配置中的快捷键字段类型无效"),
        }
    };
    let mut ocr = read_key("hotkey", "F4")?;
    let translate = read_key("translate_hotkey", "F2")?;
    let matches = crate::platform::hotkeys_match;
    if matches(&ocr, &translate)? {
        bail!("识别和翻译不能使用相同的快捷键");
    }
    if matches(&ocr, "F4")? && !matches(&translate, "F5")? {
        ocr = "F5".into();
    }
    // Only the new action gets a fallback; user-defined bindings retain priority.
    let silent = std::iter::once("F4".to_owned())
        .chain((6..=24).map(|number| format!("F{number}")))
        .find(|candidate| {
            !matches(&ocr, candidate).unwrap_or(true)
                && !matches(&translate, candidate).unwrap_or(true)
        })
        .context("无法为静默识别选择空闲的默认快捷键")?;
    object.insert("hotkey".into(), Value::String(ocr));
    object.insert("silent_hotkey".into(), Value::String(silent));
    Ok(())
}

/// API bases may use a user-selected HTTPS gateway, but never inline credentials or queries.
/// Errors deliberately omit the URL, which may have contained a mistakenly pasted key.
pub(crate) fn api_endpoint(base: &str, path: &str) -> Result<reqwest::Url> {
    anyhow::ensure!(
        base.len() <= 2048 && !base.chars().any(char::is_control),
        "API 地址无效"
    );
    let base = reqwest::Url::parse(base.trim()).map_err(|_| anyhow!("API 地址格式无效"))?;
    anyhow::ensure!(
        base.scheme() == "https"
            && base.host_str().is_some()
            && base.username().is_empty()
            && base.password().is_none()
            && base.query().is_none()
            && base.fragment().is_none(),
        "API 地址必须使用 HTTPS，且不能包含账号、密码、查询参数或片段"
    );
    reqwest::Url::parse(&format!("{}/{}", base.as_str().trim_end_matches('/'), path))
        .map_err(|_| anyhow!("API 地址格式无效"))
}

/// Like [`api_endpoint`], but also accepts plain HTTP on loopback hosts so local
/// runtimes (Ollama, LM Studio, vLLM, llama.cpp) work. Remote HTTP is still rejected.
pub fn generic_endpoint(base: &str, path: &str) -> Result<reqwest::Url> {
    anyhow::ensure!(
        base.len() <= 2048 && !base.chars().any(char::is_control),
        "API 地址无效"
    );
    let parsed = reqwest::Url::parse(base.trim()).map_err(|_| anyhow!("API 地址格式无效"))?;
    if parsed.scheme() == "http" {
        anyhow::ensure!(
            is_loopback(&parsed)
                && parsed.username().is_empty()
                && parsed.password().is_none()
                && parsed.query().is_none()
                && parsed.fragment().is_none(),
            "HTTP 地址仅允许本机服务（localhost / 127.0.0.1 / ::1）；远程服务请使用 HTTPS"
        );
        return reqwest::Url::parse(&format!(
            "{}/{}",
            parsed.as_str().trim_end_matches('/'),
            path
        ))
        .map_err(|_| anyhow!("API 地址格式无效"));
    }
    api_endpoint(base, path)
}

/// Whether a (possibly incomplete) generic base URL points at this machine.
pub fn generic_url_is_loopback(base: &str) -> bool {
    reqwest::Url::parse(base.trim()).is_ok_and(|url| is_loopback(&url))
}

pub(crate) fn is_loopback(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Model names are sent in JSON bodies; Gemini also places them in the URL path.
pub(crate) fn validate_model(model: &str, protocol: ModelProtocol) -> Result<()> {
    anyhow::ensure!(
        model.len() <= 256 && !model.chars().any(char::is_control),
        "模型名称过长或含有控制字符"
    );
    if protocol == ModelProtocol::Gemini && !model.is_empty() {
        let name = gemini_model(model);
        anyhow::ensure!(
            !name.is_empty()
                && name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.')),
            "Gemini 模型名称只能包含字母、数字、短横线、下划线和点"
        );
    }
    Ok(())
}

fn default_path() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("APPDATA").filter(|s| !s.is_empty()) {
        return Ok(PathBuf::from(dir).join("SightOCR").join("config.json"));
    }
    #[cfg(not(windows))]
    {
        if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
            return Ok(PathBuf::from(dir).join("SightOCR/config.json"));
        }
        if let Some(dir) = std::env::var_os("HOME").filter(|s| !s.is_empty()) {
            return Ok(PathBuf::from(dir).join(".config/SightOCR/config.json"));
        }
    }
    bail!("无法确定用户配置目录（APPDATA 未设置）")
}

fn read_json(path: &Path) -> Result<Value> {
    let file = File::open(path).with_context(|| format!("无法读取配置文件：{}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("配置读取失败")?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        bail!("配置文件超过 1 MiB 限制");
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    serde_json::from_slice(bytes).map_err(|_| anyhow!("配置 JSON 无效；原文件未修改"))
}

pub fn normalize_language(language: &str) -> Result<&'static str> {
    let legacy = match language.trim() {
        "中文" | "zh" | "zh-CN" => "zh-Hans",
        "zh-TW" => "zh-Hant",
        "English" => "en",
        "日本語" => "ja",
        "한국어" => "ko",
        "Français" => "fr",
        "Deutsch" => "de",
        "Русский" => "ru",
        "Español" => "es",
        value => value,
    };
    LANGUAGES
        .iter()
        .find(|(code, name)| *code == legacy || *name == legacy)
        .map(|(code, _)| *code)
        .context("配置中的翻译语言无效，请重新选择语言")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_saved_by_2_1_then_2_0_with_both_layout_names_loads() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("config.json");
        // 2.0.x keeps 2.1's keys when it saves, next to its own old names.
        std::fs::write(
            &path,
            r#"{"replace_newline":false,"layout_analysis":true,"hotkey":"F5",
                "translate_hotkey":"F2","silent_hotkey":"F4","model_profiles":[],
                "openai_api_key":"","openai_base_url":"https://api.openai.com/v1"}"#,
        )?;
        let config = Config::read(&path)?;
        assert!(config.layout_analysis, "the 2.1 name wins");
        // An old file with only the 2.0 name still migrates.
        std::fs::write(&path, r#"{"replace_newline":true}"#)?;
        assert!(Config::read(&path)?.layout_analysis);
        Ok(())
    }

    #[test]
    fn fresh_hotkeys_are_f5_f2_f4_and_legacy_defaults_migrate_once() -> Result<()> {
        let defaults = Config::default();
        assert_eq!(defaults.hotkey, "F5");
        assert_eq!(defaults.translate_hotkey, "F2");
        assert_eq!(defaults.silent_hotkey, "F4");
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("config.json");
        for old in ["{}", r#"{"hotkey":" f4 ","translate_hotkey":"F2"}"#] {
            fs::write(&path, old)?;
            let config = Config::read(&path)?;
            assert_eq!(config.hotkey, "F5");
            assert_eq!(config.translate_hotkey, "F2");
            assert_eq!(config.silent_hotkey, "F4");
            assert_eq!(
                fs::read_to_string(&path)?,
                old,
                "loading must not rewrite config"
            );
            config.save(&path)?;
            let saved = read_json(&path)?;
            assert_eq!(saved["silent_hotkey"], "F4");
            assert!(!config.extra.contains_key("silent_hotkey"));
            let reloaded = Config::read(&path)?;
            assert_eq!(reloaded.hotkey, config.hotkey);
            assert_eq!(reloaded.silent_hotkey, config.silent_hotkey);
        }
        Ok(())
    }

    #[test]
    fn silent_hotkey_migration_preserves_custom_bindings_and_avoids_occupied_defaults() -> Result<()>
    {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("config.json");
        for (ocr, translate, expected_ocr, silent) in [
            ("F4", "F5", "F4", "F6"),
            ("Ctrl+F4", "Alt+F2", "Ctrl+F4", "F4"),
            ("Ctrl+O", "F4", "Ctrl+O", "F6"),
            ("F6", "F4", "F6", "F7"),
            ("F4", "F6", "F5", "F4"),
        ] {
            fs::write(
                &path,
                serde_json::to_vec(&serde_json::json!({
                    "hotkey": ocr, "translate_hotkey": translate,
                    "future_option": true, "openai_api_key": "synthetic-secret"
                }))?,
            )?;
            let config = Config::read(&path)?;
            assert_eq!(config.hotkey, expected_ocr);
            assert_eq!(config.translate_hotkey, translate);
            assert_eq!(config.silent_hotkey, silent);
            assert_eq!(config.extra["future_option"], true);
            assert_eq!(
                config.profile("openai").unwrap().api_key,
                "synthetic-secret"
            );
        }
        Ok(())
    }

    #[test]
    fn explicitly_saved_silent_hotkey_is_not_migrated_or_reset() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("config.json");
        let mut config = Config {
            hotkey: "F4".into(),
            translate_hotkey: "F5".into(),
            silent_hotkey: " Ctrl + Shift + S ".into(),
            ..Config::default()
        };
        config.normalize()?;
        config.save(&path)?;
        for _ in 0..2 {
            let reloaded = Config::read(&path)?;
            assert_eq!(reloaded.hotkey, "F4");
            assert_eq!(reloaded.translate_hotkey, "F5");
            assert_eq!(reloaded.silent_hotkey, "Ctrl + Shift + S");
            reloaded.save(&path)?;
        }
        Ok(())
    }

    #[test]
    fn all_three_hotkeys_reject_equivalent_duplicates_and_invalid_silent_bindings() -> Result<()> {
        for (ocr, translate, silent) in [
            ("Ctrl+O", "control+o", "F4"),
            ("F5", "Win+Shift+O", "shift+meta+o"),
            ("Ctrl+O", "F2", "control+o"),
            ("F5", "F2", ""),
            ("F5", "F2", "Ctrl"),
        ] {
            let mut config = Config {
                hotkey: ocr.into(),
                translate_hotkey: translate.into(),
                silent_hotkey: silent.into(),
                ..Config::default()
            };
            assert!(config.normalize().is_err());
        }
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("config.json");
        // A malformed explicit third field is an error, never a migration trigger.
        for text in [
            r#"{"hotkey":"F5","translate_hotkey":"F2","silent_hotkey":"F5"}"#,
            r#"{"silent_hotkey":false}"#,
            r#"{"silent_hotkey":""}"#,
            r#"{"hotkey":"Ctrl+O","translate_hotkey":"control+o"}"#,
        ] {
            fs::write(&path, text)?;
            assert!(Config::read(&path).is_err());
            assert_eq!(fs::read_to_string(&path)?, text);
        }
        Ok(())
    }

    #[test]
    fn imports_custom_paths_without_touching_source_and_preserves_unknown_fields() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("old");
        let custom = root.join("preferences");
        fs::create_dir_all(&custom)?;
        fs::write(root.join("paths.json"), br#"{"config_dir":"preferences"}"#)?;
        let original = r#"{"source_lang":"ja","last_source_lang":"English","last_target_lang":"中文","future_option":{"enabled":true}}"#;
        fs::write(custom.join("config.json"), original)?;
        let dest = temp.path().join("new/config.json");
        let config = Config::load_from(&dest, &[root])?;
        assert_eq!(config.source_lang, "en");
        assert_eq!(config.target_lang, "zh-Hans");
        assert_eq!(config.extra["future_option"]["enabled"], true);
        assert_eq!(fs::read_to_string(custom.join("config.json"))?, original);
        config.save(&dest)?;
        assert_eq!(Config::read(&dest)?.extra["future_option"]["enabled"], true);
        Ok(())
    }

    #[test]
    fn corrupt_config_is_never_overwritten_or_reimported() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("config.json");
        fs::write(&path, b"{broken")?;
        assert!(Config::load_from(&path, &[]).is_err());
        assert!(Config::default().save(&path).is_err());
        assert_eq!(fs::read(&path)?, b"{broken");
        fs::write(&path, br#"{"hotkey":false}"#)?;
        assert!(Config::default().save(&path).is_err());
        assert_eq!(fs::read(&path)?, br#"{"hotkey":false}"#);
        Ok(())
    }

    #[test]
    fn canonical_language_wins_and_bom_is_accepted() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("config.json");
        fs::write(
            &path,
            "\u{feff}{\"source_lang\":\"ja\",\"last_source_lang\":\"English\"}",
        )?;
        assert_eq!(Config::read(&path)?.source_lang, "ja");
        assert_eq!(normalize_language("Русский")?, "ru");
        assert!(normalize_language("not-a-language").is_err());
        Ok(())
    }

    #[test]
    fn fixed_ai_providers_migrate_to_model_interfaces_and_selections_follow() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("config.json");
        fs::write(
            &path,
            r#"{"api_key":"old-ocr-key","appearance":"dark","future_option":42,
               "openai_api_key":" synthetic-ai-key ","openai_base_url":"https://gateway.example/custom/v1/",
               "openai_translate_model":"custom/model",
               "nvidia_api_key":"","nvidia_base_url":"https://integrate.api.nvidia.com/v1",
               "mistral_api_key":"synthetic-mistral",
               "generic_protocol":"gemini","generic_base_url":"https://generativelanguage.example/v1beta",
               "generic_api_key":"g","generic_ocr_model":"models/gemini-test",
               "last_ocr_selection":"Mistral_table","last_translate_selection":"OpenAI"}"#,
        )?;
        let original = fs::read(&path)?;
        let config = Config::read(&path)?;
        assert_eq!(
            fs::read(&path)?,
            original,
            "loading must not rewrite config"
        );
        let ids: Vec<_> = config
            .model_profiles
            .iter()
            .map(|p| p.id.as_str())
            .collect();
        // NVIDIA had no key and was not selected, so it is not created.
        assert_eq!(ids, ["openai", "mistral", "generic"]);
        let openai = config.profile("openai").unwrap();
        assert_eq!(openai.api_key, "synthetic-ai-key");
        assert_eq!(openai.base_url, "https://gateway.example/custom/v1");
        assert_eq!(openai.ocr_model, OPENAI_MODEL);
        assert_eq!(openai.translate_model, "custom/model");
        let mistral = config.profile("mistral").unwrap();
        assert_eq!(mistral.protocol, ModelProtocol::MistralOcr);
        assert_eq!(mistral.base_url, MISTRAL_BASE_URL);
        assert_eq!(mistral.ocr_model, MISTRAL_OCR_MODEL);
        assert!(!mistral.can_translate());
        let generic = config.profile("generic").unwrap();
        assert_eq!(generic.protocol, ModelProtocol::Gemini);
        assert_eq!(generic.ocr_model, "models/gemini-test");
        assert_eq!(config.last_ocr_selection, "Model.mistral_table");
        assert_eq!(config.last_translate_selection, "Model.openai");
        assert_eq!(config.api_key, "old-ocr-key");
        config.save(&path)?;
        let saved: Value = serde_json::from_slice(&fs::read(&path)?)?;
        for legacy in [
            "openai_api_key",
            "mistral_api_key",
            "generic_base_url",
            "nvidia_base_url",
        ] {
            assert!(
                saved.get(legacy).is_none(),
                "{legacy} must not be kept twice"
            );
        }
        assert_eq!(saved["appearance"], "dark");
        assert_eq!(saved["future_option"], 42);
        assert_eq!(saved["model_profiles"][0]["protocol"], "openai_chat");
        let reloaded = Config::read(&path)?;
        assert!(reloaded.model_profiles == config.model_profiles);
        assert_eq!(reloaded.last_translate_selection, "Model.openai");
        Ok(())
    }

    #[test]
    fn unconfigured_selected_provider_migrates_with_defaults_and_unknown_falls_back() -> Result<()>
    {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("config.json");
        fs::write(
            &path,
            r#"{"last_ocr_selection":"Nvidia_formula","last_translate_selection":"Generic"}"#,
        )?;
        let config = Config::read(&path)?;
        // The selected NVIDIA interface is kept (with documented defaults) so the user
        // only needs to add a key; the empty generic interface cannot be kept.
        let nvidia = config.profile("nvidia").unwrap();
        assert_eq!(nvidia.base_url, NVIDIA_BASE_URL);
        assert_eq!(nvidia.ocr_model, NVIDIA_OCR_MODEL);
        assert_eq!(config.last_ocr_selection, "Model.nvidia_formula");
        assert_eq!(config.last_translate_selection, "默认");
        assert!(config.profile("generic").is_none());
        Ok(())
    }

    #[test]
    fn model_interfaces_validate_and_selections_to_removed_interfaces_reset() -> Result<()> {
        let valid = ModelProfile {
            id: "tokenhub".into(),
            name: " 混元 ".into(),
            protocol: ModelProtocol::OpenaiChat,
            base_url: " https://tokenhub.example/v1/ ".into(),
            api_key: " key ".into(),
            ocr_model: "vision".into(),
            translate_model: "text".into(),
        };
        let mut config = Config {
            model_profiles: vec![valid.clone()],
            last_ocr_selection: "Model.tokenhub_table".into(),
            last_translate_selection: "Model.tokenhub".into(),
            ..Config::default()
        };
        config.normalize()?;
        let profile = &config.model_profiles[0];
        assert_eq!(profile.name, "混元");
        assert_eq!(profile.base_url, "https://tokenhub.example/v1");
        assert_eq!(profile.api_key, "key");
        assert_eq!(config.last_ocr_selection, "Model.tokenhub_table");
        assert_eq!(
            model_ocr_selection(&config.last_ocr_selection),
            Some(("tokenhub", "table"))
        );
        assert_eq!(
            model_translator(&config.last_translate_selection),
            Some("tokenhub")
        );
        // Removing the interface resets both selections to the built-in engines.
        config.model_profiles.clear();
        config.normalize()?;
        assert_eq!(config.last_ocr_selection, "默认");
        assert_eq!(config.last_translate_selection, "默认");
        for broken in [
            ModelProfile {
                id: "Bad_ID".into(),
                ..valid.clone()
            },
            ModelProfile {
                base_url: String::new(),
                ..valid.clone()
            },
            ModelProfile {
                base_url: "http://remote.example/v1".into(),
                ..valid.clone()
            },
            ModelProfile {
                ocr_model: String::new(),
                translate_model: String::new(),
                ..valid.clone()
            },
            ModelProfile {
                name: "x".repeat(41),
                ..valid.clone()
            },
            ModelProfile {
                protocol: ModelProtocol::Gemini,
                ocr_model: "a/b".into(),
                ..valid.clone()
            },
        ] {
            let mut config = Config {
                model_profiles: vec![broken],
                ..Config::default()
            };
            assert!(config.normalize().is_err());
        }
        let mut duplicate = Config {
            model_profiles: vec![valid.clone(), valid.clone()],
            ..Config::default()
        };
        assert!(duplicate.normalize().is_err());
        // Mistral OCR cannot translate; its translation model is cleared.
        let mut mistral = Config {
            model_profiles: vec![ModelProfile {
                protocol: ModelProtocol::MistralOcr,
                ..valid
            }],
            last_translate_selection: "Model.tokenhub".into(),
            ..Config::default()
        };
        mistral.normalize()?;
        assert!(mistral.model_profiles[0].translate_model.is_empty());
        assert_eq!(mistral.last_translate_selection, "默认");
        Ok(())
    }

    #[test]
    fn ai_api_bases_require_https_and_never_echo_embedded_secrets() -> Result<()> {
        for base in [
            "http://localhost/v1",
            "https://user:private-key@example.com/v1",
            "https://example.com/v1?api_key=private-key",
            "https://example.com/v1#private-key",
            "not-a-url-private-key",
            "https://example.com/\nprivate-key",
        ] {
            let error = api_endpoint(base, "chat/completions")
                .unwrap_err()
                .to_string();
            assert!(!error.contains("private-key"));
        }
        assert_eq!(
            api_endpoint("https://gateway.example/custom/v1/", "chat/completions")?.as_str(),
            "https://gateway.example/custom/v1/chat/completions"
        );
        Ok(())
    }

    #[test]
    fn model_interfaces_allow_http_only_on_loopback() -> Result<()> {
        for base in [
            "http://localhost:11434/v1",
            "http://127.0.0.1:1234/v1",
            "http://[::1]:8000/v1",
            "https://gateway.example/v1",
        ] {
            generic_endpoint(base, "chat/completions")?;
        }
        for base in [
            "http://192.168.1.10:11434/v1",
            "http://example.com/v1",
            "http://localhost.example.com/v1",
            "http://user:private-key@localhost/v1",
            "http://localhost/v1?key=private-key",
        ] {
            let error = generic_endpoint(base, "").unwrap_err().to_string();
            assert!(!error.contains("private-key"), "{base}");
        }
        assert!(api_endpoint("http://localhost:11434/v1", "").is_err());
        Ok(())
    }

    #[test]
    fn old_configs_default_to_system_proxy_and_preserve_manual_drafts_on_roundtrip() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("config.json");
        fs::write(&path, r#"{"api_key":"synthetic-key","appearance":"dark"}"#)?;
        let mut config = Config::read(&path)?;
        assert_eq!(config.proxy.mode, ProxyMode::System);
        assert!(
            config.proxy.url.is_empty()
                && config.proxy.username.is_empty()
                && config.proxy.password.is_empty()
        );
        config.proxy = ProxyConfig {
            mode: ProxyMode::Direct,
            url: "incomplete manual draft".into(),
            username: "user name".into(),
            password: " secret with spaces ".into(),
        };
        config.save(&path)?;
        let saved = Config::read(&path)?;
        assert!(saved.proxy == config.proxy);
        assert_eq!(saved.api_key, "synthetic-key");
        assert_eq!(saved.extra["appearance"], "dark");
        assert!(!saved.extra.contains_key("proxy"));
        let serialized: Value = serde_json::from_slice(&fs::read(&path)?)?;
        assert_eq!(serialized["proxy"]["mode"], "direct");
        assert!(
            serde_json::from_value::<Config>(serde_json::json!({"proxy":{"mode":"unknown"}}))
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn manual_proxy_validation_rejects_ambiguous_addresses_without_echoing_secrets() -> Result<()> {
        for url in [
            "",
            "private-secret",
            "ftp://host:21",
            "http://host:0",
            "http://host:99999",
            "http://user:private-secret@host:8080",
            "http://@host:8080",
            "http://host:8080/path",
            "http://host:8080?password=private-secret",
            "http://host:8080#private-secret",
            "http://host:8080/\nprivate-secret",
            "socks4://host:1080",
        ] {
            let mut config = ProxyConfig {
                mode: ProxyMode::Manual,
                url: url.into(),
                ..ProxyConfig::default()
            };
            let error = config.normalize().unwrap_err().to_string();
            assert!(!error.contains("private-secret"));
            // The same draft must never block disabling the proxy or using the OS setting.
            for mode in [ProxyMode::Direct, ProxyMode::System] {
                config.mode = mode;
                config.normalize()?;
            }
        }
        for url in [
            "http://127.0.0.1:7890",
            "https://proxy.example:443/",
            "socks5://[::1]:1080",
            "socks5h://proxy.example:1080",
        ] {
            let mut config = ProxyConfig {
                mode: ProxyMode::Manual,
                url: format!(" {url} "),
                ..ProxyConfig::default()
            };
            config.normalize()?;
            assert_eq!(config.url, url);
        }
        Ok(())
    }

    #[test]
    fn proxy_credentials_validate_for_the_selected_protocol_without_trimming_passwords(
    ) -> Result<()> {
        let mut config = ProxyConfig {
            mode: ProxyMode::Manual,
            url: "http://proxy.example:8080".into(),
            username: "name".into(),
            password: " spaces: % ".into(),
        };
        config.normalize()?;
        assert_eq!(config.password, " spaces: % ");
        config.username = "invalid:name".into();
        assert!(config.normalize().is_err());
        config.url = "socks5h://proxy.example:1080".into();
        config.normalize()?;
        config.password.clear();
        assert!(config.normalize().is_err());
        config.password = "界".repeat(86);
        assert!(config.normalize().is_err());
        config.mode = ProxyMode::Direct;
        config.normalize()?;
        Ok(())
    }
}
