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
pub const PROVIDERS: &[&str] = &["bing", "baidu", "tencent", "openai", "nvidia"];

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

pub(crate) fn validate_provider(provider: &str) -> Result<&'static str> {
    Ok(match provider {
        "bing" => "Bing",
        "baidu" => "Baidu",
        "tencent" => "Tencent",
        "openai" => "OpenAI",
        "nvidia" => "Nvidia",
        _ => bail!("Unsupported provider; choose bing, baidu, tencent, openai, or nvidia"),
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

    pub(crate) fn ocr(&mut self, path: &Path, table: bool) -> Result<Value> {
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
            config.last_translate_selection = provider.into();
        }
        config.normalize().context("Invalid translation settings")?;
        let provider = match config.last_translate_selection.as_str() {
            "默认" | "Bing" | "Edge" => "bing",
            "Baidu" => "baidu",
            "Tencent" => "tencent",
            "OpenAI" => "openai",
            "Nvidia" => "nvidia",
            _ => bail!("Unsupported saved provider; set provider or update settings"),
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
