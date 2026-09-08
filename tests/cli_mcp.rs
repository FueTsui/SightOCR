//! Exercise actual Windows console pipes and the MCP lifecycle without network.
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    path::Path,
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

fn command(config: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_sightocr-cli"));
    command.env("SIGHTOCR_CONFIG", config);
    command
}

fn run(mut command: Command, input: &[u8]) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let err = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).unwrap();
        bytes
    });
    child.stdin.take().unwrap().write_all(input).unwrap();
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(30) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("Console/MCP process did not exit within 30 seconds");
        }
        thread::sleep(Duration::from_millis(20));
    };
    Output {
        status,
        stdout: out.join().unwrap(),
        stderr: err.join().unwrap(),
    }
}

#[test]
fn discoverable_help_version_and_strict_exit_codes() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("missing.json");
    for args in [vec!["--help"], vec!["--version"], vec![]] {
        let mut cmd = command(&config);
        cmd.args(args);
        let output = run(cmd, b"");
        assert!(output.status.success());
        assert!(!output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
    let mut cmd = command(&config);
    cmd.args(["translate", "text", "--to", "auto", "--json"]);
    let output = run(cmd, b"");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(
        serde_json::from_slice::<Value>(&output.stderr).unwrap()["error"]["message"].is_string()
    );
    assert!(!config.exists());
}

#[test]
fn utf8_stdin_file_output_and_provider_override_preserve_settings() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("settings.json");
    let settings = br#"{"last_translate_selection":"OpenAI","openai_api_key":"secret-never-log-this","source_lang":"ja","target_lang":"zh-Hans"}"#;
    std::fs::write(&config, settings).unwrap();
    let text = "中文输入\nsecond line";
    let mut cmd = command(&config);
    cmd.args([
        "translate",
        "--stdin",
        "--from",
        "en",
        "--to",
        "en",
        "--provider",
        "bing",
        "--json",
    ]);
    let output = run(cmd, format!("\u{feff}{text}").as_bytes());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["text"], text);
    assert_eq!(result["provider"], "bing");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("secret-never-log-this"));

    let source = temp.path().join("输入.txt");
    let destination = temp.path().join("输出.json");
    std::fs::write(&source, text).unwrap();
    let mut cmd = command(&config);
    cmd.args(["--translate", "--input"])
        .arg(source)
        .args([
            "--from",
            "en",
            "--to",
            "en",
            "--provider",
            "tencent",
            "--format",
            "json",
            "--output",
        ])
        .arg(&destination);
    let output = run(cmd, b"");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(destination).unwrap()).unwrap()["text"],
        text
    );
    assert_eq!(std::fs::read(config).unwrap(), settings);
}

#[test]
fn headless_default_config_does_not_migrate_legacy_secrets() {
    let temp = tempfile::tempdir().unwrap();
    let appdata = temp.path().join("appdata");
    let legacy = br#"{"last_translate_selection":"OpenAI","openai_api_key":"legacy-secret"}"#;
    std::fs::write(temp.path().join("SightOCR.py"), "legacy app").unwrap();
    std::fs::write(temp.path().join("config.json"), legacy).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sightocr-cli"));
    cmd.current_dir(temp.path())
        .env_remove("SIGHTOCR_CONFIG")
        .env("APPDATA", &appdata)
        .args(["translate", "hello", "--from", "en", "--to", "en", "--json"]);
    let output = run(cmd, b"");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["provider"],
        "bing"
    );
    assert!(!appdata.exists());
    assert_eq!(
        std::fs::read(temp.path().join("config.json")).unwrap(),
        legacy
    );
}

#[test]
fn stdio_mcp_lifecycle_tools_errors_and_reuse_are_clean_json() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("missing.json");
    let requests = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","clientInfo":{"name":"process-test","version":"1"},"capabilities":{}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","method":"unknown/notification"}),
        json!({"jsonrpc":"2.0","id":"list","method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"sightocr_languages","arguments":{}}}),
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"sightocr_translate","arguments":{"text":"hello 中文","source_lang":"en","target_lang":"en"}}}),
        json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"sightocr_translate","arguments":{"text":"second call","source_lang":"en","target_lang":"en"}}}),
        json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"sightocr_ocr","arguments":{"image_path":"missing.png"}}}),
        json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"sightocr_languages","arguments":{"extra":1}}}),
        json!({"jsonrpc":"2.0","id":8,"method":"ping"}),
    ];
    let payload = requests
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let mut cmd = command(&config);
    cmd.arg("mcp");
    let output = run(cmd, payload.as_bytes());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let replies: Vec<Value> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(replies.len(), 8, "Notifications must not produce output");
    assert_eq!(replies[0]["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(replies[1]["id"], "list");
    assert_eq!(replies[1]["result"]["tools"].as_array().unwrap().len(), 3);
    assert!(replies[2]["result"]["structuredContent"]["languages"].is_array());
    for (reply, expected) in [(&replies[3], "hello 中文"), (&replies[4], "second call")] {
        assert_eq!(reply["result"]["isError"], false);
        assert_eq!(reply["result"]["structuredContent"]["text"], expected);
        assert_eq!(
            serde_json::from_str::<Value>(reply["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap(),
            reply["result"]["structuredContent"]
        );
    }
    assert_eq!(replies[5]["result"]["isError"], true);
    assert_eq!(replies[6]["result"]["isError"], true);
    assert_eq!(replies[7]["result"], json!({}));
    assert!(!config.exists());
}

#[test]
fn stdio_recovers_after_malformed_json_and_rejects_invalid_utf8_input() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("missing.json");
    let mut cmd = command(&config);
    cmd.arg("--mcp");
    let output = run(
        cmd,
        b"not-json\n{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"ping\"}\n",
    );
    assert!(output.status.success());
    let replies: Vec<Value> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(replies[0]["error"]["code"], -32700);
    assert_eq!(replies[1]["id"], 9);
    let mut cmd = command(&config);
    cmd.args(["translate", "--stdin", "--json"]);
    let output = run(cmd, &[0xff]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(
        serde_json::from_slice::<Value>(&output.stderr).unwrap()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("UTF-8")
    );
}
