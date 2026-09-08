//! A bounded MCP stdio server for SightOCR's three synchronous tools.
//! Implements the 2025-11-25 / 2025-06-18 lifecycle and tools subset. No HTTP,
//! resources, prompts, sampling, tasks, or configuration mutation are advertised.
use crate::cli::{self, Headless, MAX_TEXT_BYTES, PROVIDERS};
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::io::{BufRead, Write};

const PROTOCOL_VERSION: &str = "2025-11-25";
const SUPPORTED_VERSIONS: &[&str] = &[PROTOCOL_VERSION, "2025-06-18"];
// A 1 MiB UTF-8 text can expand six-fold when JSON escapes are used.
const MAX_MESSAGE_BYTES: usize = 7 * 1024 * 1024;

pub fn run_stdio() -> Result<()> {
    serve(std::io::stdin().lock(), std::io::stdout().lock())
}

fn serve(mut input: impl BufRead, mut output: impl Write) -> Result<()> {
    let mut server = Server::default();
    loop {
        let frame = read_frame(&mut input).context("Cannot read MCP stdin")?;
        let reply = match frame {
            Frame::Eof => break,
            Frame::Oversized => Some(error(
                Value::Null,
                -32600,
                "Message exceeds the 7 MiB limit",
            )),
            Frame::Message(bytes) => match serde_json::from_slice(&bytes) {
                Ok(request) => server.handle(request),
                Err(_) => Some(error(
                    Value::Null,
                    -32700,
                    "Parse error: expected one UTF-8 JSON object per line",
                )),
            },
        };
        if let Some(reply) = reply {
            serde_json::to_writer(&mut output, &reply).context("Cannot write MCP response")?;
            output
                .write_all(b"\n")
                .context("Cannot write MCP response")?;
            output.flush().context("Cannot flush MCP stdout")?;
        }
    }
    Ok(())
}

enum Frame {
    Eof,
    Oversized,
    Message(Vec<u8>),
}

/// Drain oversized lines without ever accumulating their full contents.
fn read_frame(input: &mut impl BufRead) -> std::io::Result<Frame> {
    let mut bytes = Vec::new();
    let mut oversized = false;
    loop {
        let buffer = input.fill_buf()?;
        if buffer.is_empty() {
            return Ok(if oversized {
                Frame::Oversized
            } else if bytes.is_empty() {
                Frame::Eof
            } else {
                Frame::Message(bytes)
            });
        }
        let newline = buffer.iter().position(|b| *b == b'\n');
        let take = newline.map_or(buffer.len(), |index| index + 1);
        if !oversized {
            if bytes.len().saturating_add(take) > MAX_MESSAGE_BYTES {
                oversized = true;
                bytes.clear();
            } else {
                bytes.extend_from_slice(&buffer[..take]);
            }
        }
        input.consume(take);
        if newline.is_some() {
            return Ok(if oversized {
                Frame::Oversized
            } else {
                Frame::Message(bytes)
            });
        }
    }
}

#[derive(Default, PartialEq, Eq)]
enum Phase {
    #[default]
    New,
    Initializing,
    Ready,
}

#[derive(Default)]
struct Server {
    phase: Phase,
    headless: Headless,
}

impl Server {
    fn handle(&mut self, request: Value) -> Option<Value> {
        let Some(object) = request.as_object() else {
            return Some(error(
                Value::Null,
                -32600,
                "Invalid request; JSON-RPC batches are not supported",
            ));
        };
        let id = object.get("id");
        let valid_id = id.is_none_or(|value| value.is_string() || value.is_i64() || value.is_u64());
        let method = object.get("method").and_then(Value::as_str);
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || !valid_id
            || method.is_none()
        {
            // No requests are sent to clients, so valid client responses can be ignored.
            if valid_id
                && id.is_some()
                && method.is_none()
                && object.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
                && (object.contains_key("result") ^ object.contains_key("error"))
            {
                return None;
            }
            return Some(error(
                if valid_id {
                    id.cloned().unwrap_or(Value::Null)
                } else {
                    Value::Null
                },
                -32600,
                "Invalid JSON-RPC request",
            ));
        }
        let method = method.unwrap_or_default();
        let params = object.get("params");
        if id.is_none() {
            // Notifications NEVER receive a response, including unknown ones.
            if method == "notifications/initialized"
                && self.phase == Phase::Initializing
                && params.is_none_or(Value::is_object)
            {
                self.phase = Phase::Ready;
            }
            return None;
        }
        let id = id.cloned().unwrap_or(Value::Null);
        if params.is_some_and(|value| !value.is_object()) {
            return Some(error(id, -32602, "params must be an object"));
        }
        let empty = Map::new();
        let params = params.and_then(Value::as_object).unwrap_or(&empty);
        let reply = match method {
            "initialize" => self.initialize(&id, params),
            "ping" => success(id, json!({})),
            _ if self.phase != Phase::Ready => error(
                id,
                -32002,
                "Initialize and send notifications/initialized before calling tools",
            ),
            "tools/list" => {
                if params.keys().any(|key| key != "_meta") {
                    error(
                        id,
                        -32602,
                        "tools/list has no pagination cursor or other parameters",
                    )
                } else {
                    success(id, json!({"tools": tool_definitions()}))
                }
            }
            "tools/call" => self.call_tool(id, params),
            _ => error(id, -32601, "Method not found"),
        };
        Some(reply)
    }

    fn initialize(&mut self, id: &Value, params: &Map<String, Value>) -> Value {
        if self.phase != Phase::New {
            return error(id.clone(), -32600, "Session is already initialized");
        }
        let Some(version) = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 64)
        else {
            return error(id.clone(), -32602, "initialize requires protocolVersion");
        };
        let valid_info = params
            .get("clientInfo")
            .and_then(Value::as_object)
            .is_some_and(|info| {
                ["name", "version"].iter().all(|key| {
                    info.get(*key)
                        .and_then(Value::as_str)
                        .is_some_and(|value| !value.is_empty() && value.len() <= 256)
                })
            });
        if !valid_info || !params.get("capabilities").is_some_and(Value::is_object) {
            return error(
                id.clone(),
                -32602,
                "initialize requires clientInfo name/version and capabilities",
            );
        }
        let version = if SUPPORTED_VERSIONS.contains(&version) {
            version
        } else {
            PROTOCOL_VERSION
        };
        self.phase = Phase::Initializing;
        success(
            id.clone(),
            json!({
                "protocolVersion":version,
                "capabilities":{"tools":{"listChanged":false}},
                "serverInfo":{"name":"sightocr", "version":env!("CARGO_PKG_VERSION")},
            "instructions":"Local Windows OCR and translation. sightocr_ocr never uploads images. sightocr_translate sends text to the configured or explicitly selected translation service and may use its quota. Settings are read only. Calls are processed serially with a 120-second worker limit. Cancellation notifications do not interrupt an active call. Closing stdin ends the session after the current call finishes; terminate the server process to stop immediately."
            }),
        )
    }

    fn call_tool(&mut self, id: Value, params: &Map<String, Value>) -> Value {
        if params
            .keys()
            .any(|key| !matches!(key.as_str(), "name" | "arguments" | "_meta"))
        {
            return error(id, -32602, "Unsupported tools/call parameter");
        }
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return error(id, -32602, "tools/call requires a tool name");
        };
        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
        if !arguments.is_object() {
            return error(id, -32602, "Tool arguments must be an object");
        }
        let result = match name {
            "sightocr_ocr" => serde_json::from_value::<OcrArgs>(arguments)
                .map_err(|_| anyhow::anyhow!("Expected image_path (non-empty string) and optional table (boolean); no extra fields"))
                .and_then(|args| {
                    anyhow::ensure!(!args.image_path.trim().is_empty() && args.image_path.len() <= 32767 && !args.image_path.chars().any(char::is_control), "Invalid local image path");
                    anyhow::ensure!(!args.image_path.starts_with("\\\\") && !args.image_path.starts_with("//"), "Use a local disk image path, not a network or device path");
                    anyhow::ensure!(std::path::Path::new(&args.image_path).is_absolute(), "image_path must be an absolute Windows file path");
                    self.headless.ocr(std::path::Path::new(&args.image_path), args.table)
                }),
            "sightocr_translate" => serde_json::from_value::<TranslateArgs>(arguments)
                .map_err(|_| anyhow::anyhow!("Expected text and optional source_lang, target_lang, provider strings; no extra fields"))
                .and_then(|args| self.headless.translate(&args.text, args.source_lang.as_deref(), args.target_lang.as_deref(), args.provider.as_deref())),
            "sightocr_languages" => {
                if arguments.as_object().is_some_and(Map::is_empty) { Ok(cli::languages()) }
                else { Err(anyhow::anyhow!("sightocr_languages accepts no arguments")) }
            }
            _ => return error(id, -32602, "Unknown tool name"),
        };
        let result = match result {
            Ok(data) => {
                json!({"content":[{"type":"text", "text":data.to_string()}], "structuredContent":data, "isError":false})
            }
            Err(error) => {
                json!({"content":[{"type":"text", "text":error.to_string()}], "isError":true})
            }
        };
        success(id, result)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OcrArgs {
    image_path: String,
    #[serde(default)]
    table: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TranslateArgs {
    text: String,
    #[serde(default, deserialize_with = "optional_string")]
    source_lang: Option<String>,
    #[serde(default, deserialize_with = "optional_string")]
    target_lang: Option<String>,
    #[serde(default, deserialize_with = "optional_string")]
    provider: Option<String>,
}

// An omitted optional string is allowed; explicit null contradicts inputSchema.
fn optional_string<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
}

fn success(id: Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0", "id":id, "result":result})
}
fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0", "id":id, "error":{"code":code, "message":message}})
}

fn tool_definitions() -> Value {
    let source_codes: Vec<&str> = crate::config::LANGUAGES
        .iter()
        .map(|(code, _)| *code)
        .collect();
    let target_codes: Vec<&str> = source_codes
        .iter()
        .copied()
        .filter(|code| *code != "auto")
        .collect();
    json!([
        {
            "name":"sightocr_ocr", "title":"Local image OCR",
            "description":"Recognize text or TSV table content in a local Windows image file with OneOCR. Always local: the image is never sent to any online service, regardless of saved OCR settings. Supports PNG, JPEG, BMP, WebP; file limit 64 MiB and 100 million pixels. No UI, clipboard, or settings changes.",
            "inputSchema":{"type":"object", "properties":{
                "image_path":{"type":"string", "minLength":1, "maxLength":32767, "description":"Absolute Windows disk file path (for example C:\\Pictures\\image.png); URLs and UNC/device paths are rejected. Mapped drives use the host's existing filesystem access."},
                "table":{"type":"boolean", "default":false, "description":"Return table rows as TSV, preserving tabs and newlines."}
            }, "required":["image_path"], "additionalProperties":false},
            "outputSchema":{"type":"object", "properties":{
                "text":{"type":"string"}, "operation":{"const":"ocr"}, "provider":{"const":"local"}, "table":{"type":"boolean"}
            }, "required":["text","operation","provider","table"], "additionalProperties":false},
            "annotations":{"readOnlyHint":true, "destructiveHint":false, "idempotentHint":true, "openWorldHint":false}
        },
        {
            "name":"sightocr_translate", "title":"Translate text with the selected service",
            "description":"Translate text. This sends the provided text to the saved or explicitly selected online translation provider and may consume quota. Credentials come from existing SightOCR settings and are never returned. Optional language/provider overrides apply only to this call. Default settings use Bing, auto source, and zh-Hans target. UTF-8 text must be non-empty and at most 1 MiB in bytes. No automatic provider switching or settings writes.",
            "inputSchema":{"type":"object", "properties":{
                "text":{"type":"string", "minLength":1, "maxLength":MAX_TEXT_BYTES, "description":"Text to send to the translation service; maximum 1 MiB UTF-8 bytes."},
                "source_lang":{"type":"string", "enum":source_codes},
                "target_lang":{"type":"string", "enum":target_codes},
                "provider":{"type":"string", "enum":PROVIDERS}
            }, "required":["text"], "additionalProperties":false},
            "outputSchema":{"type":"object", "properties":{
                "text":{"type":"string"}, "operation":{"const":"translate"}, "provider":{"type":"string", "enum":PROVIDERS}, "source_lang":{"type":"string"}, "target_lang":{"type":"string"}
            }, "required":["text","operation","provider","source_lang","target_lang"], "additionalProperties":false},
            "annotations":{"readOnlyHint":true, "destructiveHint":false, "idempotentHint":false, "openWorldHint":true}
        },
        {
            "name":"sightocr_languages", "title":"Supported translation languages",
            "description":"List supported source and target language codes without network access or reading credentials.",
            "inputSchema":{"type":"object", "properties":{}, "additionalProperties":false},
            "outputSchema":{"type":"object", "properties":{"languages":{"type":"array", "items":{
                "type":"object", "properties":{"code":{"type":"string"}, "name":{"type":"string"}, "source":{"type":"boolean"}, "target":{"type":"boolean"}}, "required":["code","name","source","target"], "additionalProperties":false
            }}}, "required":["languages"], "additionalProperties":false},
            "annotations":{"readOnlyHint":true, "destructiveHint":false, "idempotentHint":true, "openWorldHint":false}
        }
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    fn init(version: &str) -> Value {
        json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{"protocolVersion":version, "clientInfo":{"name":"test","version":"1"}, "capabilities":{}}})
    }
    fn ready() -> Server {
        let mut server = Server::default();
        server.handle(init(PROTOCOL_VERSION));
        server.handle(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        server
    }

    #[test]
    fn initialization_negotiates_versions_and_requires_notification() {
        for (requested, expected) in [
            (PROTOCOL_VERSION, PROTOCOL_VERSION),
            ("2025-06-18", "2025-06-18"),
            ("future", PROTOCOL_VERSION),
        ] {
            let mut server = Server::default();
            assert_eq!(
                server.handle(init(requested)).unwrap()["result"]["protocolVersion"],
                expected
            );
            assert_eq!(
                server
                    .handle(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
                    .unwrap()["error"]["code"],
                -32002
            );
            assert!(server
                .handle(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
                .is_none());
            assert!(server
                .handle(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
                .unwrap()["result"]["tools"]
                .is_array());
            assert_eq!(
                server.handle(init(requested)).unwrap()["error"]["code"],
                -32600
            );
        }
    }

    #[test]
    fn invalid_requests_notifications_and_tool_errors_are_distinct() {
        let mut server = ready();
        assert!(server
            .handle(json!({"jsonrpc":"2.0","method":"unknown/notification"}))
            .is_none());
        assert_eq!(
            server
                .handle(json!({"jsonrpc":"2.0","id":null,"method":"ping"}))
                .unwrap()["error"]["code"],
            -32600
        );
        assert_eq!(
            server
                .handle(json!({"jsonrpc":"2.0","id":2,"method":"unknown"}))
                .unwrap()["error"]["code"],
            -32601
        );
        assert_eq!(server.handle(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"unknown"}})).unwrap()["error"]["code"], -32602);
        let result = server.handle(json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"sightocr_translate","arguments":{"text":"   "}}})).unwrap();
        assert_eq!(result["result"]["isError"], true);
        assert!(result.get("error").is_none());
    }

    #[test]
    fn oversized_message_is_drained_and_next_request_survives() {
        let mut input = vec![b'a'; MAX_MESSAGE_BYTES + 50];
        input.extend_from_slice(b"\n{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"ping\"}\n");
        let mut output = Vec::new();
        serve(std::io::Cursor::new(input), &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        let replies: Vec<Value> = output
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0]["error"]["code"], -32600);
        assert_eq!(replies[1]["id"], 4);
        assert_eq!(replies[1]["result"], json!({}));
    }

    #[test]
    fn tool_definitions_expose_schemas_and_privacy_annotations() {
        let definitions = tool_definitions();
        let definitions = definitions.as_array().unwrap();
        assert_eq!(definitions.len(), 3);
        for tool in definitions {
            assert!(tool["name"].as_str().unwrap().starts_with("sightocr_"));
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
            assert_eq!(tool["outputSchema"]["type"], "object");
        }
        assert_eq!(definitions[0]["annotations"]["openWorldHint"], false);
        assert_eq!(definitions[1]["annotations"]["openWorldHint"], true);
    }
}
