//! Live checks of model interfaces against real services, configured by a local `.env`.
//! Ignored by default: it sends a synthetic table image, formula images and one English
//! sentence to every configured service and may consume quota.
//!
//! ```text
//! cargo test --test live_models -- --ignored --nocapture
//! ```
//!
//! `.env` (default: the project root, or `SIGHTOCR_LIVE_ENV`) may define, per service,
//! `{PREFIX}_BASE_URL`, `{PREFIX}_API_KEY`, `{PREFIX}_OCR_MODEL` and
//! `{PREFIX}_TRANSLATE_MODEL` for the prefixes `TENCENT`, `MISTRAL` and `AZURE_OPENAI`
//! (case-insensitive). Keys are never printed. `SIGHTOCR_LIVE_FORMULAS` may point to a
//! directory of `formula-*.png` images; `SIGHTOCR_LIVE_REPORT` receives a JSON summary.

use sightocr::{
    config::{Config, ModelProfile, ModelProtocol},
    services::Services,
};
use std::{collections::BTreeMap, path::PathBuf, time::Instant};

fn read_env() -> BTreeMap<String, String> {
    let path = std::env::var_os("SIGHTOCR_LIVE_ENV")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".env"));
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            let value = value.trim().trim_matches(['"', '\'']);
            Some((key.trim().to_ascii_uppercase(), value.to_owned()))
        })
        .collect()
}

fn profiles(env: &BTreeMap<String, String>) -> Vec<ModelProfile> {
    let get = |prefix: &str, field: &str| {
        env.get(&format!("{prefix}_{field}"))
            .cloned()
            .unwrap_or_default()
    };
    let mut profiles = Vec::new();
    for (prefix, id, name, protocol) in [
        ("TENCENT", "tencent", "腾讯混元", ModelProtocol::OpenaiChat),
        (
            "MISTRAL",
            "mistral",
            "Mistral OCR",
            ModelProtocol::MistralOcr,
        ),
        (
            "AZURE_OPENAI",
            "azure",
            "Azure OpenAI",
            ModelProtocol::OpenaiChat,
        ),
        (
            "AZURE_OPENAI",
            "azure-responses",
            "Azure OpenAI (Responses)",
            ModelProtocol::OpenaiResponses,
        ),
    ] {
        let base_url = get(prefix, "BASE_URL");
        if base_url.is_empty() {
            continue;
        }
        let ocr_model = get(prefix, "OCR_MODEL");
        let mut translate_model = get(prefix, "TRANSLATE_MODEL");
        if translate_model.is_empty() && protocol.translates() {
            translate_model = ocr_model.clone();
        }
        profiles.push(ModelProfile {
            id: id.into(),
            name: name.into(),
            protocol,
            base_url,
            api_key: get(prefix, "API_KEY"),
            ocr_model,
            translate_model,
        });
    }
    profiles
}

fn preview(text: &str) -> String {
    let flat = text.replace('\t', " ⇥ ").replace('\n', " ⏎ ");
    let short: String = flat.chars().take(240).collect();
    if flat.chars().count() > 240 {
        format!("{short}…")
    } else {
        short
    }
}

#[test]
#[ignore = "live: sends synthetic images and text to the services configured in .env"]
fn configured_model_interfaces_recognize_and_translate() {
    let env = read_env();
    let profiles = profiles(&env);
    assert!(!profiles.is_empty(), "no services configured in .env");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut images = vec![(
        "table-fixture".to_owned(),
        std::fs::read(root.join("tests/fixtures/basic.png")).unwrap(),
    )];
    if let Some(dir) = std::env::var_os("SIGHTOCR_LIVE_FORMULAS") {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "png"))
            .collect();
        entries.sort();
        for path in entries {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            images.push((name, std::fs::read(path).unwrap()));
        }
    }
    let config = Config {
        model_profiles: profiles.clone(),
        source_lang: "en".into(),
        target_lang: "zh-Hans".into(),
        ..Config::default()
    };
    let mut services = Services::new().unwrap();
    let mut report = Vec::new();
    let mut failures = 0;
    for profile in &profiles {
        let mut cases: Vec<(String, String, Option<Vec<u8>>)> = Vec::new();
        for (name, png) in &images {
            let modes: &[&str] = if name.starts_with("formula") {
                &["formula"]
            } else {
                &["auto", "table"]
            };
            for mode in modes {
                cases.push((
                    format!("ocr:{mode}:{name}"),
                    mode.to_string(),
                    Some(png.clone()),
                ));
            }
        }
        if profile.can_translate() {
            cases.push(("translate".into(), String::new(), None));
        }
        for (case, mode, png) in cases {
            let started = Instant::now();
            let result = match png {
                Some(png) => services.ocr(&png, &format!("Model.{}_{}", profile.id, mode), &config),
                None => {
                    let mut config = config.clone();
                    config.last_translate_selection = format!("Model.{}", profile.id);
                    services.translate(
                        "Hello, world. Screenshots become editable text you can copy.",
                        &config,
                    )
                }
            };
            let seconds = started.elapsed().as_secs_f32();
            let (ok, text) = match &result {
                Ok(text) => (true, preview(text)),
                Err(error) => {
                    failures += 1;
                    (false, format!("{error:#}"))
                }
            };
            println!(
                "[{}] {:<34} {:>5.1}s {} {}",
                profile.name,
                case,
                seconds,
                if ok { "OK  " } else { "FAIL" },
                text
            );
            report.push(serde_json::json!({
                "service": profile.name, "protocol": profile.protocol, "case": case,
                "ok": ok, "seconds": seconds,
                "output": result.as_ref().ok(), "error": result.as_ref().err().map(|e| format!("{e:#}")),
            }));
        }
    }
    if let Some(path) = std::env::var_os("SIGHTOCR_LIVE_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
    assert_eq!(
        failures, 0,
        "{failures} live case(s) failed; see output above"
    );
}
