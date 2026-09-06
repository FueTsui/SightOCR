//! Thread-affine OneOCR FFI and lightweight TSV table reconstruction.
//!
//! Construct, call and drop `OneOcr` on the OCR worker. The Rc marker intentionally
//! prevents Send/Sync: COM ownership cannot be transferred by protecting calls
//! with a mutex.

use anyhow::{bail, ensure, Context, Result};
use image::RgbaImage;
use std::ffi::{c_char, c_void, CStr, CString, OsString};
use std::marker::PhantomData;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::ptr;
use std::rc::Rc;
use windows_sys::Win32::Globalization::{
    GetACP, WideCharToMultiByte, CP_UTF8, WC_NO_BEST_FIT_CHARS,
};
use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
use windows_sys::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows_sys::Win32::System::LibraryLoader::{
    LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

mod table;

// Enlarge only small table captures. The resampled RGBA and BGRA buffers each
// use at most 24 MB, with room for Lanczos's intermediate data under the existing
// OCR image limit. Large captures and ordinary text OCR retain their input size.
const TABLE_UPSCALE_MAX_EDGE: u32 = 1600;
const TABLE_UPSCALE_MAX_PIXELS: u64 = 1_500_000;
const TABLE_UPSCALE_FACTOR: u32 = 2;

type Handle = *mut c_void;
type CreateOptions = unsafe extern "C" fn(*mut Handle) -> u32;
type Release = unsafe extern "C" fn(Handle) -> u32;
type SetDelayLoad = unsafe extern "C" fn(Handle, c_char) -> u32;
type CreatePipeline =
    unsafe extern "C" fn(*const c_char, *const c_char, Handle, *mut Handle) -> u32;
type SetLineCount = unsafe extern "C" fn(Handle, u32) -> u32;
type SetResolution = unsafe extern "C" fn(Handle, u32, u32) -> u32;
type RunPipeline = unsafe extern "C" fn(Handle, *const ImageInfo, Handle, *mut Handle) -> u32;
type GetLineCount = unsafe extern "C" fn(Handle, *mut u64) -> u32;
type GetLine = unsafe extern "C" fn(Handle, u64, *mut Handle) -> u32;
type GetContent = unsafe extern "C" fn(Handle, *mut *const c_char) -> u32;
type GetBox = unsafe extern "C" fn(Handle, *mut *const BoundingBox) -> u32;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Point {
    x: f32,
    y: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct BoundingBox {
    top_left: Point,
    top_right: Point,
    bottom_right: Point,
    bottom_left: Point,
}

#[repr(C)]
struct ImageInfo {
    image_type: u32,
    width: u32,
    height: u32,
    // The ABI has 4 bytes of padding before these 64-bit fields on Windows x64.
    stride: u64,
    data_pointer: u64,
}

struct Api {
    create_init: CreateOptions,
    set_delay: SetDelayLoad,
    create_pipeline: CreatePipeline,
    create_process: CreateOptions,
    set_line_count: SetLineCount,
    set_resolution: SetResolution,
    run: RunPipeline,
    line_count: GetLineCount,
    line: GetLine,
    content: GetContent,
    bounding_box: GetBox,
    release_result: Release,
    release_process: Release,
    release_pipeline: Release,
    release_init: Release,
}

impl Api {
    unsafe fn load(dll: &libloading::os::windows::Library) -> Result<Self> {
        // SAFETY: these are the exported C signatures of the bundled OneOCR DLL.
        // Copies of the symbols are used only while the owning library is alive.
        macro_rules! symbol {
            ($name:literal) => {
                // SAFETY: the caller supplies the bundled library and the
                // destination field fixes this export's exact C signature.
                *unsafe { dll.get(concat!($name, "\0").as_bytes()) }
                    .with_context(|| concat!("OneOCR DLL 缺少导出函数 ", $name))?
            };
        }
        Ok(Self {
            create_init: symbol!("CreateOcrInitOptions"),
            set_delay: symbol!("OcrInitOptionsSetUseModelDelayLoad"),
            create_pipeline: symbol!("CreateOcrPipeline"),
            create_process: symbol!("CreateOcrProcessOptions"),
            set_line_count: symbol!("OcrProcessOptionsSetMaxRecognitionLineCount"),
            set_resolution: symbol!("OcrProcessOptionsSetResizeResolution"),
            run: symbol!("RunOcrPipeline"),
            line_count: symbol!("GetOcrLineCount"),
            line: symbol!("GetOcrLine"),
            content: symbol!("GetOcrLineContent"),
            bounding_box: symbol!("GetOcrLineBoundingBox"),
            release_result: symbol!("ReleaseOcrResult"),
            release_process: symbol!("ReleaseOcrProcessOptions"),
            release_pipeline: symbol!("ReleaseOcrPipeline"),
            release_init: symbol!("ReleaseOcrInitOptions"),
        })
    }
}

struct NativeHandle {
    raw: Handle,
    release: Release,
}

impl NativeHandle {
    fn empty(release: Release) -> Self {
        Self {
            raw: ptr::null_mut(),
            release,
        }
    }

    fn verify(&self, operation: &str) -> Result<()> {
        ensure!(!self.raw.is_null(), "{operation} 返回空句柄");
        Ok(())
    }
}

impl Drop for NativeHandle {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // SAFETY: each owning handle is released exactly once, with its
            // matching release function, before its DLL is unloaded.
            unsafe { (self.release)(self.raw) };
        }
    }
}

struct ComApartment(PhantomData<Rc<()>>);

impl ComApartment {
    fn new() -> Result<Self> {
        // SAFETY: initialization and matching uninitialization occur on this
        // thread. S_FALSE is also success and owns an uninitialization call.
        let result = unsafe { CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        ensure!(
            result >= 0,
            "初始化 OCR COM 环境失败: 0x{:08X}",
            result as u32
        );
        Ok(Self(PhantomData))
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: the apartment is !Send and only exists after successful init.
        unsafe { CoUninitialize() };
    }
}

/// A local OCR engine. Must be created and retained inside the OCR worker.
pub struct OneOcr {
    // Fields are dropped in declaration order. Preserve handles → DLLs → COM.
    process: NativeHandle,
    pipeline: NativeHandle,
    _init: NativeHandle,
    api: Api,
    _dll: libloading::os::windows::Library,
    _onnx: libloading::os::windows::Library,
    _com: ComApartment,
}

impl OneOcr {
    pub fn new(resources_dir: &Path) -> Result<Self> {
        ensure!(
            cfg!(target_pointer_width = "64"),
            "本地 OneOCR 需要 64 位程序"
        );
        let directory = resources_dir
            .canonicalize()
            .with_context(|| format!("找不到 OneOCR 资源目录: {}", resources_dir.display()))?;
        for name in ["oneocr.dll", "onnxruntime.dll", "oneocr.onemodel"] {
            ensure!(
                directory.join(name).is_file(),
                "缺少 OneOCR 文件: {}",
                directory.join(name).display()
            );
        }
        let model = model_path_acp(&directory.join("oneocr.onemodel"))?;
        let com = ComApartment::new()?;
        let flags = LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32;
        // SAFETY: absolute paths and constrained dependency lookup avoid loading
        // another DLL from the working directory/PATH. Libraries outlive handles.
        let onnx = unsafe {
            libloading::os::windows::Library::load_with_flags(
                directory.join("onnxruntime.dll"),
                flags,
            )
        }
        .context("加载 onnxruntime.dll 失败")?;
        // SAFETY: this absolute bundled DLL path uses the same restricted search.
        let dll = unsafe {
            libloading::os::windows::Library::load_with_flags(directory.join("oneocr.dll"), flags)
        }
        .context("加载 oneocr.dll 失败")?;
        // SAFETY: the loaded bundled library exports the documented C ABI.
        let api = unsafe { Api::load(&dll) }?;
        let mut init = NativeHandle::empty(api.release_init);
        check(
            // SAFETY: each out pointer references a live owned slot. Installing
            // its guard before the call cleans up handles returned on failure.
            unsafe { (api.create_init)(&mut init.raw) },
            "CreateOcrInitOptions",
        )?;
        init.verify("CreateOcrInitOptions")?;
        check(
            // SAFETY: init is a live, non-null handle owned by this thread.
            unsafe { (api.set_delay)(init.raw, 0) },
            "OcrInitOptionsSetUseModelDelayLoad",
        )?;
        let mut pipeline = NativeHandle::empty(api.release_pipeline);
        let license = b"kj)TGtrK>f]b[Piow.gU+nC@s\"\"\"\"\"\"4\0";
        check(
            // SAFETY: strings are NUL-terminated and handles/out slots are live.
            unsafe {
                (api.create_pipeline)(
                    model.as_ptr(),
                    license.as_ptr().cast(),
                    init.raw,
                    &mut pipeline.raw,
                )
            },
            "CreateOcrPipeline",
        )?;
        pipeline.verify("CreateOcrPipeline")?;
        let mut process = NativeHandle::empty(api.release_process);
        check(
            // SAFETY: process.raw is a live out slot protected by a release guard.
            unsafe { (api.create_process)(&mut process.raw) },
            "CreateOcrProcessOptions",
        )?;
        process.verify("CreateOcrProcessOptions")?;
        check(
            // SAFETY: process is the live options handle returned by this DLL.
            unsafe { (api.set_line_count)(process.raw, 1000) },
            "OcrProcessOptionsSetMaxRecognitionLineCount",
        )?;
        check(
            // SAFETY: process is live and both configured dimensions are nonzero.
            unsafe { (api.set_resolution)(process.raw, 1152, 768) },
            "OcrProcessOptionsSetResizeResolution",
        )?;
        Ok(Self {
            process,
            pipeline,
            _init: init,
            api,
            _dll: dll,
            _onnx: onnx,
            _com: com,
        })
    }

    pub fn recognize(&mut self, image: &RgbaImage, table: bool) -> Result<String> {
        let (pixels, width, height, input_scale) = prepare_recognition_bgra(image, table)?;
        let info = ImageInfo {
            image_type: 3,
            width,
            height,
            stride: u64::from(width) * 4,
            data_pointer: pixels.as_ptr() as u64,
        };
        let mut result = NativeHandle::empty(self.api.release_result);
        check(
            // SAFETY: pixel storage, options and pipeline remain alive for the
            // synchronous call. The result guard precedes every fallible read.
            unsafe { (self.api.run)(self.pipeline.raw, &info, self.process.raw, &mut result.raw) },
            "RunOcrPipeline",
        )?;
        result.verify("RunOcrPipeline")?;
        let mut count = 0;
        check(
            // SAFETY: result is live and count is a writable u64 output slot.
            unsafe { (self.api.line_count)(result.raw, &mut count) },
            "GetOcrLineCount",
        )?;
        ensure!(count <= 1000, "OneOCR 返回的行数超出配置上限: {count}");
        let mut texts = Vec::with_capacity(count as usize);
        let mut lines = Vec::with_capacity(count as usize);
        for index in 0..count {
            let mut line = ptr::null_mut();
            check(
                // SAFETY: index is within the returned count and result is live.
                unsafe { (self.api.line)(result.raw, index, &mut line) },
                "GetOcrLine",
            )?;
            ensure!(!line.is_null(), "OneOCR 返回空文本行");
            let mut content = ptr::null();
            check(
                // SAFETY: line is non-null and borrowed from the live result.
                unsafe { (self.api.content)(line, &mut content) },
                "GetOcrLineContent",
            )?;
            ensure!(!content.is_null(), "OneOCR 返回空文本指针");
            // SAFETY: content is NUL-terminated UTF-8 owned by the live result.
            // Copy before releasing that result; preserve malformed bytes as �.
            let text = unsafe { CStr::from_ptr(content) }
                .to_string_lossy()
                .into_owned();
            if table {
                let mut bounds = ptr::null();
                check(
                    // SAFETY: line and the bounding-box output slot are valid.
                    unsafe { (self.api.bounding_box)(line, &mut bounds) },
                    "GetOcrLineBoundingBox",
                )?;
                ensure!(!bounds.is_null(), "OneOCR 返回空文本边界框");
                // SAFETY: the successful call returns a non-null, aligned
                // BoundingBox borrowed from result; copy it before release.
                let bounds = bounds_in_original_image(unsafe { *bounds }, input_scale);
                let rect = table::Rect::from_box(bounds)?;
                lines.push(table::TextLine { text, rect });
            } else {
                texts.push(text);
            }
        }
        Ok(if table {
            table::reconstruct(&lines, image)
        } else {
            texts.join("\n")
        })
    }
}

fn check(code: u32, operation: &str) -> Result<()> {
    ensure!(code == 0, "{operation} 失败，错误码: 0x{code:08X}");
    Ok(())
}

/// OneOCR takes a narrow ACP path, not UTF-8 (unless Windows uses UTF-8 ACP).
/// Reject best-fit substitutions, then try the filesystem's short path alias.
fn model_path_acp(path: &Path) -> Result<CString> {
    // Narrow CRT paths do not reliably understand canonicalize's verbatim prefix.
    let path = path.as_os_str().encode_wide().collect::<Vec<_>>();
    let path = if path.starts_with(&[92, 92, 63, 92, 85, 78, 67, 92]) {
        [&[92, 92][..], &path[8..]].concat()
    } else if path.starts_with(&[92, 92, 63, 92]) {
        path[4..].to_vec()
    } else {
        path
    };
    if let Some(encoded) = encode_acp(&path)? {
        return Ok(encoded);
    }
    let mut terminated = path.clone();
    terminated.push(0);
    // SAFETY: input is NUL-terminated and output sizes are queried first.
    let required = unsafe { GetShortPathNameW(terminated.as_ptr(), ptr::null_mut(), 0) };
    if required > 0 {
        let mut short = vec![0u16; required as usize];
        // SAFETY: output has the capacity just requested from this API.
        let length =
            unsafe { GetShortPathNameW(terminated.as_ptr(), short.as_mut_ptr(), required) };
        if length > 0 && length < required {
            short.truncate(length as usize);
            if let Some(encoded) = encode_acp(&short)? {
                return Ok(encoded);
            }
        }
    }
    bail!("OneOCR 模型路径无法用当前 Windows 代码页表示，且没有可用短路径。请将资源目录移到纯英文路径后重试: {}", OsString::from_wide(&path).to_string_lossy())
}

fn encode_acp(wide: &[u16]) -> Result<Option<CString>> {
    ensure!(!wide.contains(&0), "模型路径包含 NUL 字符");
    let size = i32::try_from(wide.len()).context("模型路径过长")?;
    // SAFETY: GetACP has no arguments or preconditions.
    let cp = unsafe { GetACP() };
    let flags = if cp == CP_UTF8 {
        0
    } else {
        WC_NO_BEST_FIT_CHARS
    };
    let mut substituted = 0;
    let used = if cp == CP_UTF8 {
        ptr::null_mut()
    } else {
        &mut substituted
    };
    // SAFETY: explicit input length matches the UTF-16 buffer. Output is null
    // for the sizing pass; UTF-8 requires a null default-character pointer.
    let required = unsafe {
        WideCharToMultiByte(
            cp,
            flags,
            wide.as_ptr(),
            size,
            ptr::null_mut(),
            0,
            ptr::null(),
            used,
        )
    };
    ensure!(
        required > 0,
        "转换模型路径编码失败: {}",
        std::io::Error::last_os_error()
    );
    let mut bytes = vec![0u8; required as usize];
    // SAFETY: output capacity matches the sizing result and input is unchanged.
    let length = unsafe {
        WideCharToMultiByte(
            cp,
            flags,
            wide.as_ptr(),
            size,
            bytes.as_mut_ptr(),
            required,
            ptr::null(),
            used,
        )
    };
    ensure!(
        length == required,
        "转换模型路径编码失败: {}",
        std::io::Error::last_os_error()
    );
    if substituted != 0 {
        return Ok(None);
    }
    Ok(Some(CString::new(bytes).context("模型路径编码包含 NUL")?))
}

fn prepare_bgra(image: &RgbaImage) -> Result<(Vec<u8>, u32, u32)> {
    ensure!(image.width() > 0 && image.height() > 0, "不能识别空图片");
    let width = image.width().max(50);
    let height = image.height().max(50);
    let length = u64::from(width) * u64::from(height) * 4;
    ensure!(
        length <= 256 * 1024 * 1024,
        "图片过大，请缩小到 6400 万像素以内再识别"
    );
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(length as usize)
        .context("没有足够内存处理 OCR 图片")?;
    pixels.resize(length as usize, 255);
    for (x, y, pixel) in image.enumerate_pixels() {
        let offset = (y as usize * width as usize + x as usize) * 4;
        let alpha = u32::from(pixel[3]);
        for (destination, source) in [2, 1, 0].into_iter().enumerate() {
            pixels[offset + destination] =
                ((u32::from(pixel[source]) * alpha + 255 * (255 - alpha) + 127) / 255) as u8;
        }
    }
    Ok((pixels, width, height))
}

fn table_upscale_dimensions(width: u32, height: u32) -> Option<(u32, u32)> {
    if width == 0
        || height == 0
        || width.max(height) > TABLE_UPSCALE_MAX_EDGE
        || u64::from(width) * u64::from(height) > TABLE_UPSCALE_MAX_PIXELS
    {
        return None;
    }
    Some((width * TABLE_UPSCALE_FACTOR, height * TABLE_UPSCALE_FACTOR))
}

fn prepare_recognition_bgra(image: &RgbaImage, table: bool) -> Result<(Vec<u8>, u32, u32, f32)> {
    if table {
        if let Some((width, height)) = table_upscale_dimensions(image.width(), image.height()) {
            // The bundled DLL rejects larger ResizeResolution settings. Upscale
            // the input instead so small isolated digits retain usable glyphs.
            let enlarged = image::imageops::resize(
                image,
                width,
                height,
                image::imageops::FilterType::Lanczos3,
            );
            let (pixels, width, height) = prepare_bgra(&enlarged)?;
            // `enlarged` is released here, before the synchronous OCR call.
            return Ok((pixels, width, height, TABLE_UPSCALE_FACTOR as f32));
        }
    }
    let (pixels, width, height) = prepare_bgra(image)?;
    Ok((pixels, width, height, 1.0))
}

fn bounds_in_original_image(mut bounds: BoundingBox, scale: f32) -> BoundingBox {
    // Padding to OneOCR's minimum 50x50 size does not scale the source image.
    // Use the explicit enlargement factor, not padded-width/source-width.
    for point in [
        &mut bounds.top_left,
        &mut bounds.top_right,
        &mut bounds.bottom_right,
        &mut bounds.bottom_left,
    ] {
        point.x /= scale;
        point.y /= scale;
    }
    bounds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_layout_matches_x64_oneocr() {
        assert_eq!(std::mem::size_of::<Point>(), 8);
        assert_eq!(std::mem::size_of::<BoundingBox>(), 32);
        assert_eq!(std::mem::size_of::<ImageInfo>(), 32);
        assert_eq!(std::mem::offset_of!(ImageInfo, stride), 16);
        assert_eq!(std::mem::offset_of!(ImageInfo, data_pointer), 24);
    }

    #[test]
    fn pads_small_images_and_composites_alpha_before_bgra() {
        let input = RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 128]));
        let (pixels, width, height) = prepare_bgra(&input).unwrap();
        assert_eq!((width, height), (50, 50));
        assert_eq!(&pixels[..4], &[127, 127, 255, 255]);
        assert!(pixels[4..].iter().all(|&value| value == 255));
    }

    #[test]
    fn ascii_model_path_round_trips() {
        let path = Path::new(r"C:\OCR\oneocr.onemodel");
        assert_eq!(
            model_path_acp(path).unwrap().to_bytes(),
            b"C:\\OCR\\oneocr.onemodel"
        );
    }

    #[test]
    fn empty_image_is_a_typed_error() {
        assert!(prepare_bgra(&RgbaImage::new(0, 0)).is_err());
        assert!(prepare_recognition_bgra(&RgbaImage::new(0, 0), true).is_err());
    }

    #[test]
    fn table_enlargement_respects_edge_and_pixel_budgets() {
        assert_eq!(table_upscale_dimensions(967, 613), Some((1934, 1226)));
        assert_eq!(table_upscale_dimensions(1600, 937), Some((3200, 1874)));
        assert_eq!(table_upscale_dimensions(1000, 1500), Some((2000, 3000)));
        for (width, height) in [
            (1601, 500),
            (500, 1601),
            (1001, 1500),
            (1500, 1001),
            (u32::MAX, u32::MAX),
            (0, 50),
            (50, 0),
        ] {
            assert_eq!(table_upscale_dimensions(width, height), None);
        }
    }

    #[test]
    fn table_mode_enlarges_pixels_without_changing_text_mode_or_padding_scale() {
        let input = RgbaImage::from_pixel(20, 30, image::Rgba([10, 20, 30, 255]));
        let (plain, width, height, scale) = prepare_recognition_bgra(&input, false).unwrap();
        assert_eq!((width, height, scale), (50, 50, 1.0));
        assert_eq!(&plain[..4], &[30, 20, 10, 255]);
        let (table, width, height, scale) = prepare_recognition_bgra(&input, true).unwrap();
        assert_eq!((width, height, scale), (50, 60, 2.0));
        assert_eq!(&table[..4], &[30, 20, 10, 255]);
        assert_eq!(&table[40 * 4..41 * 4], &[255, 255, 255, 255]);
        assert_eq!(input.dimensions(), (20, 30));
    }

    #[test]
    fn enlarged_ocr_corners_map_back_to_original_rule_coordinates() {
        let bounds = BoundingBox {
            top_left: Point { x: 10.0, y: 20.0 },
            top_right: Point { x: 38.0, y: 22.0 },
            bottom_right: Point { x: 36.0, y: 50.0 },
            bottom_left: Point { x: 8.0, y: 48.0 },
        };
        let mapped = bounds_in_original_image(bounds, 2.0);
        assert_eq!((mapped.top_left.x, mapped.top_left.y), (5.0, 10.0));
        assert_eq!((mapped.top_right.x, mapped.top_right.y), (19.0, 11.0));
        assert_eq!((mapped.bottom_right.x, mapped.bottom_right.y), (18.0, 25.0));
        assert_eq!((mapped.bottom_left.x, mapped.bottom_left.y), (4.0, 24.0));
        let original = bounds_in_original_image(bounds, 1.0);
        assert_eq!(
            (original.bottom_right.x, original.bottom_right.y),
            (36.0, 50.0)
        );
    }

    #[test]
    #[ignore = "Requires bundled Windows OneOCR DLLs/model; run with --ignored"]
    fn bundled_engine_recognizes_fixture_and_releases_repeatedly() {
        std::thread::spawn(|| {
            let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/oneocr");
            let image = image::load_from_memory(include_bytes!("../tests/fixtures/basic.png"))
                .unwrap()
                .to_rgba8();
            for _ in 0..2 {
                let mut engine = OneOcr::new(&directory).unwrap();
                let text = engine.recognize(&image, false).unwrap();
                assert!(
                    text.contains("Alpha") && text.contains("100"),
                    "Unexpected OCR: {text}"
                );
                let table = engine.recognize(&image, true).unwrap();
                assert!(
                    table.contains("Alpha\t100"),
                    "Unexpected table OCR: {table}"
                );
                // Drop on the same thread before constructing another engine.
            }
        })
        .join()
        .unwrap();
    }

    #[test]
    #[ignore = "Requires bundled Windows OneOCR DLLs/model; run with --ignored"]
    fn bundled_engine_table_keeps_columns_with_faint_rules_and_scaled_input() {
        std::thread::spawn(|| {
            let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/oneocr");
            let mut engine = OneOcr::new(&directory).unwrap();
            let mut image = image::load_from_memory(include_bytes!("../tests/fixtures/basic.png"))
                .unwrap()
                .to_rgba8();
            // Retain real glyph pixels while making the fixture's known rules
            // pale and discontinuous, as in compressed spreadsheet captures.
            for (x, y, pixel) in image.enumerate_pixels_mut() {
                let horizontal = [9, 10, 89, 90, 169, 170, 249, 250].contains(&y);
                let vertical = [9, 10, 369, 370, 709, 710].contains(&x);
                if (horizontal || vertical) && pixel[0] < 100 {
                    let along = if horizontal { x } else { y };
                    let gray = if along % 40 < 36 { 232 } else { 255 };
                    *pixel = image::Rgba([gray, gray, gray, 255]);
                }
            }
            for factor in [1, 3] {
                let scaled = image::imageops::resize(
                    &image,
                    image.width() * factor,
                    image.height() * factor,
                    image::imageops::FilterType::Triangle,
                );
                let table = engine.recognize(&scaled, true).unwrap();
                assert_eq!(
                    table, "Name\tAmount\nAlpha\t100\nBeta\t200",
                    "scale={factor}"
                );
            }
        })
        .join()
        .unwrap();
    }
}
