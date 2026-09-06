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
    pub mistral_api_key: String,
    pub mistral_base_url: String,
    pub mistral_ocr_model: String,
    pub openai_api_key: String,
    pub openai_base_url: String,
    pub openai_ocr_model: String,
    pub openai_translate_model: String,
    pub nvidia_api_key: String,
    pub nvidia_base_url: String,
    pub nvidia_ocr_model: String,
    pub nvidia_translate_model: String,
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
    pub replace_newline: bool,
    pub autostart: bool,
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
            mistral_api_key: String::new(),
            mistral_base_url: MISTRAL_BASE_URL.into(),
            mistral_ocr_model: MISTRAL_OCR_MODEL.into(),
            openai_api_key: String::new(),
            openai_base_url: OPENAI_BASE_URL.into(),
            openai_ocr_model: OPENAI_MODEL.into(),
            openai_translate_model: OPENAI_MODEL.into(),
            nvidia_api_key: String::new(),
            nvidia_base_url: NVIDIA_BASE_URL.into(),
            nvidia_ocr_model: NVIDIA_OCR_MODEL.into(),
            nvidia_translate_model: NVIDIA_TRANSLATE_MODEL.into(),
            proxy: ProxyConfig::default(),
            hotkey: "F5".into(),
            translate_hotkey: "F2".into(),
            silent_hotkey: "F4".into(),
            last_ocr_selection: "默认".into(),
            last_translate_selection: "默认".into(),
            source_lang: "auto".into(),
            target_lang: "zh-Hans".into(),
            hide_tray_icon: false,
            replace_newline: false,
            autostart: false,
            extra: BTreeMap::new(),
        }
    }
}

impl Config {
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
            if root.join("SightOCR.py").is_file() && !roots.contains(&root) {
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
            for (old, new) in [
                ("last_source_lang", "source_lang"),
                ("last_target_lang", "target_lang"),
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
            &mut self.mistral_api_key,
            &mut self.openai_api_key,
            &mut self.nvidia_api_key,
        ] {
            *secret = secret.trim().to_owned();
        }
        for (base, default) in [
            (&mut self.mistral_base_url, MISTRAL_BASE_URL),
            (&mut self.openai_base_url, OPENAI_BASE_URL),
            (&mut self.nvidia_base_url, NVIDIA_BASE_URL),
        ] {
            *base = base.trim().trim_end_matches('/').to_owned();
            if base.is_empty() {
                *base = default.into();
            }
            api_endpoint(base, "")?;
        }
        for (model, default) in [
            (&mut self.mistral_ocr_model, MISTRAL_OCR_MODEL),
            (&mut self.openai_ocr_model, OPENAI_MODEL),
            (&mut self.openai_translate_model, OPENAI_MODEL),
            (&mut self.nvidia_ocr_model, NVIDIA_OCR_MODEL),
            (&mut self.nvidia_translate_model, NVIDIA_TRANSLATE_MODEL),
        ] {
            *model = model.trim().to_owned();
            if model.is_empty() {
                *model = default.into();
            }
            if model.len() > 256 || model.chars().any(char::is_control) {
                bail!("模型名称过长或含有控制字符");
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
            assert_eq!(config.openai_api_key, "synthetic-secret");
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
    fn ai_fields_migrate_without_erasing_legacy_keys_or_unknown_preferences() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("config.json");
        fs::write(
            &path,
            r#"{"api_key":"old-ocr-key","appearance":"dark","future_option":42}"#,
        )?;
        let mut config = Config::read(&path)?;
        assert_eq!(config.api_key, "old-ocr-key");
        assert!(config.mistral_api_key.is_empty());
        assert_eq!(config.mistral_ocr_model, MISTRAL_OCR_MODEL);
        assert_eq!(config.openai_ocr_model, OPENAI_MODEL);
        assert_eq!(config.nvidia_ocr_model, NVIDIA_OCR_MODEL);
        config.openai_api_key = " synthetic-ai-key ".into();
        config.openai_base_url = " https://gateway.example/custom/v1/ ".into();
        config.openai_translate_model = " custom/model ".into();
        config.save(&path)?;
        let saved = Config::read(&path)?;
        assert_eq!(saved.openai_api_key, "synthetic-ai-key");
        assert_eq!(saved.openai_base_url, "https://gateway.example/custom/v1");
        assert_eq!(saved.openai_translate_model, "custom/model");
        assert_eq!(saved.api_key, "old-ocr-key");
        assert_eq!(saved.extra["appearance"], "dark");
        assert_eq!(saved.extra["future_option"], 42);
        assert!(!saved.extra.contains_key("openai_api_key"));
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
        let mut config = Config {
            mistral_base_url: " ".into(),
            nvidia_translate_model: " ".into(),
            ..Config::default()
        };
        config.normalize()?;
        assert_eq!(config.mistral_base_url, MISTRAL_BASE_URL);
        assert_eq!(config.nvidia_translate_model, NVIDIA_TRANSLATE_MODEL);
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
