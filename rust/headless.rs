//! Shared MCP operations. Neither configuration migration nor UI setup runs here.
use crate::{
    config::{normalize_language, Config, LANGUAGES},
    worker::{self, Output, Request, Task, Worker},
};
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::mpsc::{self, Receiver},
    time::Duration,
};

pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
/// JSON Schema pattern for the `provider` argument and result.
pub const PROVIDER_PATTERN: &str = "^(bing|baidu|tencent|model:[a-z0-9-]{1,32})$";

pub(crate) fn validate_text(text: &str) -> Result<()> {
    ensure!(!text.trim().is_empty(), "Text must not be empty");
    ensure!(text.len() <= MAX_TEXT_BYTES, "Text exceeds the 1 MiB limit");
    ensure!(!text.contains('\0'), "Text must not contain NUL bytes");
    Ok(())
}

pub(crate) fn validate_language(language: &str, target: bool) -> Result<String> {
    let language =
        normalize_language(language).context("Unsupported language; call sightocr_languages")?;
    ensure!(
        !target || language != "auto",
        "Target language must not be auto"
    );
    Ok(language.to_owned())
}

/// Map an MCP provider name to a saved selection. Model interfaces use their id:
/// `model:{id}`. The former fixed names `openai`, `nvidia` and `generic` still work
/// because migration keeps those ids.
pub(crate) fn validate_provider(provider: &str) -> Result<String> {
    Ok(match provider {
        "bing" => "Bing".into(),
        "baidu" => "Baidu".into(),
        "tencent" => "Tencent".into(),
        "openai" | "nvidia" | "generic" => {
            format!("{}{provider}", crate::config::MODEL_SELECTION_PREFIX)
        }
        _ => {
            let id = provider
                .strip_prefix("model:")
                .filter(|id| {
                    (1..=32).contains(&id.len())
                        && id
                            .bytes()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
                })
                .context("Unsupported provider; choose bing, baidu, tencent, or model:<id>")?;
            format!("{}{id}", crate::config::MODEL_SELECTION_PREFIX)
        }
    })
}

pub(crate) fn languages() -> Value {
    json!({"languages": LANGUAGES.iter().map(|(code, name)| json!({
        "code": code, "name": name, "source": true, "target": *code != "auto"
    })).collect::<Vec<_>>()})
}

/// Native DLLs are isolated in the existing private worker process, so native
/// stdout cannot corrupt JSON-RPC. The process is lazy and is killed on timeout
/// or session teardown (after any serial call finishes).
#[derive(Default)]
pub(crate) struct Headless {
    worker: Option<Worker>,
    receiver: Option<Receiver<Output>>,
    next_id: u64,
}

impl Headless {
    fn execute(&mut self, config: Config, task: Task) -> Result<Output> {
        if self.worker.is_none() {
            let (sender, receiver) = mpsc::channel();
            self.worker = Some(Worker::start(worker::resources_dir(), move |output| {
                let _ = sender.send(output);
            })?);
            self.receiver = Some(receiver);
        }
        self.next_id = self
            .next_id
            .checked_add(1)
            .context("Task counter exhausted")?;
        let worker = self.worker.as_ref().context("Worker unavailable")?;
        worker.submit(Request {
            id: self.next_id,
            config,
            task,
        })?;
        let received = self
            .receiver
            .as_ref()
            .context("Worker output unavailable")?
            .recv_timeout(Duration::from_secs(120));
        let output = match received {
            Ok(output) => output,
            Err(_) => {
                worker.cancel();
                self.worker = None;
                self.receiver = None;
                bail!("Processing timed out or the worker disconnected; retry the operation");
            }
        };
        ensure!(output.id == self.next_id, "Unexpected worker response");
        if let Some(error) = &output.error {
            bail!("{error}");
        }
        Ok(output)
    }

    pub(crate) fn ocr(&mut self, path: &Path, table: bool, layout: bool) -> Result<Value> {
        let metadata = std::fs::metadata(path).context("Cannot open local image file")?;
        ensure!(
            metadata.is_file(),
            "Image path must be a regular local file"
        );
        ensure!(
            metadata.len() <= MAX_IMAGE_BYTES,
            "Image exceeds the 64 MiB file limit"
        );
        let config = Config {
            last_ocr_selection: if table { "默认_table" } else { "默认" }.into(),
            layout_analysis: layout,
            ..Config::default()
        };
        let output = self.execute(
            config,
            Task::Recognize {
                image: worker::load_image(path)?,
                translate: false,
            },
        )?;
        let text = output.recognized.context("OCR returned no output")?;
        Ok(json!({"text":text, "operation":"ocr", "provider":"local", "table":table}))
    }

    pub(crate) fn translate(
        &mut self,
        text: &str,
        source: Option<&str>,
        target: Option<&str>,
        provider: Option<&str>,
    ) -> Result<Value> {
        validate_text(text)?;
        let source = source
            .map(|value| validate_language(value, false))
            .transpose()?;
        let target = target
            .map(|value| validate_language(value, true))
            .transpose()?;
        let provider = provider.map(validate_provider).transpose()?;
        let mut config = Config::load_read_only()
            .context("Cannot read settings; check SIGHTOCR_CONFIG and configuration fields")?;
        if let Some(source) = source {
            config.source_lang = source;
        }
        if let Some(target) = target {
            config.target_lang = target;
        }
        if let Some(provider) = provider {
            config.last_translate_selection = provider;
        }
        let requested = config.last_translate_selection.clone();
        config.normalize().context("Invalid translation settings")?;
        // normalize() resets selections of missing interfaces; never fall back silently.
        ensure!(
            config.last_translate_selection == requested
                || crate::config::model_translator(&requested).is_none(),
            "The selected model interface does not exist or cannot translate; check SightOCR settings"
        );
        let provider = match config.last_translate_selection.as_str() {
            "默认" | "Bing" | "Edge" => "bing".to_owned(),
            "Baidu" => "baidu".into(),
            "Tencent" => "tencent".into(),
            selection => match crate::config::model_translator(selection) {
                Some(id) => format!("model:{id}"),
                None => bail!("Unsupported saved provider; set provider or update settings"),
            },
        };
        let source = config.source_lang.clone();
        let target = config.target_lang.clone();
        let output = self.execute(config, Task::Translate(text.into()))?;
        let text = output
            .translated
            .context("Translation returned no output")?;
        Ok(
            json!({"text":text, "operation":"translate", "provider":provider, "source_lang":source, "target_lang":target}),
        )
    }
}
