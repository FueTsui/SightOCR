//! `sightocr-cli.exe`: one command per process, for AI agents and scripts.
//!
//! Each run reads its input, does one job and exits, so nothing stays
//! resident to lock installed files during updates. Default output is the
//! plain text on stdout; `--json` prints one object with every non-ASCII
//! character escaped, which survives any console code page. The CLI's own
//! messages are ASCII English for the same reason. Exit codes: 0 success,
//! 1 the operation failed, 2 invalid usage.
use crate::headless::{self, Headless, MAX_TEXT_BYTES};
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    io::{IsTerminal, Read, Write},
    path::{Path, PathBuf},
};

pub const USAGE: &str = "\
SightOCR CLI: local image OCR and text translation.

Usage:
  sightocr-cli ocr <image> [--table | --layout] [--json] [-o <file>]
  sightocr-cli translate (--text <text> | --file <path> | <stdin>)
               [--from <code>] [--to <code>] [--provider <name>] [--json] [-o <file>]
  sightocr-cli languages [--json]
  sightocr-cli providers [--json]
  sightocr-cli --help | --version

ocr        Recognize PNG/JPEG/BMP/WebP locally; the image is never uploaded.
           --table   rows as TSV (tab-separated cells, one row per line)
           --layout  reading order, wrapped lines merged, one paragraph per line
translate  Send text to the saved or chosen translation service (network).
           --from/--to  language codes from `languages`; --from may be auto
           --provider   bing, baidu, tencent or model:<id> from `providers`
           --file       UTF-8, or UTF-16 with a byte order mark
languages  List language codes.
providers  List translation providers and saved defaults (no credentials).

--json     Print one JSON object; non-ASCII characters are \\u-escaped.
-o <file>  Write the result to <file> as UTF-8 instead of stdout.

Settings are read from %APPDATA%\\SightOCR\\config.json or SIGHTOCR_CONFIG and
never written. Exit codes: 0 success, 1 failure, 2 invalid usage.
";

/// Upper bound for a text file or stdin before decoding (UTF-16 doubles size).
const MAX_INPUT_BYTES: u64 = 4 * MAX_TEXT_BYTES as u64;

struct Usage(String);

impl std::fmt::Display for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn usage(message: impl Into<String>) -> Usage {
    Usage(message.into())
}

enum Command {
    Help,
    Version,
    Ocr {
        image: PathBuf,
        table: bool,
        layout: bool,
    },
    Translate {
        input: Input,
        from: Option<String>,
        to: Option<String>,
        provider: Option<String>,
    },
    Languages,
    Providers,
}

enum Input {
    Text(String),
    File(PathBuf),
    Stdin,
}

struct Options {
    command: Command,
    json: bool,
    output: Option<PathBuf>,
}

/// Run one command and return the process exit code.
pub fn run(args: Vec<String>) -> i32 {
    let json_requested = args.iter().any(|arg| arg == "--json");
    let options = match parse(args) {
        Ok(options) => options,
        Err(Usage(message)) => {
            report(json_requested, &message);
            eprintln!("Run `sightocr-cli --help` for usage.");
            return 2;
        }
    };
    match execute(&options) {
        Ok(()) => 0,
        Err(error) => {
            report(options.json, &format!("{error:#}"));
            1
        }
    }
}

fn report(json_output: bool, message: &str) {
    eprintln!("sightocr-cli: {message}");
    if json_output {
        println!("{}", ascii_json(&json!({ "error": message })));
    }
}

fn parse(args: Vec<String>) -> std::result::Result<Options, Usage> {
    let mut args = args.into_iter();
    let Some(name) = args.next() else {
        return Err(usage("missing command"));
    };
    let simple = match name.as_str() {
        "-h" | "--help" | "help" => Some(Command::Help),
        "-V" | "--version" => Some(Command::Version),
        _ => None,
    };
    if let Some(command) = simple {
        if !args.as_slice().is_empty() {
            return Err(usage(format!("{name} takes no arguments")));
        }
        return Ok(Options {
            command,
            json: false,
            output: None,
        });
    }
    if !matches!(
        name.as_str(),
        "ocr" | "translate" | "languages" | "providers"
    ) {
        return Err(usage(format!("unknown command {name}")));
    }
    let mut json = false;
    let mut output = None;
    let mut positional = Vec::new();
    let (mut table, mut layout) = (false, false);
    let (mut text, mut file, mut from, mut to, mut provider) = (None, None, None, None, None);
    let value = |flag: &str, args: &mut std::vec::IntoIter<String>| {
        args.next()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| usage(format!("{flag} needs a value")))
    };
    let set = |slot: &mut Option<String>, flag: &str, given: String| {
        if slot.replace(given).is_some() {
            return Err(usage(format!("{flag} given more than once")));
        }
        Ok(())
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => json = true,
            "-o" | "--output" => {
                if output
                    .replace(PathBuf::from(value(&arg, &mut args)?))
                    .is_some()
                {
                    return Err(usage("-o given more than once"));
                }
            }
            "--table" if name == "ocr" => table = true,
            "--layout" if name == "ocr" => layout = true,
            "--text" if name == "translate" => set(&mut text, &arg, value(&arg, &mut args)?)?,
            "--file" if name == "translate" => set(&mut file, &arg, value(&arg, &mut args)?)?,
            "--from" if name == "translate" => set(&mut from, &arg, value(&arg, &mut args)?)?,
            "--to" if name == "translate" => set(&mut to, &arg, value(&arg, &mut args)?)?,
            "--provider" if name == "translate" => {
                set(&mut provider, &arg, value(&arg, &mut args)?)?
            }
            "--" => positional.extend(args.by_ref()),
            flag if flag.starts_with('-') && flag.len() > 1 => {
                return Err(usage(format!("unknown option {flag} for {name}")));
            }
            _ => positional.push(arg),
        }
    }
    let no_positional = |positional: &[String]| match positional.first() {
        Some(extra) => Err(usage(format!("unexpected argument {extra}"))),
        None => Ok(()),
    };
    let command = match name.as_str() {
        "ocr" => {
            let mut positional = positional.into_iter();
            let image = positional
                .next()
                .ok_or_else(|| usage("ocr needs an image path"))?;
            no_positional(&positional.collect::<Vec<_>>())?;
            if table && layout {
                return Err(usage("--table and --layout cannot be combined"));
            }
            Command::Ocr {
                image: PathBuf::from(image),
                table,
                layout,
            }
        }
        "translate" => {
            no_positional(&positional)?;
            let input = match (text, file) {
                (Some(_), Some(_)) => return Err(usage("use either --text or --file")),
                (Some(text), None) => Input::Text(text),
                (None, Some(file)) => Input::File(PathBuf::from(file)),
                (None, None) if std::io::stdin().is_terminal() => {
                    return Err(usage("give --text, --file, or pipe text to stdin"));
                }
                (None, None) => Input::Stdin,
            };
            Command::Translate {
                input,
                from,
                to,
                provider,
            }
        }
        "languages" => {
            no_positional(&positional)?;
            Command::Languages
        }
        _ => {
            no_positional(&positional)?;
            Command::Providers
        }
    };
    Ok(Options {
        command,
        json,
        output,
    })
}

fn execute(options: &Options) -> Result<()> {
    let data = match &options.command {
        Command::Help => return write_stdout(USAGE),
        Command::Version => {
            return write_stdout(&format!("sightocr-cli {}\n", env!("CARGO_PKG_VERSION")))
        }
        Command::Ocr {
            image,
            table,
            layout,
        } => Headless::default().ocr(image, *table, *layout)?,
        Command::Translate {
            input,
            from,
            to,
            provider,
        } => {
            let text = match input {
                Input::Text(text) => text.clone(),
                Input::File(path) => read_text(
                    std::fs::File::open(path).context("Cannot open the text file")?,
                    "text file",
                )?,
                Input::Stdin => read_text(std::io::stdin().lock(), "stdin")?,
            };
            Headless::default().translate(
                &text,
                from.as_deref(),
                to.as_deref(),
                provider.as_deref(),
            )?
        }
        Command::Languages => headless::languages(),
        Command::Providers => headless::providers()?,
    };
    let rendered = if options.json {
        ascii_json(&data) + "\n"
    } else {
        plain(&options.command, &data)
    };
    match &options.output {
        Some(path) => write_file(path, &rendered),
        None => write_stdout(&rendered),
    }
}

/// Plain output: the result text itself, or one tab-separated line per entry.
fn plain(command: &Command, data: &Value) -> String {
    let field = |value: &Value, key: &str| value[key].as_str().unwrap_or_default().to_owned();
    let mut text = match command {
        Command::Languages => lines(&data["languages"], |entry| {
            let role = if entry["target"] == true {
                "source,target"
            } else {
                "source"
            };
            format!("{}\t{}\t{role}", field(entry, "code"), field(entry, "name"))
        }),
        Command::Providers => {
            let mut text = lines(&data["providers"], |entry| {
                let mut line = format!("{}\t{}", field(entry, "provider"), field(entry, "name"));
                if entry["configured"] != true {
                    line.push_str("\tnot configured");
                }
                if entry["default"] == true {
                    line.push_str("\tdefault");
                }
                line
            });
            text.push_str(&format!(
                "\ndefault languages: {} -> {}",
                field(data, "source_lang"),
                field(data, "target_lang")
            ));
            text
        }
        _ => field(data, "text"),
    };
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

fn lines(list: &Value, line: impl Fn(&Value) -> String) -> String {
    list.as_array()
        .map(|entries| entries.iter().map(line).collect::<Vec<_>>().join("\n"))
        .unwrap_or_default()
}

/// Serialize with every non-ASCII character as `\uXXXX`. Non-ASCII can only
/// occur inside JSON strings, so escaping character by character is safe.
pub fn ascii_json(value: &Value) -> String {
    let raw = value.to_string();
    let mut escaped = String::with_capacity(raw.len());
    for c in raw.chars() {
        if c.is_ascii() {
            escaped.push(c);
        } else {
            for unit in c.encode_utf16(&mut [0; 2]) {
                escaped.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    escaped
}

/// Decode UTF-8 (optional BOM) or UTF-16 with a BOM; Windows PowerShell 5.1
/// writes UTF-16 LE by default.
fn read_text(reader: impl Read, source: &str) -> Result<String> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_INPUT_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("Cannot read {source}"))?;
    ensure!(
        bytes.len() as u64 <= MAX_INPUT_BYTES,
        "Text exceeds the 1 MiB limit"
    );
    decode(&bytes).with_context(|| format!("Cannot decode {source}"))
}

fn decode(bytes: &[u8]) -> Result<String> {
    let utf16 = |rest: &[u8], unit: fn([u8; 2]) -> u16| -> Result<String> {
        let (pairs, remainder) = rest.as_chunks::<2>();
        ensure!(remainder.is_empty(), "UTF-16 text has an odd byte count");
        let units: Vec<u16> = pairs.iter().copied().map(unit).collect();
        String::from_utf16(&units).context("Invalid UTF-16 text")
    };
    match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => utf8(rest),
        [0xFF, 0xFE, rest @ ..] => utf16(rest, u16::from_le_bytes),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, u16::from_be_bytes),
        _ => utf8(bytes),
    }
}

fn utf8(bytes: &[u8]) -> Result<String> {
    match std::str::from_utf8(bytes) {
        Ok(text) => Ok(text.to_owned()),
        Err(_) => bail!("Text must be UTF-8, or UTF-16 with a byte order mark"),
    }
}

fn write_stdout(text: &str) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
        .context("Cannot write to stdout")
}

fn write_file(path: &Path, text: &str) -> Result<()> {
    ensure!(!path.is_dir(), "Output path is a folder");
    std::fs::write(path, text).context("Cannot write the output file")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn parses_commands_and_rejects_bad_usage() {
        let options = parse(args(&[
            "ocr", "a.png", "--table", "--json", "-o", "out.txt",
        ]))
        .unwrap_or_else(|_| panic!("valid ocr"));
        assert!(options.json && options.output == Some(PathBuf::from("out.txt")));
        assert!(matches!(
            options.command,
            Command::Ocr {
                table: true,
                layout: false,
                ..
            }
        ));
        let options = parse(args(&["translate", "--text", "hi", "--to", "en"]))
            .unwrap_or_else(|_| panic!("valid translate"));
        assert!(matches!(
            options.command,
            Command::Translate {
                input: Input::Text(_),
                ..
            }
        ));
        for bad in [
            &[][..],
            &["ocr"],
            &["ocr", "a.png", "b.png"],
            &["ocr", "a.png", "--table", "--layout"],
            &["ocr", "a.png", "--text", "x"],
            &["translate", "--text", "a", "--file", "b"],
            &["translate", "--text", "a", "--text", "b"],
            &["translate", "--to"],
            &["languages", "extra"],
            &["--help", "--json"],
            &["mcp"],
        ] {
            assert!(parse(args(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn json_is_ascii_and_round_trips() {
        let value = json!({"text": "中文\t表格\n😀 \"q\""});
        let encoded = ascii_json(&value);
        assert!(encoded.is_ascii());
        assert_eq!(serde_json::from_str::<Value>(&encoded).unwrap(), value);
    }

    #[test]
    fn decodes_utf8_and_utf16_text() {
        assert_eq!(decode("中文".as_bytes()).unwrap(), "中文");
        assert_eq!(decode(b"\xEF\xBB\xBFabc").unwrap(), "abc");
        let mut le = vec![0xFF, 0xFE];
        le.extend("中a".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(decode(&le).unwrap(), "中a");
        let mut be = vec![0xFE, 0xFF];
        be.extend("中a".encode_utf16().flat_map(u16::to_be_bytes));
        assert_eq!(decode(&be).unwrap(), "中a");
        assert!(decode(&[0xC4, 0xE3]).is_err());
    }

    #[test]
    fn plain_output_keeps_text_and_lists_entries() {
        let ocr = Command::Ocr {
            image: PathBuf::new(),
            table: true,
            layout: false,
        };
        assert_eq!(plain(&ocr, &json!({"text": "a\tb\nc\td"})), "a\tb\nc\td\n");
        let languages = plain(&Command::Languages, &headless::languages());
        assert!(languages.starts_with("auto\t") && languages.contains("\tsource,target"));
    }
}
