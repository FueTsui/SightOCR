//! Run the real `sightocr-cli.exe` through Windows pipes, without network:
//! translations use the same source and target language, which never leaves
//! the process. OCR uses the repository model when it is present.
use serde_json::Value;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

fn command(config: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_sightocr-cli"));
    command.env("SIGHTOCR_CONFIG", config).args(args);
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
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input).unwrap();
    drop(stdin);
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(90) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("CLI process did not exit within 90 seconds");
        }
        thread::sleep(Duration::from_millis(20));
    };
    Output {
        status,
        stdout: out.join().unwrap(),
        stderr: err.join().unwrap(),
    }
}

fn ok(output: &Output) -> &str {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    std::str::from_utf8(&output.stdout).unwrap()
}

#[test]
fn translate_reads_text_stdin_and_files_and_prints_ascii_json() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("missing.json");
    let same = ["--from", "en", "--to", "en"];

    let output = run(
        command(
            &config,
            &[&["translate", "--text", "hello 中文"][..], &same].concat(),
        ),
        b"",
    );
    assert_eq!(ok(&output), "hello 中文\n");

    let output = run(
        command(&config, &[&["translate"][..], &same].concat()),
        "管道\n输入".as_bytes(),
    );
    assert_eq!(ok(&output), "管道\n输入\n");

    // Windows PowerShell 5.1 writes UTF-16 LE with a byte order mark.
    let file = temp.path().join("utf16.txt");
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend("第一行\n第二行".encode_utf16().flat_map(u16::to_le_bytes));
    std::fs::write(&file, bytes).unwrap();
    let path = file.to_str().unwrap();
    let output = run(
        command(
            &config,
            &[&["translate", "--file", path, "--json"][..], &same].concat(),
        ),
        b"",
    );
    let text = ok(&output);
    assert!(text.is_ascii(), "{text}");
    let value: Value = serde_json::from_str(text).unwrap();
    assert_eq!(value["text"], "第一行\n第二行");
    assert_eq!(value["provider"], "bing");
    assert_eq!(value["target_lang"], "en");

    let saved = temp.path().join("out.txt");
    let output = run(
        command(
            &config,
            &[
                &["translate", "--text", "保存", "-o", saved.to_str().unwrap()][..],
                &same,
            ]
            .concat(),
        ),
        b"",
    );
    assert_eq!(ok(&output), "");
    assert_eq!(std::fs::read_to_string(&saved).unwrap(), "保存\n");
    assert!(!config.exists(), "settings must never be written");
}

#[test]
fn lists_languages_and_providers_without_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config.json");
    std::fs::write(
        &config,
        r#"{"baidu_trans_appid":"synthetic-id","baidu_trans_appkey":"synthetic-secret","target_lang":"ja"}"#,
    )
    .unwrap();
    let output = run(command(&config, &["providers", "--json"]), b"");
    let text = ok(&output);
    assert!(!text.contains("synthetic"), "credentials leaked: {text}");
    let value: Value = serde_json::from_str(text).unwrap();
    let providers = value["providers"].as_array().unwrap();
    assert_eq!(providers[0]["provider"], "bing");
    assert_eq!(providers[0]["default"], true);
    assert_eq!(providers[1]["configured"], true);
    assert_eq!(providers[2]["configured"], false);
    assert_eq!(value["target_lang"], "ja");

    let output = run(command(&config, &["languages"]), b"");
    let text = ok(&output);
    assert!(text
        .lines()
        .any(|line| line.starts_with("auto\t") && line.ends_with("\tsource")));
    assert!(text
        .lines()
        .any(|line| line.starts_with("en\t") && line.ends_with("\tsource,target")));
}

#[test]
fn usage_errors_exit_2_and_failures_exit_1() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("missing.json");
    for args in [
        &[][..],
        &["mcp"],
        &["ocr"],
        &["ocr", "a.png", "--table", "--layout"],
        &["translate", "--text", "a", "--file", "b"],
        &["translate", "--bogus"],
        &["languages", "extra"],
        &["--help", "extra"],
    ] {
        let output = run(command(&config, args), b"");
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(!output.stderr.is_empty(), "{args:?}");
    }

    let missing = temp.path().join("missing.png");
    let output = run(
        command(&config, &["ocr", missing.to_str().unwrap(), "--json"]),
        b"",
    );
    assert_eq!(output.status.code(), Some(1));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value["error"]
        .as_str()
        .unwrap()
        .contains("Cannot open local image file"));

    let output = run(
        command(&config, &["translate", "--to", "auto", "--text", "x"]),
        b"",
    );
    assert_eq!(output.status.code(), Some(1));

    let output = run(command(&config, &["--help"]), b"");
    assert!(ok(&output).contains("sightocr-cli ocr <image>"));
}

// build.rs sets `has_oneocr` only when the model files are present, so a run
// without them reports this test as ignored instead of passed.
#[test]
#[cfg_attr(
    not(has_oneocr),
    ignore = "OneOCR model files are not in resources/oneocr (see tests/ExtractOCR.bat)"
)]
fn recognizes_a_table_with_the_local_model() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let resources = root.join("resources/oneocr");
    let temp = tempfile::tempdir().unwrap();
    let image = root.join("tests/fixtures/basic.png");
    let mut cmd = command(
        &temp.path().join("missing.json"),
        &["ocr", image.to_str().unwrap(), "--table"],
    );
    cmd.env("SIGHTOCR_RESOURCES", &resources);
    let output = run(cmd, b"");
    let text = ok(&output);
    assert!(
        text.contains("Alpha\t100") && text.contains("Beta\t200"),
        "{text}"
    );
}
