//! Shared headless commands. Neither configuration migration nor UI setup runs here.
use crate::{
    config::{normalize_language, Config, LANGUAGES},
    worker::{self, Output, Request, Task, Worker},
};
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    time::Duration,
};

pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
pub const PROVIDERS: &[&str] = &["bing", "baidu", "tencent", "openai", "nvidia"];

const HELP: &str = "Usage: sightocr-cli <command> [options]

Commands:
  ocr IMAGE [--table]                         Local OCR; never uploads the image
  translate [TEXT | --stdin | --input FILE]   Sends text to the selected provider
  languages                                  List supported language codes
  mcp                                        MCP JSON-RPC over UTF-8 stdio

Translation options:
  --from CODE       Source language (auto, en, zh-Hans, ...)
  --to CODE         Target language (must not be auto)
  --provider NAME   bing, baidu, tencent, openai, nvidia (one invocation only)

Output options (ocr, translate, languages):
  --format text|json    Default: text; --json is an alias for --format json
  --output FILE        Write UTF-8 output to a file instead of stdout

  -h, --help           Show this help
  -V, --version        Show version

Compatibility aliases: --ocr, --translate, --mcp.
Use -- before positional text that starts with a dash. A single '-' reads stdin.
Text input must be UTF-8 (optional BOM), non-empty, and at most 1 MiB.
Images must be local regular files, at most 64 MiB and 100 million pixels.
SIGHTOCR_CONFIG selects an existing settings file; commands never save or migrate it.
SIGHTOCR_RESOURCES selects the OneOCR model directory. Missing settings use defaults.
Translation uses saved provider/languages unless overridden; credentials come from settings.
Exit codes: 0 success; 2 invalid command arguments; 1 processing or I/O failure.
";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Version,
    Mcp,
    Languages,
    Ocr {
        image: PathBuf,
        table: bool,
    },
    Translate {
        input: TextInput,
        source: Option<String>,
        target: Option<String>,
        provider: Option<String>,
    },
}

#[derive(Debug, PartialEq, Eq)]
enum TextInput {
    Literal(String),
    Stdin,
    File(PathBuf),
}

#[derive(Debug)]
struct Options {
    command: Command,
    json: bool,
    output: Option<PathBuf>,
}

fn parse(args: &[String]) -> Result<Options> {
    let plain = |command| Options {
        command,
        json: false,
        output: None,
    };
    if args.is_empty() {
        return Ok(plain(Command::Help));
    }
    if args.len() == 1 {
        match args[0].as_str() {
            "help" | "-h" | "--help" => return Ok(plain(Command::Help)),
            "-V" | "--version" => return Ok(plain(Command::Version)),
            "mcp" | "--mcp" => return Ok(plain(Command::Mcp)),
            _ => {}
        }
    }
    let kind = match args[0].as_str() {
        "ocr" | "--ocr" => "ocr",
        "translate" | "--translate" => "translate",
        "languages" => "languages",
        _ => bail!("Unknown command or unexpected arguments; run --help"),
    };
    if args.len() == 2 && matches!(args[1].as_str(), "--help" | "-h") {
        return Ok(plain(Command::Help));
    }
    let mut json = false;
    let mut output = None;
    let mut source = None;
    let mut target = None;
    let mut provider = None;
    let mut input = None;
    let mut table = false;
    let mut seen = HashSet::new();
    let mut positional = Vec::new();
    let mut index = 1;
    let mut literal = false;
    while index < args.len() {
        let flag = args[index].as_str();
        index += 1;
        if !literal && flag == "--" {
            literal = true;
            continue;
        }
        if literal || !flag.starts_with('-') || flag == "-" {
            positional.push(flag.to_owned());
            continue;
        }
        let canonical = if flag == "--json" { "--format" } else { flag };
        ensure!(seen.insert(canonical), "Duplicate command option");
        match flag {
            "--table" if kind == "ocr" => table = true,
            "--stdin" if kind == "translate" => {
                ensure!(input.is_none(), "Choose only one text input");
                input = Some(TextInput::Stdin);
            }
            "--json" => json = true,
            "--output" | "--format" | "--input" | "--from" | "--to" | "--provider" => {
                ensure!(
                    matches!(flag, "--output" | "--format") || kind == "translate",
                    "Option is not valid for this command"
                );
                let value = args.get(index).context("Option requires a value")?;
                ensure!(
                    !value.is_empty() && !value.starts_with("--"),
                    "Option requires a value"
                );
                index += 1;
                match flag {
                    "--output" => output = Some(PathBuf::from(value)),
                    "--format" => {
                        json = match value.as_str() {
                            "text" => false,
                            "json" => true,
                            _ => bail!("Output format must be text or json"),
                        }
                    }
                    "--input" => {
                        ensure!(input.is_none(), "Choose only one text input");
                        input = Some(if value == "-" {
                            TextInput::Stdin
                        } else {
                            TextInput::File(value.into())
                        });
                    }
                    "--from" => source = Some(validate_language(value, false)?),
                    "--to" => target = Some(validate_language(value, true)?),
                    "--provider" => {
                        validate_provider(value)?;
                        provider = Some(value.clone());
                    }
                    _ => unreachable!(),
                }
            }
            _ => bail!("Unknown option; run --help"),
        }
    }
    let command = match kind {
        "ocr" => {
            ensure!(
                positional.len() == 1 && !positional[0].is_empty(),
                "ocr requires exactly one image path"
            );
            Command::Ocr {
                image: positional.remove(0).into(),
                table,
            }
        }
        "translate" => {
            ensure!(
                positional.len() <= 1 && (positional.is_empty() || input.is_none()),
                "Choose exactly one text input"
            );
            if let Some(text) = positional.pop() {
                input = Some(if text == "-" {
                    TextInput::Stdin
                } else {
                    TextInput::Literal(text)
                });
            }
            Command::Translate {
                input: input.context("translate requires TEXT, --stdin, or --input FILE")?,
                source,
                target,
                provider,
            }
        }
        _ => {
            ensure!(
                positional.is_empty(),
                "languages does not accept positional arguments"
            );
            Command::Languages
        }
    };
    Ok(Options {
        command,
        json,
        output,
    })
}

/// Runs in both the console binary and the GUI binary's compatibility CLI path.
pub fn run(args: Vec<String>) -> i32 {
    let requested_json =
        args.iter().any(|a| a == "--json") || args.windows(2).any(|a| a == ["--format", "json"]);
    let options = match parse(&args) {
        Ok(options) => options,
        Err(error) => {
            report_error(&error, requested_json);
            return 2;
        }
    };
    match execute(options) {
        Ok(()) => 0,
        Err(error) => {
            report_error(&error, requested_json);
            1
        }
    }
}

fn report_error(error: &anyhow::Error, json: bool) {
    // Only the intentionally safe outer error is emitted; never dump config or inputs.
    let message = error.to_string();
    if json {
        let _ = writeln!(
            std::io::stderr().lock(),
            "{}",
            json!({"error":{"message":message}})
        );
    } else {
        let _ = writeln!(std::io::stderr().lock(), "SightOCR: {message}");
    }
}

fn execute(options: Options) -> Result<()> {
    let result = match options.command {
        Command::Help => return write_output(HELP.as_bytes(), None),
        Command::Version => {
            return write_output(
                format!("SightOCR {}\n", env!("CARGO_PKG_VERSION")).as_bytes(),
                None,
            )
        }
        Command::Mcp => return crate::mcp::run_stdio(),
        Command::Languages => languages(),
        Command::Ocr { image, table } => Headless::default().ocr(&image, table)?,
        Command::Translate {
            input,
            source,
            target,
            provider,
        } => {
            let text = match input {
                TextInput::Literal(text) => text,
                TextInput::Stdin => read_text(std::io::stdin().lock())?,
                TextInput::File(path) => {
                    read_text(File::open(path).context("Cannot open UTF-8 input file")?)?
                }
            };
            Headless::default().translate(
                &text,
                source.as_deref(),
                target.as_deref(),
                provider.as_deref(),
            )?
        }
    };
    let mut output = if options.json {
        serde_json::to_string(&result)?
    } else if let Some(text) = result.get("text").and_then(Value::as_str) {
        text.to_owned()
    } else {
        LANGUAGES
            .iter()
            .map(|(code, name)| format!("{code}\t{name}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    if !output.ends_with('\n') {
        output.push('\n');
    }
    write_output(output.as_bytes(), options.output.as_deref())
}

fn write_output(bytes: &[u8], path: Option<&Path>) -> Result<()> {
    if let Some(path) = path {
        std::fs::write(path, bytes).context("Cannot write output file")
    } else {
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(bytes).context("Cannot write stdout")?;
        stdout.flush().context("Cannot flush stdout")
    }
}

fn read_text(reader: impl Read) -> Result<String> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_TEXT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .context("Cannot read text input")?;
    ensure!(
        bytes.len() <= MAX_TEXT_BYTES,
        "Text exceeds the 1 MiB limit"
    );
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
    let text = std::str::from_utf8(bytes)
        .context("Text input must be UTF-8")?
        .to_owned();
    validate_text(&text)?;
    Ok(text)
}

pub(crate) fn validate_text(text: &str) -> Result<()> {
    ensure!(!text.trim().is_empty(), "Text must not be empty");
    ensure!(text.len() <= MAX_TEXT_BYTES, "Text exceeds the 1 MiB limit");
    ensure!(!text.contains('\0'), "Text must not contain NUL bytes");
    Ok(())
}

pub(crate) fn validate_language(language: &str, target: bool) -> Result<String> {
    let language = normalize_language(language).context("Unsupported language; run languages")?;
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
            _ => bail!("Unsupported saved provider; use --provider or update settings"),
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

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).into()).collect()
    }

    #[test]
    fn invalid_flags_and_ambiguous_inputs_are_rejected() {
        for input in [
            vec!["translate", "hello", "--table"],
            vec!["ocr", "image.png", "--from", "en"],
            vec!["translate", "--stdin", "--input", "file.txt"],
            vec!["translate", "hello", "--stdin"],
            vec!["translate", "hello", "--provider", "unknown"],
            vec!["translate", "hello", "--to", "auto"],
            vec!["ocr", "x", "--output"],
            vec!["ocr", "x", "--table", "--table"],
            vec!["languages", "--json", "--format", "json"],
            vec!["mcp", "--json"],
            vec!["--help", "bad"],
        ] {
            assert!(parse(&args(&input)).is_err(), "{input:?}");
        }
    }

    #[test]
    fn aliases_and_dash_text_are_supported() {
        let options = parse(&args(&["--translate", "--to", "zh-CN", "--", "--hello"])).unwrap();
        assert_eq!(
            options.command,
            Command::Translate {
                input: TextInput::Literal("--hello".into()),
                source: None,
                target: Some("zh-Hans".into()),
                provider: None
            }
        );
        assert_eq!(parse(&args(&["--mcp"])).unwrap().command, Command::Mcp);
        assert_eq!(
            parse(&args(&["translate", "-"])).unwrap().command,
            Command::Translate {
                input: TextInput::Stdin,
                source: None,
                target: None,
                provider: None
            }
        );
    }

    #[test]
    fn utf8_input_is_bounded_and_bom_is_removed() {
        assert_eq!(read_text("\u{feff}中文\n".as_bytes()).unwrap(), "中文\n");
        assert!(read_text(&[0xff][..]).is_err());
        assert!(read_text(" \n".as_bytes()).is_err());
        assert!(read_text(vec![b'a'; MAX_TEXT_BYTES + 1].as_slice()).is_err());
        assert!(validate_text("a\0b").is_err());
    }
}
