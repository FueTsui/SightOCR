//! Sample already displayed text without waking or repainting the UI being tested.

#![cfg(any(debug_assertions, test))]

use super::live_display_smoke;
use anyhow::{ensure, Context, Result};
use eframe::egui::{Color32, ColorImage};
use serde::Serialize;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

const DURATION: Duration = Duration::from_secs(6);
const MAX_DURATION: Duration = Duration::from_secs(12);
const BASELINE_TIMEOUT: Duration = Duration::from_millis(900);
const MIN_SAMPLES: usize = 60;
const CHANNEL_TOLERANCE: u8 = 8;

#[derive(Debug, Serialize)]
pub(super) struct SampleMetrics {
    pub elapsed_ms: u64,
    pub changed_pixels: usize,
    pub missing_ink_pixels: usize,
}

#[derive(Debug, Serialize)]
pub(super) struct Report {
    /// Number of frames compared against the stable baseline.
    pub samples: usize,
    pub acquired_frames: usize,
    pub duration_ms: u64,
    pub max_changed_pixels: usize,
    /// Maximum disappearing baseline ink pixels in any sampled frame.
    pub missing_ink_pixels: usize,
    pub baseline_at_ms: Option<u64>,
    pub baseline_ink_pixels: usize,
    pub background: Option<[u8; 3]>,
    pub body_pixels: [i32; 4],
    pub first: PathBuf,
    pub worst: PathBuf,
    pub last: PathBuf,
    pub error: Option<String>,
    pub frames: Vec<SampleMetrics>,
}

pub(super) struct Run {
    receiver: Receiver<Result<Report>>,
    stop: Arc<AtomicBool>,
    finished: bool,
    directory: PathBuf,
}

/// body_pixels is [client_x, client_y, width, height], all physical pixels.
/// on_complete may request one egui repaint to collect the result. It is invoked
/// once, after sampling and evidence writing; it cannot affect sampled frames.
fn start(
    hwnd: usize,
    body_pixels: [i32; 4],
    directory: PathBuf,
    on_complete: impl FnOnce() + Send + 'static,
) -> Result<Run> {
    ensure!(hwnd != 0, "文字稳定性检查未指定窗口");
    ensure!(
        body_pixels[0] >= 0 && body_pixels[1] >= 0 && body_pixels[2] > 0 && body_pixels[3] > 0,
        "文字稳定性检查需要有效的物理像素客户区矩形"
    );
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let worker_directory = directory.clone();
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("sightocr-text-stability".into())
        .spawn(move || {
            let result = measure(hwnd, body_pixels, &worker_directory, &worker_stop);
            let _ = sender.send(result);
            on_complete();
        })
        .context("无法启动文字稳定性检查线程")?;
    Ok(Run {
        receiver,
        stop,
        finished: false,
        directory,
    })
}

impl Run {
    pub(super) fn start(
        hwnd: usize,
        body_pixels: [i32; 4],
        directory: PathBuf,
        on_complete: impl FnOnce() + Send + 'static,
    ) -> Result<Self> {
        start(hwnd, body_pixels, directory, on_complete)
    }

    /// Never waits or joins a thread on the UI thread.
    pub(super) fn try_finish(&mut self) -> Option<Result<Report>> {
        if self.finished {
            return None;
        }
        match self.receiver.try_recv() {
            Ok(result) => {
                self.finished = true;
                Some(result)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.finished = true;
                Some(Err(anyhow::anyhow!(
                    "文字稳定性检查线程提前退出；已有证据目录：{}",
                    self.directory.display()
                )))
            }
        }
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn measure(hwnd: usize, body: [i32; 4], directory: &Path, stop: &AtomicBool) -> Result<Report> {
    std::fs::create_dir_all(directory).context("无法建立文字稳定性证据目录")?;
    let started = Instant::now();
    let mut report = Report {
        samples: 0,
        acquired_frames: 0,
        duration_ms: 0,
        max_changed_pixels: 0,
        missing_ink_pixels: 0,
        baseline_at_ms: None,
        baseline_ink_pixels: 0,
        background: None,
        body_pixels: body,
        first: directory.join("first.png"),
        worst: directory.join("worst.png"),
        last: directory.join("last.png"),
        error: None,
        frames: Vec::with_capacity(200),
    };
    let mut first_seen = None;
    let mut candidate: Option<(ColorImage, u64)> = None;
    let mut baseline: Option<ColorImage> = None;
    let mut background = Color32::BLACK;
    let mut ink_indices = Vec::new();
    let mut worst = None;
    let mut worst_score = (0, 0);
    let mut last = None;
    let measured = (|| -> Result<()> {
        while started.elapsed() < MAX_DURATION
            && (started.elapsed() < DURATION || report.frames.len() < MIN_SAMPLES)
        {
            ensure!(!stop.load(Ordering::Acquire), "文字稳定性检查已停止");
            // capture only reads this known foreground process's display DC. It
            // handles cross-thread DPI and DwmFlush; never post a UI input/paint.
            let image = crop(&live_display_smoke::capture(hwnd)?, body)?;
            let elapsed_ms = started.elapsed().as_millis() as u64;
            report.acquired_frames += 1;
            if first_seen.is_none() {
                first_seen = Some(image.clone());
            }
            if baseline.is_none() {
                if let Some((previous, timestamp)) = candidate.as_ref() {
                    let bg = dominant_background(&image);
                    let ink = ink_pixels(&image, bg);
                    // Two matching text-bearing frames establish the first
                    // baseline. Empty/black buffers cannot become a passing one.
                    if changed_pixels(previous, &image) <= 4 && ink.len() >= 16 {
                        baseline = Some(previous.clone());
                        background = bg;
                        ink_indices = ink_pixels(previous, bg);
                        report.baseline_at_ms = Some(*timestamp);
                        report.baseline_ink_pixels = ink_indices.len();
                        let rgba = bg.to_array();
                        report.background = Some([rgba[0], rgba[1], rgba[2]]);
                    }
                }
                if baseline.is_none() {
                    candidate = Some((image.clone(), elapsed_ms));
                    last = Some(image);
                    ensure!(
                        started.elapsed() < BASELINE_TIMEOUT,
                        "正文未能形成两个连续稳定且包含文字的基准帧"
                    );
                    continue;
                }
                // Keep only a few images, not every captured frame.
                candidate = None;
                first_seen = None;
            }
            let reference = baseline.as_ref().expect("stable baseline");
            let changed = changed_pixels(reference, &image);
            let missing = missing_ink(&image, background, &ink_indices);
            let score = (changed, missing);
            if worst.is_none() || score > worst_score {
                worst = Some(image.clone());
                worst_score = score;
            }
            report.max_changed_pixels = report.max_changed_pixels.max(changed);
            report.missing_ink_pixels = report.missing_ink_pixels.max(missing);
            report.frames.push(SampleMetrics {
                elapsed_ms,
                changed_pixels: changed,
                missing_ink_pixels: missing,
            });
            last = Some(image);
        }
        ensure!(baseline.is_some(), "没有获取到可比较的稳定文字基准帧");
        ensure!(
            report.frames.len() >= MIN_SAMPLES,
            "{} 秒内仅获得 {} 个可比较帧，少于要求的 {MIN_SAMPLES} 帧",
            started.elapsed().as_secs(),
            report.frames.len()
        );
        Ok(())
    })();
    report.samples = report.frames.len();
    report.duration_ms = started.elapsed().as_millis() as u64;
    report.error = measured.err().map(|error| format!("{error:#}"));
    // Save evidence even if capture lost foreground, the baseline never settled,
    // or the sampling rate was insufficient. No error is converted to success.
    let first = baseline.as_ref().or(first_seen.as_ref()).or(last.as_ref());
    let worst = worst.as_ref().or(last.as_ref()).or(first);
    let final_image = last.as_ref().or(first);
    for (path, image) in [
        (&report.first, first),
        (&report.worst, worst),
        (&report.last, final_image),
    ] {
        if let Some(image) = image {
            if let Err(error) = save_png(path, image) {
                let message = format!("保存证据失败：{error:#}");
                report.error = Some(match report.error.take() {
                    Some(previous) => format!("{previous}；{message}"),
                    None => message,
                });
            }
        }
    }
    let report_path = directory.join("report.json");
    std::fs::write(&report_path, serde_json::to_vec_pretty(&report)?)
        .with_context(|| format!("无法保存稳定性报告：{}", report_path.display()))?;
    if let Some(error) = &report.error {
        anyhow::bail!("{error}；证据和报告：{}", report_path.display());
    }
    Ok(report)
}

fn crop(image: &ColorImage, rect: [i32; 4]) -> Result<ColorImage> {
    let [x, y, width, height] = rect;
    ensure!(
        x >= 0 && y >= 0 && width > 0 && height > 0,
        "正文截图矩形无效"
    );
    let (x, y, width, height) = (x as usize, y as usize, width as usize, height as usize);
    ensure!(
        x.checked_add(width).is_some_and(|end| end <= image.width())
            && y.checked_add(height)
                .is_some_and(|end| end <= image.height()),
        "正文区域已移出客户区或窗口在采样期间改变了尺寸"
    );
    let mut pixels = Vec::with_capacity(width * height);
    for row in y..y + height {
        let start = row * image.width() + x;
        pixels.extend_from_slice(&image.pixels[start..start + width]);
    }
    Ok(ColorImage {
        size: [width, height],
        pixels,
    })
}

fn dominant_background(image: &ColorImage) -> Color32 {
    let mut counts = HashMap::new();
    // A body is overwhelmingly its panel color. A regular subsample avoids a
    // large hash table and scans even very tall result panes efficiently.
    for pixel in image.pixels.iter().step_by(31) {
        *counts.entry(*pixel).or_insert(0usize) += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(color, _)| color)
        .unwrap_or(Color32::BLACK)
}

fn distance(a: Color32, b: Color32) -> u8 {
    let a = a.to_array();
    let b = b.to_array();
    a[0].abs_diff(b[0])
        .max(a[1].abs_diff(b[1]))
        .max(a[2].abs_diff(b[2]))
}

fn changed_pixels(reference: &ColorImage, image: &ColorImage) -> usize {
    reference
        .pixels
        .iter()
        .zip(&image.pixels)
        .filter(|(before, after)| distance(**before, **after) > CHANNEL_TOLERANCE)
        .count()
}

fn ink_pixels(image: &ColorImage, background: Color32) -> Vec<usize> {
    image
        .pixels
        .iter()
        .enumerate()
        .filter_map(|(index, pixel)| (distance(*pixel, background) >= 48).then_some(index))
        .collect()
}

fn missing_ink(image: &ColorImage, background: Color32, ink_indices: &[usize]) -> usize {
    ink_indices
        .iter()
        .filter(|index| distance(image.pixels[**index], background) <= 16)
        .count()
}

fn save_png(path: &Path, image: &ColorImage) -> Result<()> {
    let bytes: Vec<u8> = image
        .pixels
        .iter()
        .flat_map(|pixel| pixel.to_array())
        .collect();
    image::save_buffer_with_format(
        path,
        &bytes,
        image.width() as u32,
        image.height() as u32,
        image::ColorType::Rgba8,
        image::ImageFormat::Png,
    )
    .with_context(|| format!("无法保存 {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_local_disappearing_glyphs_and_black_frames_in_both_themes() {
        for (background, text) in [
            (Color32::WHITE, Color32::BLACK),
            (Color32::from_gray(38), Color32::WHITE),
        ] {
            let mut baseline = ColorImage::new([100, 40], background);
            for row in 10..20 {
                for col in 10..25 {
                    baseline.pixels[row * 100 + col] = text;
                }
            }
            let bg = dominant_background(&baseline);
            assert_eq!(bg, background);
            let ink = ink_pixels(&baseline, bg);
            assert_eq!(ink.len(), 150);
            assert_eq!(changed_pixels(&baseline, &baseline), 0);
            let erased = ColorImage::new([100, 40], background);
            assert_eq!(changed_pixels(&baseline, &erased), 150);
            assert_eq!(missing_ink(&erased, bg, &ink), 150);
            assert!(changed_pixels(&baseline, &ColorImage::new([100, 40], Color32::BLACK)) > 150);
        }
    }

    #[test]
    fn crops_only_the_supplied_client_region_and_rejects_resizing() {
        let mut full = ColorImage::new([5, 4], Color32::BLACK);
        full.pixels[2 * 5 + 3] = Color32::WHITE;
        let body = crop(&full, [2, 1, 2, 2]).unwrap();
        assert_eq!(body.size, [2, 2]);
        assert_eq!(body.pixels[3], Color32::WHITE);
        assert!(crop(&full, [2, 1, 4, 2]).is_err());
        assert!(crop(&full, [-1, 0, 1, 1]).is_err());
    }
}
