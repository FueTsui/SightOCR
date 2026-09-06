//! Cloud providers. No input text, API credentials, tokens, or response bodies are logged.
//! Provider selection is explicit: a failed request never sends data to another provider.

use crate::config::{normalize_language, Config, ProxyConfig, ProxyMode};
use anyhow::{Context, Result};
use base64::Engine;
use chrono::{TimeZone, Utc};
use hmac::{Hmac, Mac};
use md5::Md5;
use reqwest::blocking::{Client, ClientBuilder, RequestBuilder, Response};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fmt;
use std::io::Read;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod ai;
#[cfg(test)]
mod proxy_tests;

const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_TABLE_CELLS: usize = 100_000;
const MAX_TABLE_ROWS: usize = 10_000;
const MAX_TABLE_COLS: usize = 1_000;
const MAX_TABLES: usize = 100;
const CONTENT_TYPE: &str = "application/json; charset=utf-8";

#[derive(Debug)]
pub enum ServiceError {
    Credentials(&'static str),
    InvalidInput(&'static str),
    Network(&'static str),
    Http {
        provider: &'static str,
        status: u16,
    },
    Provider {
        provider: &'static str,
        code: String,
    },
    InvalidResponse(&'static str),
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Credentials(provider) => write!(f, "{provider}凭据未填写完整，请在设置中检查"),
            Self::InvalidInput(message) => f.write_str(message),
            Self::Network(message) => f.write_str(message),
            Self::Http { provider, status } => write!(
                f,
                "{provider}请求失败（HTTP {status}），请检查网络、服务权限或配额"
            ),
            Self::Provider { provider, code } => {
                write!(f, "{provider}返回错误（{code}），请检查凭据、权限或配额")
            }
            Self::InvalidResponse(message) => write!(f, "服务响应无效：{message}"),
        }
    }
}
impl std::error::Error for ServiceError {}

struct Token {
    credentials: [u8; 32],
    value: String,
    expires: Instant,
}

struct BingSession {
    key: String,
    token: String,
    ig: String,
    iid: String,
    cookies: String,
    expires: Instant,
    requests: u32,
}

fn build_client(proxy: &ProxyConfig) -> Result<Client> {
    apply_proxy(
        Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .pool_idle_timeout(Duration::from_secs(60))
            .tcp_keepalive(Duration::from_secs(30))
            .user_agent(concat!("SightOCR-Rust/", env!("CARGO_PKG_VERSION"))),
        proxy,
    )?
    .build()
    .map_err(|_| ServiceError::Network("无法初始化安全网络连接或代理").into())
}

pub(crate) fn apply_proxy(builder: ClientBuilder, config: &ProxyConfig) -> Result<ClientBuilder> {
    match config.mode {
        // reqwest's system-proxy feature uses environment settings first and the
        // Windows Internet Settings registry as fallback. It does not evaluate PAC.
        ProxyMode::System => Ok(builder),
        ProxyMode::Direct => Ok(builder.no_proxy()),
        ProxyMode::Manual => {
            let url = config.manual_url()?;
            let mut proxy = reqwest::Proxy::all(url.as_str())
                .map_err(|_| ServiceError::InvalidInput("无法解析手动代理配置"))?;
            if !config.username.is_empty() || !config.password.is_empty() {
                proxy = proxy.basic_auth(&config.username, &config.password);
            }
            // The explicit proxy applies to every service and does not inherit
            // environment bypass rules or a second fallback proxy.
            Ok(builder.no_proxy().proxy(proxy))
        }
    }
}

pub struct Services {
    client: Client,
    proxy: ProxyConfig,
    baidu_token: Option<Token>,
    bing_session: Option<BingSession>,
}

impl Services {
    pub fn new() -> Result<Self> {
        let proxy = ProxyConfig::default();
        let client = build_client(&proxy)?;
        Ok(Self {
            client,
            proxy,
            baidu_token: None,
            bing_session: None,
        })
    }

    fn configure_proxy(&mut self, proxy: &ProxyConfig) -> Result<()> {
        if &self.proxy == proxy {
            return Ok(());
        }
        // Build first: invalid settings must fail this task instead of silently
        // using the previous proxy or falling back to a direct connection.
        let client = build_client(proxy)?;
        self.client = client;
        self.proxy = proxy.clone();
        self.baidu_token = None;
        self.bing_session = None;
        Ok(())
    }

    pub fn ocr(&mut self, png: &[u8], selection: &str, config: &Config) -> Result<String> {
        self.ocr_cancellable(png, selection, config, &|| false)
    }

    pub fn ocr_cancellable(
        &mut self,
        png: &[u8],
        selection: &str,
        config: &Config,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<String> {
        anyhow::ensure!(!cancelled(), "任务已取消");
        self.configure_proxy(&config.proxy)?;
        if png.is_empty() {
            return Err(ServiceError::InvalidInput("待识别图片为空").into());
        }
        if ["Mistral_", "OpenAI_", "Nvidia_"]
            .iter()
            .any(|prefix| selection.starts_with(prefix))
        {
            return self.ai_ocr(png, selection, config, cancelled);
        }
        if let Some(mode) = selection.strip_prefix("Baidu_") {
            require_credentials(&config.api_key, &config.secret_key, "百度 OCR")?;
            if png.len() > 3 * 1024 * 1024 {
                return Err(
                    ServiceError::InvalidInput("百度 OCR 图片超过 3 MiB，请缩小截图范围").into(),
                );
            }
            let endpoint = match mode {
                "auto" | "accurate_basic" => "accurate_basic",
                "accurate" => "accurate",
                "general_basic" => "general_basic",
                "general" => "general",
                "table" => "table",
                "formula" => "formula",
                _ => return Err(ServiceError::InvalidInput("不支持的百度 OCR 模式").into()),
            };
            let image = base64::engine::general_purpose::STANDARD.encode(png);
            let mut form = vec![("image", image.as_str()), ("detect_direction", "true")];
            if endpoint == "formula" {
                form.push(("disp_formula", "true"));
            } else if endpoint != "table" {
                form.extend([
                    ("probability", "false"),
                    ("language_type", "auto_detect"),
                    ("paragraph", "false"),
                ]);
            }
            // One token refresh on an explicit expiration error; never switch providers.
            for attempt in 0..2 {
                let token = self.baidu_access_token(config)?;
                let response = send(
                    self.client
                        .post(format!(
                            "https://aip.baidubce.com/rest/2.0/ocr/v1/{endpoint}"
                        ))
                        .query(&[("access_token", token.as_str())])
                        .form(&form),
                )?;
                let value = read_json(response, "百度 OCR")?;
                if matches!(numeric_code(&value["error_code"]), Some(110 | 111)) {
                    self.baidu_token = None;
                    if attempt == 0 {
                        continue;
                    }
                }
                return parse_baidu_ocr(&value, endpoint);
            }
            unreachable!("the second attempt always returns")
        } else if let Some(mode) = selection.strip_prefix("Tencent_") {
            require_credentials(
                &config.tencent_secret_id,
                &config.tencent_secret_key,
                "腾讯 OCR",
            )?;
            if png.len() > 5 * 1024 * 1024 {
                return Err(
                    ServiceError::InvalidInput("腾讯 OCR 图片超过 5 MiB，请缩小截图范围").into(),
                );
            }
            let action = match mode {
                "auto" | "general_accurate" => "GeneralAccurateOCR",
                "general_basic" => "GeneralBasicOCR",
                "table" => "RecognizeTableAccurateOCR",
                "formula" => "RecognizeFormulaOCR",
                _ => return Err(ServiceError::InvalidInput("不支持的腾讯 OCR 模式").into()),
            };
            let body =
                json!({"ImageBase64": base64::engine::general_purpose::STANDARD.encode(png)});
            let value = self.tencent_request(
                &config.tencent_secret_id,
                &config.tencent_secret_key,
                "ocr",
                action,
                &body,
            )?;
            parse_tencent_ocr(&value, mode)
        } else {
            Err(ServiceError::InvalidInput("本地 OCR 应由本地引擎处理，未发送云端请求").into())
        }
    }

    pub fn translate(&mut self, text: &str, config: &Config) -> Result<String> {
        self.translate_cancellable(text, config, &|| false)
    }

    pub fn translate_cancellable(
        &mut self,
        text: &str,
        config: &Config,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<String> {
        anyhow::ensure!(!cancelled(), "任务已取消");
        self.configure_proxy(&config.proxy)?;
        if text.trim().is_empty() {
            return Err(ServiceError::InvalidInput("待翻译文本为空").into());
        }
        let source = normalize_language(&config.source_lang)?;
        let target = normalize_language(&config.target_lang)?;
        if target == "auto" {
            return Err(ServiceError::InvalidInput("请选择明确的目标语言").into());
        }
        // Preserve the chosen target language. The Python version silently reversed it.
        if source == target {
            return Ok(text.to_owned());
        }
        let translated = match config.last_translate_selection.as_str() {
            "Baidu" => self.baidu_translate(text, source, target, config),
            "Tencent" => self.tencent_translate(text, source, target, config),
            "OpenAI" | "Nvidia" => self.ai_translate(text, source, target, config, cancelled),
            "默认" | "Bing" | "Edge" => self.bing_translate(text, source, target, cancelled),
            _ => Err(ServiceError::InvalidInput("不支持的翻译服务").into()),
        }?;
        anyhow::ensure!(!cancelled(), "任务已取消");
        if translated.trim().is_empty() {
            return Err(ServiceError::InvalidResponse("翻译结果为空").into());
        }
        Ok(translated)
    }

    fn baidu_access_token(&mut self, config: &Config) -> Result<String> {
        let credentials = credential_fingerprint(&config.api_key, &config.secret_key);
        if let Some(token) = &self.baidu_token {
            if token.credentials == credentials && token.expires > Instant::now() {
                return Ok(token.value.clone());
            }
        }
        self.baidu_token = None;
        // Credentials belong in the POST body, never in a URL that a proxy may log.
        let response = send(
            self.client
                .post("https://aip.baidubce.com/oauth/2.0/token")
                .timeout(Duration::from_secs(12))
                .form(&[
                    ("grant_type", "client_credentials"),
                    ("client_id", config.api_key.as_str()),
                    ("client_secret", config.secret_key.as_str()),
                ]),
        )?;
        let value = read_json(response, "百度授权")?;
        if let Some(error) = value.get("error") {
            return Err(provider_error("百度授权", error).into());
        }
        let token = required_str(&value, "access_token")?.to_owned();
        if token.is_empty() || token.len() > 16_384 {
            return Err(ServiceError::InvalidResponse("访问令牌为空或过长").into());
        }
        let lifetime = numeric_code(&value["expires_in"])
            .unwrap_or(1800)
            .clamp(1, 30 * 24 * 3600);
        self.baidu_token = Some(Token {
            credentials,
            value: token.clone(),
            expires: Instant::now() + Duration::from_secs(lifetime.saturating_sub(60).max(1)),
        });
        Ok(token)
    }

    fn baidu_translate(
        &self,
        text: &str,
        source: &str,
        target: &str,
        config: &Config,
    ) -> Result<String> {
        require_credentials(
            &config.baidu_trans_appid,
            &config.baidu_trans_appkey,
            "百度翻译",
        )?;
        if text.len() > 6000 {
            return Err(
                ServiceError::InvalidInput("百度翻译单次文本超过 6000 字节，请分段翻译").into(),
            );
        }
        let salt = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("系统时间无效")?
            .as_nanos()
            .to_string();
        let signature = baidu_signature(
            &config.baidu_trans_appid,
            text,
            &salt,
            &config.baidu_trans_appkey,
        );
        let source = baidu_language(source);
        let target = baidu_language(target);
        let form = [
            ("appid", config.baidu_trans_appid.as_str()),
            ("q", text),
            ("from", source),
            ("to", target),
            ("salt", salt.as_str()),
            ("sign", signature.as_str()),
        ];
        let value = read_json(
            send(
                self.client
                    .post("https://api.fanyi.baidu.com/api/trans/vip/translate")
                    .form(&form),
            )?,
            "百度翻译",
        )?;
        parse_baidu_translation(&value)
    }

    fn tencent_translate(
        &self,
        text: &str,
        source: &str,
        target: &str,
        config: &Config,
    ) -> Result<String> {
        require_credentials(
            &config.tencent_trans_secret_id,
            &config.tencent_trans_secret_key,
            "腾讯翻译",
        )?;
        if text.chars().count() > 2000 || text.len() > 6000 {
            return Err(ServiceError::InvalidInput(
                "腾讯翻译单次文本超过 2000 字符或 6000 字节，请分段翻译",
            )
            .into());
        }
        let body = json!({"SourceText": text, "Source": tencent_language(source)?,
            "Target": tencent_language(target)?, "ProjectId": 0});
        let value = self.tencent_request(
            &config.tencent_trans_secret_id,
            &config.tencent_trans_secret_key,
            "tmt",
            "TextTranslate",
            &body,
        )?;
        Ok(required_str(tencent_response(&value)?, "TargetText")?.to_owned())
    }

    fn tencent_request(
        &self,
        secret_id: &str,
        secret_key: &str,
        service: &str,
        action: &str,
        body: &Value,
    ) -> Result<Value> {
        let (host, version) = match service {
            "ocr" => ("ocr.tencentcloudapi.com", "2018-11-19"),
            "tmt" => ("tmt.tencentcloudapi.com", "2018-03-21"),
            _ => return Err(ServiceError::InvalidInput("不支持的腾讯云服务").into()),
        };
        let payload = serde_json::to_string(body)?;
        let timestamp = Utc::now().timestamp();
        let authorization = tencent_authorization(
            secret_id, secret_key, service, action, host, &payload, timestamp,
        )?;
        let request = self
            .client
            .post(format!("https://{host}/"))
            .header("Authorization", authorization)
            .header("Content-Type", CONTENT_TYPE)
            .header("Host", host)
            .header("X-TC-Action", action)
            .header("X-TC-Version", version)
            .header("X-TC-Timestamp", timestamp.to_string());
        let request = if service == "tmt" {
            request.header("X-TC-Region", "ap-guangzhou")
        } else {
            request
        };
        read_json(send(request.body(payload))?, "腾讯云")
    }

    fn bing_translate(
        &mut self,
        text: &str,
        source: &str,
        target: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<String> {
        if text.chars().count() > 5000 {
            return Err(
                ServiceError::InvalidInput("默认翻译单次文本超过 5000 字符，请分段翻译").into(),
            );
        }
        let source = edge_language(source);
        let target = edge_language(target);
        let source = if source == "auto" {
            "auto-detect"
        } else {
            source
        };
        // The public Bing UI currently limits requests to 1000 UTF-16 units.
        // Preserve the existing 5000-character app limit through bounded chunks.
        translate_bing_chunks(text, cancelled, |chunk| {
            self.bing_translate_chunk(chunk, source, target, cancelled)
        })
    }

    fn bing_translate_chunk(
        &mut self,
        text: &str,
        source: &str,
        target: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<String> {
        for attempt in 0..2 {
            anyhow::ensure!(!cancelled(), "任务已取消");
            self.ensure_bing_session()?;
            // Session initialization may block. Recheck before sending any text.
            anyhow::ensure!(!cancelled(), "任务已取消");
            let session = self.bing_session.as_mut().expect("session was initialized");
            session.requests += 1;
            let iid = format!("{}.{}", session.iid, session.requests);
            let response = send(
                self.client
                    .post("https://www.bing.com/ttranslatev3")
                    .query(&[
                        ("isVertical", "1"),
                        ("IG", session.ig.as_str()),
                        ("IID", iid.as_str()),
                    ])
                    .header(reqwest::header::REFERER, "https://www.bing.com/translator")
                    .header(reqwest::header::COOKIE, &session.cookies)
                    .form(&[
                        ("fromLang", source),
                        ("to", target),
                        ("text", text),
                        ("token", session.token.as_str()),
                        ("key", session.key.as_str()),
                    ]),
            )?;
            if matches!(response.status().as_u16(), 401 | 403) {
                self.bing_session = None;
                if attempt == 0 {
                    continue;
                }
            }
            let value = read_json(response, "默认翻译")?;
            // Bing can return an authorization error inside an HTTP 200 body.
            if matches!(numeric_code(&value["statusCode"]), Some(401 | 403)) {
                self.bing_session = None;
                if attempt == 0 {
                    continue;
                }
            }
            return parse_edge_translation(&value);
        }
        unreachable!("the second attempt always returns")
    }

    fn ensure_bing_session(&mut self) -> Result<()> {
        if self
            .bing_session
            .as_ref()
            .is_some_and(|session| session.expires > Instant::now())
        {
            return Ok(());
        }
        self.bing_session = None;
        // The old edge.microsoft.com/translate/auth endpoint currently returns
        // HTTP 404. Use the session published by Microsoft's own translator
        // page. This compatibility endpoint is not a guaranteed public API.
        // All text stays with Microsoft; redirects remain disabled globally.
        let response = send(
            self.client
                .get("https://www.bing.com/translator")
                .header(reqwest::header::CACHE_CONTROL, "no-cache")
                .timeout(Duration::from_secs(15)),
        )?;
        let mut cookies = Vec::new();
        for value in response.headers().get_all(reqwest::header::SET_COOKIE) {
            if let Ok(value) = value.to_str() {
                if let Some(pair) = value.split(';').next().filter(|pair| pair.contains('=')) {
                    cookies.push(pair.to_owned());
                }
            }
        }
        let cookies = cookies.join("; ");
        if cookies.len() > 16_384 {
            return Err(ServiceError::InvalidResponse("翻译会话 Cookie 过长").into());
        }
        let bytes = read_response(response, "默认翻译授权", 2 * 1024 * 1024)?;
        let html = std::str::from_utf8(&bytes)
            .map_err(|_| ServiceError::InvalidResponse("翻译页面不是 UTF-8"))?;
        let mut session = parse_bing_session(html)?;
        session.cookies = cookies;
        self.bing_session = Some(session);
        Ok(())
    }
}

/// Parse data, never execute scripts from the translator page. Response content
/// and session tokens must not appear in parse errors or application logs.
fn parse_bing_session(html: &str) -> Result<BingSession> {
    let invalid = || {
        ServiceError::InvalidResponse(
            "微软翻译页面会话格式已变更，请稍后重试或手动选择其他翻译服务",
        )
    };
    let array = html
        .match_indices("params_AbusePreventionHelper")
        .find_map(|(offset, marker)| html[offset + marker.len()..].trim_start().strip_prefix('='))
        .ok_or_else(invalid)?;
    // Bound the isolated script expression even when an unexpected page is returned.
    let expression = array.trim_start().chars().take(8192).collect::<String>();
    let params = serde_json::Deserializer::from_str(&expression)
        .into_iter::<Value>()
        .next()
        .and_then(std::result::Result::ok)
        .ok_or_else(invalid)?;
    let params = params
        .as_array()
        .filter(|params| params.len() >= 2)
        .ok_or_else(invalid)?;
    let key = params[0]
        .as_u64()
        .filter(|&key| key > 0)
        .ok_or_else(invalid)?
        .to_string();
    let token = params[1]
        .as_str()
        .filter(|token| {
            !token.is_empty() && token.len() <= 4096 && token.bytes().all(|c| c.is_ascii_graphic())
        })
        .ok_or_else(invalid)?
        .to_owned();
    let lifetime = params
        .get(2)
        .and_then(Value::as_u64)
        .unwrap_or(600_000)
        .clamp(1000, 15 * 60 * 1000)
        .saturating_sub(30_000)
        .max(1000);
    let ig = html
        .match_indices("IG:")
        .find_map(|(offset, marker)| {
            let tail = html[offset + marker.len()..].trim_start();
            let quoted = tail.strip_prefix('"')?.split('"').next()?;
            (quoted.len() >= 16
                && quoted.len() <= 64
                && quoted.bytes().all(|c| c.is_ascii_hexdigit()))
            .then(|| quoted.to_owned())
        })
        .ok_or_else(invalid)?;
    let iid = html
        .match_indices("data-iid=\"")
        .find_map(|(offset, marker)| {
            let value = html[offset + marker.len()..].split('"').next()?;
            (value.starts_with("translator.")
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-')))
            .then(|| value.to_owned())
        })
        .ok_or_else(invalid)?;
    Ok(BingSession {
        key,
        token,
        ig,
        iid,
        cookies: String::new(),
        expires: Instant::now() + Duration::from_millis(lifetime),
        requests: 0,
    })
}

fn bing_chunks(mut text: &str) -> Vec<&str> {
    let mut chunks = Vec::new();
    while !text.is_empty() {
        let (mut units, mut cut, mut whitespace) = (0, text.len(), 0);
        for (offset, ch) in text.char_indices() {
            if units + ch.len_utf16() > 1000 {
                cut = offset;
                break;
            }
            units += ch.len_utf16();
            if ch.is_whitespace() && units >= 500 {
                whitespace = offset + ch.len_utf8();
            }
        }
        if cut < text.len() && whitespace > 0 {
            cut = whitespace;
        }
        let (chunk, remainder) = text.split_at(cut);
        // Pure whitespace does not need a remote request.
        if !chunk.trim().is_empty() {
            chunks.push(chunk);
        }
        text = remainder;
    }
    chunks
}

fn translate_bing_chunks(
    text: &str,
    cancelled: &dyn Fn() -> bool,
    mut translate: impl FnMut(&str) -> Result<String>,
) -> Result<String> {
    let mut translated = Vec::new();
    for chunk in bing_chunks(text) {
        anyhow::ensure!(!cancelled(), "任务已取消");
        translated.push(translate(chunk)?);
    }
    anyhow::ensure!(!cancelled(), "任务已取消");
    Ok(translated.join("\n"))
}

fn require_credentials(id: &str, secret: &str, provider: &'static str) -> Result<()> {
    if id.trim().is_empty() || secret.trim().is_empty() {
        return Err(ServiceError::Credentials(provider).into());
    }
    Ok(())
}

fn credential_fingerprint(id: &str, secret: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update((id.len() as u64).to_be_bytes());
    hash.update(id.as_bytes());
    hash.update(secret.as_bytes());
    hash.finalize().into()
}

fn send(request: RequestBuilder) -> Result<Response> {
    // Do not attach reqwest::Error as a cause: its URL can contain an access token.
    request.send().map_err(|error| {
        if error.is_timeout() {
            ServiceError::Network("网络请求超时，请稍后重试")
        } else {
            ServiceError::Network("安全网络连接失败，请检查网络、代理或证书")
        }
        .into()
    })
}

fn read_response(response: Response, provider: &'static str, limit: usize) -> Result<Vec<u8>> {
    if !response.status().is_success() {
        return Err(ServiceError::Http {
            provider,
            status: response.status().as_u16(),
        }
        .into());
    }
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err(ServiceError::InvalidResponse("响应体超过大小限制").into());
    }
    let mut bytes = Vec::new();
    response
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ServiceError::Network("读取服务响应失败或超时"))?;
    if bytes.len() > limit {
        return Err(ServiceError::InvalidResponse("响应体超过大小限制").into());
    }
    Ok(bytes)
}

fn read_json(response: Response, provider: &'static str) -> Result<Value> {
    let bytes = read_response(response, provider, MAX_RESPONSE_BYTES)?;
    serde_json::from_slice(&bytes)
        .map_err(|_| ServiceError::InvalidResponse("无法解析 JSON").into())
}

fn required_str<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .context(ServiceError::InvalidResponse("缺少文本字段"))
}

fn numeric_code(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

fn provider_error(provider: &'static str, value: &Value) -> ServiceError {
    let code = if let Some(number) = value.as_u64() {
        number.to_string()
    } else if let Some(code) = value.as_str().filter(|code| {
        !code.is_empty()
            && code.len() <= 96
            && code
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
    }) {
        code.to_owned()
    } else {
        "Unknown".into()
    };
    ServiceError::Provider { provider, code }
}

fn baidu_response(value: &Value, provider: &'static str) -> Result<()> {
    if let Some(code) = value.get("error_code") {
        if numeric_code(code) != Some(0) {
            return Err(provider_error(provider, code).into());
        }
    }
    Ok(())
}

fn tencent_response(value: &Value) -> Result<&Value> {
    let response = value
        .get("Response")
        .filter(|v| v.is_object())
        .context(ServiceError::InvalidResponse("缺少腾讯云 Response"))?;
    if let Some(error) = response.get("Error") {
        return Err(provider_error("腾讯云", &error["Code"]).into());
    }
    Ok(response)
}

fn joined_field(value: &Value, array: &str, field: &str, separator: &str) -> Result<String> {
    let items = value
        .get(array)
        .and_then(Value::as_array)
        .context(ServiceError::InvalidResponse("缺少识别结果数组"))?;
    items
        .iter()
        .map(|item| required_str(item, field))
        .collect::<Result<Vec<_>>>()
        .map(|lines| lines.join(separator))
}

fn formula_field(value: &Value, array: &str, field: &str) -> Result<String> {
    let items = value
        .get(array)
        .and_then(Value::as_array)
        .context(ServiceError::InvalidResponse("缺少识别结果数组"))?;
    let mut formulas = Vec::new();
    for item in items {
        let text = required_str(item, field)?.trim();
        // Match Mistral's display-math text while keeping the recognized LaTeX
        // intact. Each provider result is one formula, including multiline TeX.
        let latex = [("\\[", "\\]"), ("\\(", "\\)"), ("$$", "$$"), ("$", "$")]
            .iter()
            .find_map(|(open, close)| text.strip_prefix(open)?.strip_suffix(close))
            .unwrap_or(text)
            .trim();
        if !latex.is_empty() {
            formulas.push(format!("\\[\n{latex}\n\\]"));
        }
    }
    Ok(formulas.join("\n\n"))
}

fn parse_baidu_ocr(value: &Value, mode: &str) -> Result<String> {
    baidu_response(value, "百度 OCR")?;
    match mode {
        "table" => parse_tables(
            value,
            "tables_result",
            "body",
            ["row_start", "col_start", "row_end", "col_end", "words"],
        ),
        "formula" => formula_field(value, "formula_result", "words"),
        _ => joined_field(value, "words_result", "words", "\n"),
    }
}

fn parse_tencent_ocr(value: &Value, mode: &str) -> Result<String> {
    let response = tencent_response(value)?;
    match mode {
        "table" => parse_tables(
            response,
            "TableDetections",
            "Cells",
            ["RowTl", "ColTl", "RowBr", "ColBr", "Text"],
        ),
        "formula" => formula_field(response, "FormulaInfoList", "DetectedText"),
        _ => joined_field(response, "TextDetections", "DetectedText", "\n"),
    }
}

fn parse_tables(value: &Value, array: &str, cells_key: &str, keys: [&str; 5]) -> Result<String> {
    let tables = value
        .get(array)
        .and_then(Value::as_array)
        .context(ServiceError::InvalidResponse("缺少表格数组"))?;
    if tables.len() > MAX_TABLES {
        return Err(ServiceError::InvalidResponse("表格数量超过限制").into());
    }
    let mut output = Vec::new();
    let mut total_cells = 0usize;
    for table in tables {
        let cells_value = table
            .get(cells_key)
            .context(ServiceError::InvalidResponse("缺少单元格数组"))?;
        // Tencent explicitly permits null when no cells were detected.
        if cells_value.is_null() {
            continue;
        }
        let cells = cells_value
            .as_array()
            .context(ServiceError::InvalidResponse("单元格格式无效"))?;
        if cells.is_empty() {
            continue;
        }
        if cells.len() > MAX_TABLE_CELLS {
            return Err(ServiceError::InvalidResponse("单元格数量超过限制").into());
        }
        let mut parsed = Vec::with_capacity(cells.len());
        let (mut rows, mut cols) = (0usize, 0usize);
        for cell in cells {
            let mut indices = [0; 4];
            for (index, key) in keys[..4].iter().enumerate() {
                indices[index] = cell
                    .get(*key)
                    .and_then(Value::as_u64)
                    .and_then(|n| usize::try_from(n).ok())
                    .context(ServiceError::InvalidResponse("单元格索引无效"))?;
            }
            let [row, col, row_end, col_end] = indices;
            if row >= row_end
                || col >= col_end
                || row_end > MAX_TABLE_ROWS
                || col_end > MAX_TABLE_COLS
            {
                return Err(ServiceError::InvalidResponse("表格行列范围无效或过大").into());
            }
            rows = rows.max(row_end);
            cols = cols.max(col_end);
            parsed.push((row, col, required_str(cell, keys[4])?));
        }
        // Both providers number grid boundaries, so end indices are exclusive.
        let size = rows
            .checked_mul(cols)
            .context(ServiceError::InvalidResponse("表格尺寸溢出"))?;
        total_cells = total_cells
            .checked_add(size)
            .context(ServiceError::InvalidResponse("表格总尺寸溢出"))?;
        if total_cells > MAX_TABLE_CELLS {
            return Err(ServiceError::InvalidResponse("表格总尺寸超过限制").into());
        }
        let mut grid = vec![String::new(); size];
        for (row, col, text) in parsed {
            let destination = &mut grid[row * cols + col];
            if !destination.is_empty() && !text.is_empty() {
                destination.push(' ');
            }
            destination.push_str(&text.replace(['\r', '\n', '\t'], " "));
        }
        output.push(
            grid.chunks(cols)
                .map(|row| row.join("\t"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    Ok(output.join("\n\n"))
}

fn parse_baidu_translation(value: &Value) -> Result<String> {
    baidu_response(value, "百度翻译")?;
    joined_field(value, "trans_result", "dst", "\n")
}

fn parse_edge_translation(value: &Value) -> Result<String> {
    if numeric_code(&value["statusCode"]).is_some_and(|code| code >= 400) {
        return Err(provider_error("默认翻译", &value["statusCode"]).into());
    }
    if let Some(error) = value.get("error") {
        return Err(provider_error("默认翻译", &error["code"]).into());
    }
    value
        .as_array()
        .and_then(|items| items.first())
        .and_then(|item| item["translations"].as_array())
        .and_then(|items| items.first())
        .and_then(|item| item["text"].as_str())
        .map(str::to_owned)
        .context(ServiceError::InvalidResponse("缺少翻译文本"))
}

fn baidu_language(language: &str) -> &str {
    match language {
        "zh-Hans" => "zh",
        "zh-Hant" => "cht",
        "ja" => "jp",
        "ko" => "kor",
        "fr" => "fra",
        "es" => "spa",
        "pt-PT" => "pt",
        "vi" => "vie",
        "ms" => "may",
        "ar" => "ara",
        "km" => "hkm",
        "nb" => "nob",
        "nn" => "nno",
        "fa" => "per",
        "sv" => "swe",
        "uk" => "ukr",
        value => value,
    }
}

fn tencent_language(language: &str) -> Result<&str> {
    match language {
        "zh-Hans" => Ok("zh"),
        "zh-Hant" => Ok("zh-TW"),
        "pt-PT" => Ok("pt"),
        "auto" | "en" | "ja" | "ko" | "fr" | "es" | "it" | "de" | "tr" | "ru" | "pt" | "vi"
        | "id" | "th" | "ms" | "ar" | "hi" => Ok(language),
        _ => Err(ServiceError::InvalidInput("腾讯翻译不支持所选语言，请手动选择其他服务").into()),
    }
}

fn edge_language(language: &str) -> &str {
    if language == "nn" {
        "nb"
    } else {
        language
    }
}

fn baidu_signature(id: &str, text: &str, salt: &str, secret: &str) -> String {
    let mut hash = Md5::new();
    for part in [id, text, salt, secret] {
        hash.update(part.as_bytes());
    }
    format!("{:x}", hash.finalize())
}

fn hmac_sha256(key: &[u8], message: &str) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts keys of any length");
    mac.update(message.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

// TC3 reference: https://cloud.tencent.com/document/api/213/30654
fn tencent_authorization(
    id: &str,
    secret: &str,
    service: &str,
    action: &str,
    host: &str,
    payload: &str,
    timestamp: i64,
) -> Result<String> {
    let date = Utc
        .timestamp_opt(timestamp, 0)
        .single()
        .context("系统时间超出有效范围")?
        .format("%Y-%m-%d")
        .to_string();
    let signed_headers = "content-type;host;x-tc-action";
    let canonical_headers = format!(
        "content-type:{CONTENT_TYPE}\nhost:{host}\nx-tc-action:{}\n",
        action.to_ascii_lowercase()
    );
    let payload_hash = format!("{:x}", Sha256::digest(payload.as_bytes()));
    let canonical_request =
        format!("POST\n/\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}");
    let scope = format!("{date}/{service}/tc3_request");
    let to_sign = format!(
        "TC3-HMAC-SHA256\n{timestamp}\n{scope}\n{:x}",
        Sha256::digest(canonical_request.as_bytes())
    );
    let date_key = hmac_sha256(format!("TC3{secret}").as_bytes(), &date);
    let service_key = hmac_sha256(&date_key, service);
    let signing_key = hmac_sha256(&service_key, "tc3_request");
    let signature = hmac_sha256(&signing_key, &to_sign)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!("TC3-HMAC-SHA256 Credential={id}/{scope}, SignedHeaders={signed_headers}, Signature={signature}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formula_ocr_matches_mistral_display_math_and_keeps_all_formulas() -> Result<()> {
        let derivative = r"\frac { d } { d x } \left ( x ^ { n } \right) = n x ^ { n - 1 }";
        let multiline = "\\begin{aligned}\na &= b \\\\\nc &= d\n\\end{aligned}";
        let expected = format!("\\[\n{derivative}\n\\]\n\n\\[\n{multiline}\n\\]");
        let baidu = json!({"formula_result":[{"words":derivative},{"words":multiline}]});
        let tencent = json!({"Response":{"FormulaInfoList":[
            {"DetectedText":derivative},{"DetectedText":multiline}
        ]}});
        assert_eq!(parse_baidu_ocr(&baidu, "formula")?, expected);
        assert_eq!(parse_tencent_ocr(&tencent, "formula")?, expected);
        Ok(())
    }

    #[test]
    fn formula_ocr_normalizes_existing_math_delimiters_without_nesting() -> Result<()> {
        for text in [
            " \\[ x ^ 2 \\] ",
            "\\[\nx ^ 2\n\\]",
            r"\(x ^ 2\)",
            "$$x ^ 2$$",
            "$x ^ 2$",
        ] {
            let baidu = json!({"formula_result":[{"words":text}]});
            let tencent = json!({"Response":{"FormulaInfoList":[{"DetectedText":text}]}});
            assert_eq!(parse_baidu_ocr(&baidu, "formula")?, "\\[\nx ^ 2\n\\]");
            assert_eq!(parse_tencent_ocr(&tencent, "formula")?, "\\[\nx ^ 2\n\\]");
        }
        Ok(())
    }

    #[test]
    fn formula_ocr_preserves_empty_results_and_rejects_invalid_fields() -> Result<()> {
        for entries in [json!([]), json!([{"words":" \n "},{"words":r"\[\]"}])] {
            assert_eq!(
                parse_baidu_ocr(&json!({"formula_result":entries}), "formula")?,
                ""
            );
        }
        assert_eq!(
            parse_tencent_ocr(&json!({"Response":{"FormulaInfoList":[]}}), "formula")?,
            ""
        );
        assert!(parse_baidu_ocr(&json!({"formula_result":[{"words":null}]}), "formula").is_err());
        assert!(
            parse_tencent_ocr(&json!({"Response":{"FormulaInfoList":[{}]}}), "formula").is_err()
        );
        Ok(())
    }

    #[test]
    fn text_ocr_does_not_acquire_formula_delimiters() -> Result<()> {
        assert_eq!(
            parse_baidu_ocr(&json!({"words_result":[{"words":"안녕하세요"}]}), "general")?,
            "안녕하세요"
        );
        assert_eq!(
            parse_tencent_ocr(
                &json!({"Response":{"TextDetections":[{"DetectedText":"x ^ 2"}]}}),
                "general_basic"
            )?,
            "x ^ 2"
        );
        Ok(())
    }

    #[test]
    fn tables_use_exclusive_end_indices_and_preserve_blank_columns() -> Result<()> {
        let value = json!({"tables_result":[{"body":[
            {"row_start":0,"row_end":1,"col_start":0,"col_end":2,"words":"标题"},
            {"row_start":1,"row_end":2,"col_start":0,"col_end":1,"words":"A\nB"},
            {"row_start":1,"row_end":2,"col_start":1,"col_end":2,"words":"C"}
        ]}]});
        assert_eq!(parse_baidu_ocr(&value, "table")?, "标题\t\nA B\tC");
        let value = json!({"Response":{"TableDetections":[{"Cells":[
            {"RowTl":0,"RowBr":1,"ColTl":0,"ColBr":1,"Text":"唯一单元格"}
        ]}]}});
        assert_eq!(parse_tencent_ocr(&value, "table")?, "唯一单元格");
        Ok(())
    }

    #[test]
    fn malicious_and_negative_table_dimensions_return_errors() {
        for end in [json!(u64::MAX), json!(-1), json!(0), json!(10001)] {
            let value = json!({"Response":{"TableDetections":[{"Cells":[
                {"RowTl":0,"RowBr":end,"ColTl":0,"ColBr":1,"Text":"x"}
            ]}]}});
            assert!(parse_tencent_ocr(&value, "table").is_err());
        }
    }

    #[test]
    fn all_translated_paragraphs_are_retained() -> Result<()> {
        let value = json!({"trans_result":[{"dst":"第一段"},{"dst":"第二段"}]});
        assert_eq!(parse_baidu_translation(&value)?, "第一段\n第二段");
        assert_eq!(
            parse_edge_translation(&json!([{"translations":[{"text":"结果"}]}]))?,
            "结果"
        );
        Ok(())
    }

    #[test]
    fn bing_page_session_parser_accepts_spacing_and_ignores_unrelated_widgets() -> Result<()> {
        let html = r#"<script>var _G={IG: "0123456789ABCDEF0123456789ABCDEF"};
            var params_AbusePreventionHelper = [1780000000000,"synthetic-token",3600000];</script>
            <div data-iid="other.123"></div><div data-iid="translator.5023"></div>"#;
        let session = parse_bing_session(html)?;
        assert_eq!(session.key, "1780000000000");
        assert_eq!(session.token, "synthetic-token");
        assert_eq!(session.iid, "translator.5023");
        assert!(session.expires > Instant::now());
        assert!(session.expires <= Instant::now() + Duration::from_secs(15 * 60));
        Ok(())
    }

    #[test]
    fn bing_page_parser_rejects_changed_or_malformed_pages_without_echoing_tokens() {
        for html in [
            "<html>challenge</html>",
            r#"IG:"0123456789ABCDEF"; params_AbusePreventionHelper=[1,"private-token",1000]; data-iid="other.1""#,
            r#"IG:"0123456789ABCDEF"; params_AbusePreventionHelper=[-1,"private-token",1000]; data-iid="translator.1""#,
        ] {
            let error = parse_bing_session(html)
                .err()
                .expect("malformed session must fail");
            assert!(!error.to_string().contains("private-token"));
        }
        let error = parse_edge_translation(&json!({"statusCode":429,"message":"private text"}))
            .unwrap_err();
        assert!(error.to_string().contains("429"));
        assert!(!error.to_string().contains("private text"));
    }

    #[test]
    fn bing_chunks_keep_unicode_whole_and_respect_the_browser_limit() {
        for text in ["Hello".to_owned(), "测试🚀 ".repeat(400), "A".repeat(5000)] {
            let chunks = bing_chunks(&text);
            assert_eq!(chunks.concat(), text);
            assert!(chunks
                .iter()
                .all(|chunk| chunk.encode_utf16().count() <= 1000));
            assert!(chunks.iter().all(|chunk| !chunk.is_empty()));
        }
    }

    #[test]
    fn cancelled_bing_translation_does_not_submit_later_chunks() {
        let submitted = std::cell::Cell::new(0);
        let text = "A".repeat(3000);
        let result = translate_bing_chunks(&text, &|| submitted.get() > 0, |chunk| {
            submitted.set(submitted.get() + 1);
            Ok(chunk.to_owned())
        });
        assert_eq!(submitted.get(), 1);
        assert_eq!(result.unwrap_err().to_string(), "任务已取消");
    }

    #[test]
    fn identical_languages_return_input_without_initializing_bing() -> Result<()> {
        let mut services = Services::new()?;
        let config = Config {
            source_lang: "en".into(),
            target_lang: "en".into(),
            ..Config::default()
        };
        assert_eq!(services.translate("Hello", &config)?, "Hello");
        assert!(services.bing_session.is_none());
        Ok(())
    }

    #[test]
    fn provider_failures_are_errors_not_recognized_text() {
        let error = parse_baidu_ocr(
            &json!({"error_code":17,"error_msg":"private text"}),
            "general",
        )
        .unwrap_err();
        assert!(error.downcast_ref::<ServiceError>().is_some());
        assert!(!error.to_string().contains("private text"));
        assert!(parse_tencent_ocr(
            &json!({"Response":{"Error":{"Code":"AuthFailure.SignatureFailure"}}}),
            "table"
        )
        .is_err());
    }

    #[test]
    fn credential_cache_identity_changes_when_either_secret_changes() {
        assert_ne!(
            credential_fingerprint("a", "bc"),
            credential_fingerprint("ab", "c")
        );
        assert_ne!(
            credential_fingerprint("id", "old"),
            credential_fingerprint("id", "new")
        );
    }

    #[test]
    fn language_mappings_are_provider_specific() -> Result<()> {
        assert_eq!(baidu_language("ja"), "jp");
        assert_eq!(baidu_language("zh-Hant"), "cht");
        assert_eq!(tencent_language("zh-Hant")?, "zh-TW");
        assert!(tencent_language("mn-Mong").is_err());
        Ok(())
    }

    #[test]
    fn baidu_signs_unescaped_utf8_text() {
        // Independently generated using Python hashlib; values are fake fixtures.
        assert_eq!(
            baidu_signature("test-id", "你好 & hello", "12345", "test-key"),
            "be5b55a8ddf4ec7bc5cc662023887f61"
        );
    }

    #[test]
    fn tencent_signing_fixture_uses_utc_and_exact_payload() -> Result<()> {
        // Independently generated using Python hashlib/hmac, no real credentials.
        let auth = tencent_authorization(
            "test-id",
            "test-key",
            "ocr",
            "GeneralBasicOCR",
            "ocr.tencentcloudapi.com",
            r#"{"ImageBase64":"AQID"}"#,
            1551113065,
        )?;
        assert!(auth.contains("/2019-02-25/ocr/tc3_request"));
        assert!(auth.ends_with(
            "Signature=1fdf428d3f152bf9def6f6140a1c60e1897f7e301aec0f2d7227f41008fad44c"
        ));
        Ok(())
    }
}
