use crate::{config::Config, ocr::OneOcr, services::Services};
use anyhow::{Context, Result};
use image::RgbaImage;
use serde::{Deserialize, Serialize};
use std::{
    io::Cursor,
    path::{Path, PathBuf},
};

mod process;
pub use process::{run_subprocess, Worker, SUBPROCESS_ARG};

pub enum Task {
    Recognize { image: RgbaImage, translate: bool },
    Translate(String),
}

pub struct Request {
    pub id: u64,
    pub config: Config,
    pub task: Task,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Output {
    pub id: u64,
    pub recognized: Option<String>,
    pub translated: Option<String>,
    pub warning: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProgressStage {
    Recognizing,
    LocalFallback,
    Translating,
}

#[derive(Serialize, Deserialize)]
pub struct Progress {
    pub id: u64,
    pub stage: ProgressStage,
    pub recognized: Option<String>,
    pub warning: Option<String>,
}

pub struct Engine {
    resources: PathBuf,
    local: Option<OneOcr>,
    services: Option<Services>,
}

impl Engine {
    pub fn new(resources: PathBuf) -> Self {
        Self {
            resources,
            local: None,
            services: None,
        }
    }

    fn services(&mut self) -> Result<&mut Services> {
        if self.services.is_none() {
            self.services = Some(Services::new()?);
        }
        Ok(self.services.as_mut().expect("services initialized"))
    }

    fn local(&mut self, image: &RgbaImage, table: bool) -> Result<String> {
        if self.local.is_none() {
            self.local = Some(OneOcr::new(&self.resources)?);
        }
        self.local
            .as_mut()
            .expect("OCR initialized")
            .recognize(image, table)
    }

    pub fn execute(&mut self, request: Request, output: &mut Output) -> Result<()> {
        self.execute_cancellable(request, output, || false)
    }

    fn execute_cancellable(
        &mut self,
        request: Request,
        output: &mut Output,
        cancelled: impl Fn() -> bool,
    ) -> Result<()> {
        self.execute_with_progress(request, output, cancelled, |_| {})
    }

    fn execute_with_progress(
        &mut self,
        request: Request,
        output: &mut Output,
        cancelled: impl Fn() -> bool,
        progress: impl Fn(Progress),
    ) -> Result<()> {
        anyhow::ensure!(!cancelled(), "任务已取消");
        let id = request.id;
        let config = request.config;
        match request.task {
            Task::Translate(text) => {
                anyhow::ensure!(!text.trim().is_empty(), "请输入需要翻译的文字");
                progress(Progress {
                    id,
                    stage: ProgressStage::Translating,
                    recognized: None,
                    warning: None,
                });
                anyhow::ensure!(!cancelled(), "任务已取消");
                output.translated = Some(
                    self.services()?
                        .translate_cancellable(&text, &config, &cancelled)?,
                );
            }
            Task::Recognize { image, translate } => {
                anyhow::ensure!(image.width() > 0 && image.height() > 0, "截图区域为空");
                anyhow::ensure!(
                    u64::from(image.width()) * u64::from(image.height()) <= 100_000_000,
                    "图片过大（最多 1 亿像素）"
                );
                let selection = &config.last_ocr_selection;
                let table = selection.ends_with("_table");
                progress(Progress {
                    id,
                    stage: ProgressStage::Recognizing,
                    recognized: None,
                    warning: None,
                });
                anyhow::ensure!(!cancelled(), "任务已取消");
                let text = if is_cloud_ocr(selection) {
                    let mut png = Cursor::new(Vec::new());
                    image.write_to(&mut png, image::ImageFormat::Png)?;
                    anyhow::ensure!(!cancelled(), "任务已取消");
                    match self.services()?.ocr_cancellable(
                        png.get_ref(),
                        selection,
                        &config,
                        &cancelled,
                    ) {
                        Ok(text) => text,
                        Err(error) => {
                            anyhow::ensure!(!cancelled(), "任务已取消");
                            output.warning = Some(format!(
                                "云端识别失败，已使用本地识别（保留原接口设置）：{error:#}"
                            ));
                            progress(Progress {
                                id,
                                stage: ProgressStage::LocalFallback,
                                recognized: None,
                                warning: output.warning.clone(),
                            });
                            anyhow::ensure!(!cancelled(), "任务已取消");
                            self.local(&image, table)?
                        }
                    }
                } else {
                    self.local(&image, table)?
                };
                anyhow::ensure!(!text.trim().is_empty(), "未识别到文字，请选择更清晰的区域");
                let text = normalize_text(
                    &text,
                    config.replace_newline && !table && !selection.ends_with("_formula"),
                );
                output.recognized = Some(text.clone());
                if translate {
                    anyhow::ensure!(!cancelled(), "任务已取消");
                    progress(Progress {
                        id,
                        stage: ProgressStage::Translating,
                        recognized: Some(text.clone()),
                        warning: output.warning.clone(),
                    });
                    anyhow::ensure!(!cancelled(), "任务已取消");
                    output.translated = Some(
                        self.services()?
                            .translate_cancellable(&text, &config, &cancelled)?,
                    );
                }
            }
        }
        Ok(())
    }
}

fn is_cloud_ocr(selection: &str) -> bool {
    ["Baidu_", "Tencent_", "Mistral_", "OpenAI_", "Nvidia_"]
        .iter()
        .any(|prefix| selection.starts_with(prefix))
}

/// Never join table rows: tabs and newlines carry spreadsheet structure.
pub fn normalize_text(text: &str, join_lines: bool) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    if join_lines {
        text.lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        text
    }
}

pub fn resources_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("SIGHTOCR_RESOURCES") {
        return PathBuf::from(path);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let adjacent = parent.join("resources/oneocr");
            if adjacent.is_dir() {
                return adjacent;
            }
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/oneocr")
}

pub fn load_image(path: &Path) -> Result<RgbaImage> {
    let mut reader = image::ImageReader::open(path)
        .context("无法打开图片")?
        .with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(30_000);
    limits.max_image_height = Some(30_000);
    limits.max_alloc = Some(400_000_000);
    reader.limits(limits);
    let image = reader
        .decode()
        .context("图片损坏、不受支持或超过内存限制")?
        .into_rgba8();
    anyhow::ensure!(
        u64::from(image.width()) * u64::from(image.height()) <= 100_000_000,
        "图片过大（最多 1 亿像素）"
    );
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_newlines_are_normalized_but_table_layout_survives() {
        assert_eq!(normalize_text("A\tB\r\n1\t2", false), "A\tB\n1\t2");
        assert_eq!(
            normalize_text("  hello\r\n\r\n world  ", true),
            "hello world"
        );
    }
    #[test]
    fn empty_translation_is_rejected_without_network() {
        let mut engine = Engine::new(PathBuf::new());
        let request = Request {
            id: 7,
            config: Config::default(),
            task: Task::Translate(" \n".into()),
        };
        let mut output = Output::default();
        assert!(engine.execute(request, &mut output).is_err());
        assert!(engine.services.is_none());
        assert!(output.translated.is_none());
    }

    #[test]
    fn cancelled_job_never_initializes_engines_or_network() {
        let mut engine = Engine::new(PathBuf::new());
        let request = Request {
            id: 1,
            config: Config::default(),
            task: Task::Translate("hello".into()),
        };
        assert!(engine
            .execute_cancellable(request, &mut Output::default(), || true)
            .is_err());
        assert!(engine.services.is_none());
        assert!(engine.local.is_none());
    }

    #[test]
    fn cancellation_at_progress_notification_prevents_starting_recognition_or_translation() {
        for task in [
            Task::Translate("hello".into()),
            Task::Recognize {
                image: RgbaImage::new(2, 2),
                translate: true,
            },
        ] {
            let mut engine = Engine::new(PathBuf::new());
            let cancelled = std::cell::Cell::new(false);
            let count = std::cell::Cell::new(0);
            let mut output = Output::default();
            let result = engine.execute_with_progress(
                Request {
                    id: 19,
                    config: Config::default(),
                    task,
                },
                &mut output,
                || cancelled.get(),
                |progress| {
                    assert_eq!(progress.id, 19);
                    assert!(progress.recognized.is_none());
                    count.set(count.get() + 1);
                    cancelled.set(true);
                },
            );
            assert!(result.is_err());
            assert_eq!(count.get(), 1);
            assert!(engine.local.is_none() && engine.services.is_none());
            assert!(output.recognized.is_none() && output.translated.is_none());
        }
    }

    #[test]
    fn ai_ocr_routes_to_the_selected_cloud_and_cancellation_prevents_all_engines() {
        for provider in ["Mistral", "OpenAI", "Nvidia"] {
            for mode in ["auto", "table", "formula"] {
                let selection = format!("{provider}_{mode}");
                assert!(is_cloud_ocr(&selection));
                let mut engine = Engine::new(PathBuf::new());
                let request = Request {
                    id: 1,
                    config: Config {
                        last_ocr_selection: selection,
                        ..Config::default()
                    },
                    task: Task::Recognize {
                        image: RgbaImage::new(1, 1),
                        translate: true,
                    },
                };
                assert!(engine
                    .execute_cancellable(request, &mut Output::default(), || true)
                    .is_err());
                assert!(engine.local.is_none() && engine.services.is_none());
            }
        }
        assert!(!is_cloud_ocr("默认") && !is_cloud_ocr("默认_table"));
    }

    #[test]
    #[ignore = "requires bundled OneOCR DLL/model"]
    fn bundled_cancellation_after_ocr_does_not_start_translation() {
        let mut engine = Engine::new(resources_dir());
        let image = image::load_from_memory(include_bytes!("../tests/fixtures/basic.png"))
            .unwrap()
            .into_rgba8();
        let request = Request {
            id: 1,
            config: Config::default(),
            task: Task::Recognize {
                image,
                translate: true,
            },
        };
        let cancelled = std::cell::Cell::new(false);
        let stages = std::cell::RefCell::new(Vec::new());
        let mut output = Output::default();
        let result = engine.execute_with_progress(
            request,
            &mut output,
            || cancelled.get(),
            |progress| {
                stages.borrow_mut().push(progress.stage);
                if progress.stage == ProgressStage::Translating {
                    assert!(progress.recognized.as_deref().unwrap().contains("Alpha"));
                    cancelled.set(true);
                }
            },
        );
        assert!(result.is_err());
        assert_eq!(
            *stages.borrow(),
            vec![ProgressStage::Recognizing, ProgressStage::Translating]
        );
        assert!(output.recognized.as_deref().unwrap().contains("Alpha"));
        assert!(output.translated.is_none());
        assert!(
            engine.services.is_none(),
            "Cancellation must not initialize HTTP or send text"
        );
    }
}
