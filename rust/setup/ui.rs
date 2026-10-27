//! The setup window: a minimal Windows 11 style page per step (welcome,
//! options, progress, result) in SightOCR's own palette.

use crate::args::{Options, Quiet};
use crate::engine::{self, Failure, InstallPlan, Installed, Tasks, UninstallPlan};
use crate::env::Env;
use crate::payload::Package;
use crate::{exit, logln, win, Launcher, VERSION};
use eframe::egui::{
    self, pos2, vec2, Align, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId,
    Layout, Margin, Rect, Response, RichText, Sense, Shape, Stroke, StrokeKind, TextStyle, Ui,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

const RUNNING_NOTICE: &str = "SightOCR 正在运行。继续将自动关闭它，未保存的识别内容将丢失。";

// ------------------------------------------------------------------ entry

pub fn install(env: Env, package: Package, options: Options, launcher: Launcher) -> i32 {
    let env = Arc::new(env);
    let package = Arc::new(package);
    let update = options.update;
    // An in-app update never shows the options page.
    let quiet = if update && options.quiet == Quiet::Interactive {
        Quiet::Silent
    } else {
        options.quiet
    };
    let survey = match engine::survey(&env) {
        Ok(survey) => survey,
        Err(error) => {
            let failure = Failure {
                code: exit::PREPARE_FAILED,
                message: format!("{error:#}"),
            };
            logln!("FAILED: {}", failure.message);
            return report_failure(options.suppress_msgboxes, failure, Role::Install);
        }
    };
    let target = engine::resolve_target(&env, &survey, options.dir.as_deref(), update);
    let installed_here = survey.version_in(&target);
    let tasks = match &options.tasks {
        Some(names) => Tasks::from_names(names),
        None if installed_here.is_some() => engine::current_tasks(&env, &target),
        None => Tasks::DEFAULT,
    };
    let mut dirs = vec![target.clone()];
    dirs.extend(
        survey
            .installs
            .iter()
            .filter(|install| install.uninstaller.is_some())
            .map(|install| install.dir.clone()),
    );

    if quiet != Quiet::Interactive {
        let newer = survey
            .highest_version()
            .filter(|version| engine::compare_versions(version, VERSION).is_gt());
        if let (false, Some(newer), false) = (update, &newer, options.suppress_msgboxes) {
            if !win::confirm(&format!(
                "已安装较新的 SightOCR（v{newer}）。是否继续安装较旧的 v{VERSION}？"
            )) {
                return exit::CANCELLED;
            }
        }
        // Declined: stop. Suppressed message boxes: continue unapproved, so
        // setup fails with an explanation instead of ending SightOCR unasked.
        let approved = update || confirm_close(&dirs, options.suppress_msgboxes);
        if !approved && !options.suppress_msgboxes {
            return exit::CANCELLED;
        }
        let plan = InstallPlan {
            dir: target,
            tasks: (!update).then_some(tasks),
            update,
            approved,
        };
        let relaunch = update && !options.no_launch;
        if quiet == Quiet::VerySilent {
            return match engine::install(&env, &package, &plan, &|_, _| {}) {
                Ok(done) => {
                    if relaunch {
                        launcher.launch(&done.dir);
                    }
                    exit::OK
                }
                Err(failure) => report_failure(options.suppress_msgboxes, failure, Role::Install),
            };
        }
        let app = App::new(
            Role::Install,
            env,
            package,
            launcher,
            Mode::Auto {
                relaunch,
                suppress: options.suppress_msgboxes,
            },
        );
        return app.run_with(|app, ctx| app.start_install(ctx, plan));
    }

    let mut app = App::new(Role::Install, env, package, launcher, Mode::Interactive);
    app.survey = survey;
    app.dir_locked = installed_here.is_some() && options.dir.is_none();
    app.dir_text = target.display().to_string();
    app.tasks = tasks;
    // Debug builds only: open the options page directly for screenshots.
    #[cfg(debug_assertions)]
    if std::env::var("SIGHTOCR_SETUP_TEST_PAGE").as_deref() == Ok("options") {
        app.page = Page::Options;
    }
    app.run_with(|_, _| {})
}

pub fn uninstall(env: Env, package: Package, options: Options, dir: PathBuf) -> i32 {
    let env = Arc::new(env);
    let package = Arc::new(package);
    let dir = win::plain(&dir);
    let launcher = Launcher { result_file: None };
    if options.quiet != Quiet::Interactive {
        let approved = confirm_close(std::slice::from_ref(&dir), options.suppress_msgboxes);
        if !approved && !options.suppress_msgboxes {
            return exit::CANCELLED;
        }
        let plan = UninstallPlan {
            dir,
            remove_settings: false,
            approved,
        };
        if options.quiet == Quiet::VerySilent {
            return match engine::uninstall(&env, &package, &plan, &|_, _| {}) {
                Ok(_) => exit::OK,
                Err(failure) => report_failure(options.suppress_msgboxes, failure, Role::Uninstall),
            };
        }
        let app = App::new(
            Role::Uninstall,
            env,
            package,
            launcher,
            Mode::Auto {
                relaunch: false,
                suppress: options.suppress_msgboxes,
            },
        );
        return app.run_with(|app, ctx| app.start_uninstall(ctx, plan));
    }
    let mut app = App::new(Role::Uninstall, env, package, launcher, Mode::Interactive);
    app.dir_text = dir.display().to_string();
    app.run_with(|_, _| {})
}

/// Ask before closing a running SightOCR in silent modes. False when the
/// user declines, or when message boxes are suppressed (setup then stops
/// with an error instead of ending the program unasked).
fn confirm_close(dirs: &[PathBuf], suppress: bool) -> bool {
    match engine::running(dirs) {
        Ok(processes) if processes.is_empty() => true,
        Ok(_) if suppress => false,
        Ok(_) => win::confirm(&format!(
            "{RUNNING_NOTICE}\n\n点击“确定”继续，点击“取消”退出。"
        )),
        Err(_) => !suppress,
    }
}

fn report_failure(suppress: bool, failure: Failure, role: Role) -> i32 {
    if !suppress {
        let what = match role {
            Role::Install => "安装未完成",
            Role::Uninstall => "卸载未完成",
        };
        let log = crate::log::path()
            .map(|path| format!("\n\n日志：{}", path.display()))
            .unwrap_or_default();
        win::message_box(&format!("{what}：{}{log}", failure.message), true);
    }
    failure.code
}

// ------------------------------------------------------------------ theme

#[derive(Clone, Copy)]
struct Palette {
    canvas: Color32,
    /// Plain window background.
    window: Color32,
    panel: Color32,
    panel_alt: Color32,
    text: Color32,
    muted: Color32,
    border: Color32,
    strong_border: Color32,
    accent: Color32,
    accent_hover: Color32,
    accent_soft: Color32,
    on_accent: Color32,
    danger: Color32,
    warning_soft: Color32,
    success: Color32,
}

fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            canvas: Color32::from_rgb(32, 32, 32),
            window: Color32::from_rgb(32, 32, 32),
            panel: Color32::from_rgb(45, 45, 45),
            panel_alt: Color32::from_rgb(39, 39, 39),
            text: Color32::from_rgb(245, 245, 245),
            muted: Color32::from_rgb(180, 180, 180),
            border: Color32::from_rgb(58, 58, 58),
            strong_border: Color32::from_rgb(110, 110, 110),
            accent: Color32::from_rgb(96, 205, 255),
            accent_hover: Color32::from_rgb(86, 185, 230),
            accent_soft: Color32::from_rgb(36, 62, 77),
            on_accent: Color32::from_rgb(20, 20, 20),
            danger: Color32::from_rgb(255, 153, 164),
            warning_soft: Color32::from_rgb(67, 53, 25),
            success: Color32::from_rgb(108, 203, 95),
        }
    } else {
        Palette {
            canvas: Color32::from_rgb(248, 249, 252),
            window: Color32::WHITE,
            panel: Color32::WHITE,
            panel_alt: Color32::from_rgb(243, 243, 243),
            text: Color32::from_rgb(26, 26, 26),
            muted: Color32::from_rgb(96, 96, 96),
            border: Color32::from_rgb(229, 229, 229),
            strong_border: Color32::from_rgb(138, 138, 138),
            accent: Color32::from_rgb(0, 120, 212),
            accent_hover: Color32::from_rgb(26, 134, 217),
            accent_soft: Color32::from_rgb(230, 241, 251),
            on_accent: Color32::WHITE,
            danger: Color32::from_rgb(196, 43, 28),
            warning_soft: Color32::from_rgb(255, 244, 206),
            success: Color32::from_rgb(15, 123, 15),
        }
    }
}

fn colors(ui: &Ui) -> Palette {
    palette(ui.visuals().dark_mode)
}

fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

fn configure(ctx: &egui::Context) {
    let fonts_dir = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("Fonts");
    let mut fonts = FontDefinitions::default();
    let mut load = |key: &str, files: &[&str]| -> bool {
        for file in files {
            if let Ok(bytes) = std::fs::read(fonts_dir.join(file)) {
                fonts
                    .font_data
                    .insert(key.into(), Arc::new(FontData::from_owned(bytes)));
                return true;
            }
        }
        false
    };
    let regular: Vec<&str> = [
        load("ui", &["SegUIVar.ttf", "segoeui.ttf"]).then_some("ui"),
        load("cjk", &["msyh.ttc", "simhei.ttf", "simsun.ttc"]).then_some("cjk"),
    ]
    .into_iter()
    .flatten()
    .collect();
    let strong: Vec<&str> = [
        load("ui-bold", &["seguisb.ttf", "segoeuib.ttf"]).then_some("ui-bold"),
        load("cjk-bold", &["msyhbd.ttc", "msyh.ttc"]).then_some("cjk-bold"),
    ]
    .into_iter()
    .flatten()
    .collect();
    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    for (index, key) in regular.iter().enumerate() {
        proportional.insert(index, key.to_string());
    }
    let fallback = proportional.clone();
    let family = fonts
        .families
        .entry(FontFamily::Name("bold".into()))
        .or_default();
    family.extend(strong.iter().map(|key| key.to_string()));
    family.extend(fallback);
    ctx.set_fonts(fonts);

    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        let p = palette(theme == egui::Theme::Dark);
        ctx.style_mut_of(theme, |style| {
            style.text_styles.insert(TextStyle::Heading, bold(22.0));
            style
                .text_styles
                .insert(TextStyle::Body, FontId::proportional(14.0));
            style
                .text_styles
                .insert(TextStyle::Button, FontId::proportional(14.0));
            style
                .text_styles
                .insert(TextStyle::Small, FontId::proportional(12.0));
            style.spacing.item_spacing = vec2(8.0, 8.0);
            style.spacing.interact_size = vec2(32.0, 32.0);
            style.spacing.button_padding = vec2(12.0, 6.0);
            let v = &mut style.visuals;
            v.override_text_color = Some(p.text);
            v.panel_fill = p.canvas;
            v.window_fill = p.panel;
            v.extreme_bg_color = p.panel;
            v.faint_bg_color = p.panel_alt;
            v.selection.bg_fill = p.accent_soft;
            v.selection.stroke = Stroke::new(1.5_f32, p.accent);
            v.text_cursor.stroke = Stroke::new(1.5_f32, p.accent);
            for visual in [
                &mut v.widgets.noninteractive,
                &mut v.widgets.inactive,
                &mut v.widgets.hovered,
                &mut v.widgets.active,
                &mut v.widgets.open,
            ] {
                visual.corner_radius = CornerRadius::same(4);
                visual.expansion = 0.0;
                visual.bg_stroke = Stroke::new(1.0_f32, p.border);
                visual.fg_stroke = Stroke::new(1.0_f32, p.text);
                visual.bg_fill = p.panel;
                visual.weak_bg_fill = p.panel;
            }
            v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, p.strong_border);
            v.widgets.active.bg_stroke = Stroke::new(1.0_f32, p.accent);
        });
    }
}

// ---------------------------------------------------------------- widgets
//
// Minimal Windows 11 setup look: a plain window, a circular logo badge, one
// clear primary button and a quiet "custom options" link.

/// A Windows 11 button. Primary buttons use the accent colour.
fn button(ui: &mut Ui, label: &str, primary: bool, size: egui::Vec2) -> Response {
    let p = colors(ui);
    let font = FontId::proportional(14.0);
    let fg = if primary { p.on_accent } else { p.text };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, fg);
    let size = vec2(size.x.max(galley.size().x + 32.0), size.y);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let hovered = response.hovered();
    let (fill, stroke) = if primary {
        let fill = if response.is_pointer_button_down_on() {
            p.accent.gamma_multiply(0.85)
        } else if hovered {
            p.accent_hover
        } else {
            p.accent
        };
        (fill, Stroke::NONE)
    } else {
        (
            if hovered { p.panel_alt } else { p.panel },
            Stroke::new(1.0_f32, if hovered { p.strong_border } else { p.border }),
        )
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(4),
        fill,
        stroke,
        StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, fg);
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// Buttons side by side, centred; returns the index of a clicked button.
fn button_row(ui: &mut Ui, buttons: &[(&str, bool)], size: egui::Vec2) -> Option<usize> {
    let gap = 12.0;
    let width = buttons.len() as f32 * size.x + (buttons.len() as f32 - 1.0) * gap;
    let mut clicked = None;
    ui.allocate_ui_with_layout(
        vec2(width, size.y),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (index, (label, primary)) in buttons.iter().enumerate() {
                if button(ui, label, *primary, size).clicked() {
                    clicked = Some(index);
                }
            }
        },
    );
    clicked
}

/// A quiet accent link with a chevron in a circle, e.g. "⌄ 自定义选项".
fn options_link(ui: &mut Ui, label: &str, down: bool) -> Response {
    let p = colors(ui);
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_owned(), FontId::proportional(13.0), p.accent);
    let icon = 14.0;
    let size = vec2(
        icon + 6.0 + galley.size().x,
        galley.size().y.max(icon) + 8.0,
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let color = if response.hovered() {
        p.accent_hover
    } else {
        p.accent
    };
    let center = pos2(rect.left() + icon / 2.0, rect.center().y);
    let stroke = Stroke::new(1.3_f32, color);
    ui.painter().circle_stroke(center, icon / 2.0 - 0.5, stroke);
    let sign = if down { 1.0 } else { -1.0 };
    ui.painter().add(Shape::line(
        vec![
            center + vec2(-3.0, -1.5 * sign),
            center + vec2(0.0, 1.5 * sign),
            center + vec2(3.0, -1.5 * sign),
        ],
        stroke,
    ));
    ui.painter().galley(
        pos2(
            rect.left() + icon + 6.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        color,
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

fn checkbox(ui: &mut Ui, checked: &mut bool, label: &str, size: f32) -> Response {
    let p = colors(ui);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), FontId::proportional(size), p.text);
    let box_size = (size + 4.0).round();
    let row = vec2(
        box_size + 10.0 + galley.size().x,
        box_size.max(galley.size().y) + 8.0,
    );
    let (rect, mut response) = ui.allocate_exact_size(row, Sense::click());
    if response.clicked() {
        *checked = !*checked;
        response.mark_changed();
    }
    let hovered = response.hovered();
    let square = Rect::from_center_size(
        pos2(rect.left() + box_size / 2.0, rect.center().y),
        vec2(box_size, box_size),
    );
    if *checked {
        let fill = if hovered { p.accent_hover } else { p.accent };
        ui.painter()
            .rect_filled(square, CornerRadius::same(4), fill);
        let at = |x: f32, y: f32| square.min + vec2(x, y) * (box_size / 18.0);
        ui.painter().add(Shape::line(
            vec![at(4.5, 9.5), at(7.5, 12.5), at(13.5, 5.5)],
            Stroke::new(1.8_f32, p.on_accent),
        ));
    } else {
        ui.painter().rect(
            square,
            CornerRadius::same(4),
            if hovered { p.panel_alt } else { p.panel },
            Stroke::new(1.0_f32, p.strong_border),
            StrokeKind::Inside,
        );
    }
    ui.painter().galley(
        pos2(
            square.right() + 10.0,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        p.text,
    );
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// A short warning in a soft pill, centred under the main button.
fn notice(ui: &mut Ui, text: &str) {
    let p = colors(ui);
    egui::Frame::new()
        .fill(p.warning_soft)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(12, 7))
        .show(ui, |ui| {
            ui.set_max_width(440.0);
            ui.add(egui::Label::new(RichText::new(text).size(12.5).color(p.text)).wrap());
        });
}

fn progress_bar(ui: &mut Ui, fraction: f32, width: f32) {
    let p = colors(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 4.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(2), p.border);
    let mut filled = rect;
    filled.set_width(rect.width() * fraction.clamp(0.0, 1.0));
    if filled.width() > 0.5 {
        ui.painter()
            .rect_filled(filled, CornerRadius::same(2), p.accent);
    }
}

#[derive(Clone, Copy)]
enum Badge {
    Success,
    Error,
}

fn badge(ui: &mut Ui, kind: Badge) {
    let p = colors(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(72.0, 72.0), Sense::hover());
    let center = rect.center();
    let color = match kind {
        Badge::Success => p.success,
        Badge::Error => p.danger,
    };
    ui.painter().circle_filled(center, 32.0, color);
    let stroke = Stroke::new(
        3.5_f32,
        if ui.visuals().dark_mode {
            p.canvas
        } else {
            Color32::WHITE
        },
    );
    match kind {
        Badge::Success => {
            ui.painter().add(Shape::line(
                vec![
                    center + vec2(-12.0, 0.5),
                    center + vec2(-4.0, 8.5),
                    center + vec2(13.0, -9.0),
                ],
                stroke,
            ));
        }
        Badge::Error => {
            ui.painter()
                .line_segment([center + vec2(0.0, -14.0), center + vec2(0.0, 4.0)], stroke);
            ui.painter()
                .circle_filled(center + vec2(0.0, 13.0), 2.5, stroke.color);
        }
    }
}

/// A title followed by a small, quiet version tag, centred.
fn title_with_tag(ui: &mut Ui, title: &str, tag: &str) {
    let p = colors(ui);
    let name = ui
        .painter()
        .layout_no_wrap(title.to_owned(), bold(26.0), p.text);
    let label = ui
        .painter()
        .layout_no_wrap(tag.to_owned(), FontId::proportional(12.5), p.muted);
    let tag_size = label.size() + vec2(14.0, 6.0);
    let size = vec2(
        name.size().x + 10.0 + tag_size.x,
        name.size().y.max(tag_size.y),
    );
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().galley(
        pos2(rect.left(), rect.center().y - name.size().y / 2.0),
        name,
        p.text,
    );
    let tag_rect = Rect::from_min_size(
        pos2(
            rect.right() - tag_size.x,
            rect.center().y - tag_size.y / 2.0 + 2.0,
        ),
        tag_size,
    );
    ui.painter()
        .rect_filled(tag_rect, CornerRadius::same(5), p.panel_alt);
    ui.painter()
        .galley(tag_rect.center() - label.size() / 2.0, label, p.muted);
}

fn heading(ui: &mut Ui, text: &str) {
    let p = colors(ui);
    ui.label(RichText::new(text).font(bold(22.0)).color(p.text));
}

fn caption(ui: &mut Ui, text: &str, size: f32) {
    let p = colors(ui);
    ui.add(egui::Label::new(RichText::new(text).size(size).color(p.muted)).wrap());
}

// -------------------------------------------------------------------- app

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Install,
    Uninstall,
}

enum Mode {
    Interactive,
    /// Progress only; close by itself when done.
    Auto {
        relaunch: bool,
        suppress: bool,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Welcome,
    /// Install location and options ("自定义选项").
    Options,
    Working,
    Finished,
    Failed,
}

enum Done {
    Installed(Installed),
    Uninstalled(Vec<String>),
}

#[derive(Default)]
struct Shared {
    fraction: f32,
    status: String,
    finished: Option<Result<Done, Failure>>,
}

struct App {
    role: Role,
    mode: Mode,
    env: Arc<Env>,
    package: Arc<Package>,
    launcher: Launcher,
    exit_code: Arc<AtomicI32>,
    page: Page,
    shared: Arc<Mutex<Shared>>,
    icon: Option<egui::TextureHandle>,
    // Welcome and options
    survey: engine::Survey,
    dir_text: String,
    dir_locked: bool,
    dir_error: Option<String>,
    tasks: Tasks,
    remove_settings: bool,
    close_approved: bool,
    notice: Option<String>,
    // Result
    launch_now: bool,
    done: Option<Done>,
    failure: Option<Failure>,
}

impl App {
    fn new(
        role: Role,
        env: Arc<Env>,
        package: Arc<Package>,
        launcher: Launcher,
        mode: Mode,
    ) -> Self {
        Self {
            role,
            mode,
            env,
            package,
            launcher,
            exit_code: Arc::new(AtomicI32::new(exit::CANCELLED)),
            page: Page::Welcome,
            shared: Arc::default(),
            icon: None,
            survey: engine::Survey::default(),
            dir_text: String::new(),
            dir_locked: false,
            dir_error: None,
            tasks: Tasks::DEFAULT,
            remove_settings: false,
            close_approved: false,
            notice: None,
            launch_now: true,
            done: None,
            failure: None,
        }
    }

    /// Show the window; `start` runs once on the first frame (auto modes).
    fn run_with(self, start: impl FnOnce(&mut App, &egui::Context) + 'static) -> i32 {
        let exit_code = self.exit_code.clone();
        let title = match self.role {
            Role::Install => "SightOCR 安装",
            Role::Uninstall => "SightOCR 卸载",
        };
        let mut viewport = egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size([640.0, 460.0])
            .with_resizable(false)
            .with_maximize_button(false);
        if let Some(icon) = icon_rgba() {
            viewport = viewport.with_icon(Arc::new(egui::IconData {
                rgba: icon.0,
                width: icon.1,
                height: icon.2,
            }));
        }
        let options = eframe::NativeOptions {
            viewport,
            centered: true,
            ..Default::default()
        };
        let mut start = Some(start);
        let result = eframe::run_native(
            title,
            options,
            Box::new(move |creation| {
                configure(&creation.egui_ctx);
                let mut app = self;
                if let Some(start) = start.take() {
                    start(&mut app, &creation.egui_ctx);
                }
                Ok(Box::new(app))
            }),
        );
        if let Err(error) = result {
            logln!("Window failed: {error}");
            win::message_box(&format!("无法显示安装窗口：{error}"), true);
            return exit::INIT_FAILED;
        }
        exit_code.load(Ordering::SeqCst)
    }

    fn spawn_work(
        &mut self,
        ctx: &egui::Context,
        work: impl FnOnce(&Env, &Package, engine::Progress) -> Result<Done, Failure> + Send + 'static,
    ) {
        let (env, package, shared, ctx) = (
            self.env.clone(),
            self.package.clone(),
            self.shared.clone(),
            ctx.clone(),
        );
        self.page = Page::Working;
        std::thread::spawn(move || {
            let progress = |fraction: f32, status: &str| {
                if let Ok(mut shared) = shared.lock() {
                    shared.fraction = fraction;
                    shared.status = status.to_owned();
                }
                ctx.request_repaint();
            };
            let result = work(&env, &package, &progress);
            if let Ok(mut shared) = shared.lock() {
                shared.finished = Some(result);
            }
            ctx.request_repaint();
        });
    }

    fn start_install(&mut self, ctx: &egui::Context, plan: InstallPlan) {
        self.spawn_work(ctx, move |env, package, progress| {
            engine::install(env, package, &plan, progress).map(Done::Installed)
        });
    }

    fn start_uninstall(&mut self, ctx: &egui::Context, plan: UninstallPlan) {
        self.spawn_work(ctx, move |env, package, progress| {
            engine::uninstall(env, package, &plan, progress).map(Done::Uninstalled)
        });
    }

    fn close(&self, ctx: &egui::Context, code: i32) {
        self.exit_code.store(code, Ordering::SeqCst);
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn installed_version_here(&self) -> Option<String> {
        self.survey.version_in(Path::new(self.dir_text.trim()))
    }

    fn newer_installed(&self) -> Option<String> {
        self.survey
            .highest_version()
            .filter(|version| engine::compare_versions(version, VERSION).is_gt())
    }

    /// The main action: install (or uninstall).
    fn proceed(&mut self, ctx: &egui::Context) {
        let dir = match self.role {
            Role::Install => match engine::validate_target(Path::new(&self.dir_text)) {
                Ok(dir) => dir,
                Err(error) => {
                    // The location is edited under the custom options.
                    self.dir_error = Some(format!("{error:#}"));
                    self.page = Page::Options;
                    return;
                }
            },
            Role::Uninstall => PathBuf::from(&self.dir_text),
        };
        self.dir_error = None;
        let mut dirs = vec![dir.clone()];
        if self.role == Role::Install {
            dirs.extend(
                self.survey
                    .installs
                    .iter()
                    .filter(|install| install.uninstaller.is_some())
                    .map(|install| install.dir.clone()),
            );
        }
        let running = engine::running(&dirs)
            .map(|list| !list.is_empty())
            .unwrap_or(false);
        if running && !self.close_approved {
            self.close_approved = true;
            self.notice = Some(RUNNING_NOTICE.into());
            return;
        }
        match self.role {
            Role::Install => self.start_install(
                ctx,
                InstallPlan {
                    dir,
                    tasks: Some(self.tasks),
                    update: false,
                    approved: true,
                },
            ),
            Role::Uninstall => self.start_uninstall(
                ctx,
                UninstallPlan {
                    dir,
                    remove_settings: self.remove_settings,
                    approved: true,
                },
            ),
        }
    }

    /// Leave the custom options, keeping the choices (the folder must be valid).
    fn collapse_options(&mut self) {
        match engine::validate_target(Path::new(&self.dir_text)) {
            Ok(dir) => {
                self.dir_text = dir.display().to_string();
                self.dir_error = None;
                self.page = Page::Welcome;
            }
            Err(error) => self.dir_error = Some(format!("{error:#}")),
        }
    }

    fn finish(&mut self, ctx: &egui::Context) {
        if let (Role::Install, true, Some(Done::Installed(installed))) =
            (self.role, self.launch_now, &self.done)
        {
            self.launcher.launch(&installed.dir);
        }
        self.close(ctx, exit::OK);
    }

    fn close_failed(&self, ctx: &egui::Context) {
        let code = self
            .failure
            .as_ref()
            .map_or(exit::FATAL, |failure| failure.code);
        self.close(ctx, code);
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let finished = self
            .shared
            .lock()
            .ok()
            .and_then(|mut shared| shared.finished.take());
        let Some(result) = finished else {
            return;
        };
        match result {
            Ok(done) => {
                if let Mode::Auto { relaunch, .. } = self.mode {
                    if let (true, Done::Installed(installed)) = (relaunch, &done) {
                        self.launcher.launch(&installed.dir);
                    }
                    self.close(ctx, exit::OK);
                }
                self.exit_code.store(exit::OK, Ordering::SeqCst);
                self.done = Some(done);
                self.page = Page::Finished;
            }
            Err(failure) => {
                self.exit_code.store(failure.code, Ordering::SeqCst);
                if let Mode::Auto { suppress: true, .. } = self.mode {
                    self.close(ctx, failure.code);
                }
                self.failure = Some(failure);
                self.page = Page::Failed;
            }
        }
    }

    /// SightOCR's icon in a white circular badge with a soft shadow.
    fn logo(&mut self, ui: &mut Ui, size: f32) {
        let p = colors(ui);
        let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
        if self.icon.is_none() {
            self.icon = icon_rgba().map(|(rgba, width, height)| {
                ui.ctx().load_texture(
                    "icon",
                    egui::ColorImage::from_rgba_unmultiplied(
                        [width as usize, height as usize],
                        &rgba,
                    ),
                    egui::TextureOptions::LINEAR,
                )
            });
        }
        let shadow = egui::Shadow {
            offset: [0, 6],
            blur: 24,
            spread: 0,
            color: Color32::from_black_alpha(if ui.visuals().dark_mode { 110 } else { 30 }),
        };
        ui.painter()
            .add(shadow.as_shape(rect, CornerRadius::same((size / 2.0) as u8)));
        // The logo is a dark mark on white, so the badge stays white in both themes.
        ui.painter()
            .circle_filled(rect.center(), size / 2.0, Color32::WHITE);
        if let Some(icon) = &self.icon {
            ui.painter().image(
                icon.id(),
                rect.shrink(size * 0.08),
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        ui.painter().circle_stroke(
            rect.center(),
            size / 2.0 - 0.5,
            Stroke::new(1.0_f32, p.border),
        );
    }

    fn install_label(&self) -> &'static str {
        let here = self.installed_version_here();
        match (&self.notice, self.newer_installed(), &here) {
            (Some(_), _, _) => "关闭并安装",
            (_, Some(_), _) => "仍然安装",
            (_, _, Some(version)) if engine::compare_versions(version, VERSION).is_eq() => {
                "重新安装"
            }
            (_, _, Some(_)) => "立即升级",
            _ => "立即安装",
        }
    }

    /// One quiet line under the main button: what will happen, or a warning.
    fn install_note(&self, ui: &mut Ui) {
        if let Some(text) = &self.notice {
            notice(ui, text);
        } else if let Some(newer) = self.newer_installed() {
            notice(
                ui,
                &format!("已安装较新的 v{newer}，继续将替换为较旧的 v{VERSION}。"),
            );
        } else if let Some(version) = self.installed_version_here() {
            let text = if engine::compare_versions(&version, VERSION).is_eq() {
                "将修复当前安装，设置、API 密钥和模型接口均会保留。".to_owned()
            } else {
                format!("将从 v{version} 升级，设置、API 密钥和模型接口均会保留。")
            };
            caption(ui, &text, 12.5);
        }
    }

    fn welcome_page(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let p = colors(ui);
        ui.add_space(64.0);
        self.logo(ui, 112.0);
        ui.add_space(22.0);
        match self.role {
            Role::Install => {
                title_with_tag(ui, "SightOCR", VERSION);
                ui.add_space(32.0);
                if button(ui, self.install_label(), true, vec2(180.0, 40.0)).clicked() {
                    self.proceed(ctx);
                }
                ui.add_space(16.0);
                self.install_note(ui);
            }
            Role::Uninstall => {
                title_with_tag(ui, "卸载 SightOCR", &self.package.index.version);
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(RichText::new(&self.dir_text).size(12.5).color(p.muted))
                        .truncate(),
                );
                ui.add_space(18.0);
                checkbox(ui, &mut self.remove_settings, "同时删除设置", 12.5).on_hover_text(
                    "包括已保存的 API 密钥、模型接口、快捷键和偏好设置。不勾选则保留，重新安装后可继续使用。",
                );
                ui.add_space(12.0);
                let primary = if self.notice.is_some() {
                    "关闭并卸载"
                } else {
                    "卸载"
                };
                match button_row(ui, &[("取消", false), (primary, true)], vec2(140.0, 40.0)) {
                    Some(0) => self.close(ctx, exit::CANCELLED),
                    Some(_) => self.proceed(ctx),
                    None => {}
                }
                if let Some(text) = self.notice.clone() {
                    ui.add_space(14.0);
                    notice(ui, &text);
                }
            }
        }
    }

    fn options_page(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let p = colors(ui);
        ui.add_space(40.0);
        heading(ui, "自定义选项");
        ui.add_space(22.0);
        ui.allocate_ui_with_layout(vec2(440.0, 200.0), Layout::top_down(Align::Min), |ui| {
            ui.label(RichText::new("安装位置").size(12.0).color(p.muted));
            ui.add_space(2.0);
            if self.dir_locked {
                egui::Frame::new()
                    .fill(p.panel_alt)
                    .stroke(Stroke::new(1.0_f32, p.border))
                    .corner_radius(CornerRadius::same(4))
                    .inner_margin(Margin::symmetric(10, 7))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.add(
                            egui::Label::new(RichText::new(&self.dir_text).color(p.text))
                                .truncate(),
                        );
                    });
                caption(ui, "升级时沿用当前安装位置。", 12.0);
            } else {
                ui.horizontal(|ui| {
                    let edit = egui::TextEdit::singleline(&mut self.dir_text)
                        .desired_width(ui.available_width() - 96.0 - 8.0)
                        .margin(Margin::symmetric(8, 7))
                        .font(FontId::proportional(14.0));
                    if ui.add(edit).changed() {
                        self.dir_error = None;
                    }
                    if button(ui, "浏览…", false, vec2(96.0, 32.0)).clicked() {
                        if let Some(folder) =
                            win::browse_folder(window_handle(), "选择 SightOCR 的安装位置")
                        {
                            let named = folder
                                .file_name()
                                .is_some_and(|name| name.eq_ignore_ascii_case("SightOCR"));
                            let chosen = if named {
                                folder
                            } else {
                                folder.join("SightOCR")
                            };
                            self.dir_text = chosen.display().to_string();
                            self.dir_error = None;
                        }
                    }
                });
            }
            if let Some(error) = &self.dir_error {
                ui.label(RichText::new(error).size(12.5).color(p.danger));
            }
            ui.add_space(12.0);
            egui::Grid::new("options")
                .num_columns(2)
                .spacing(vec2(24.0, 0.0))
                .show(ui, |ui| {
                    checkbox(ui, &mut self.tasks.start_menu, "创建开始菜单快捷方式", 13.0);
                    checkbox(ui, &mut self.tasks.desktop, "创建桌面快捷方式", 13.0);
                    ui.end_row();
                    checkbox(ui, &mut self.tasks.autostart, "开机自动启动", 13.0);
                    checkbox(
                        ui,
                        &mut self.tasks.run_as_admin,
                        "始终以管理员身份运行",
                        13.0,
                    )
                    .on_hover_text("启动 SightOCR 时需要 Windows 确认。");
                    ui.end_row();
                    checkbox(ui, &mut self.launch_now, "安装完成后运行 SightOCR", 13.0);
                    ui.end_row();
                });
        });
        ui.add_space(22.0);
        if button(ui, self.install_label(), true, vec2(180.0, 40.0)).clicked() {
            self.proceed(ctx);
        }
        if self.notice.is_some() || self.newer_installed().is_some() {
            ui.add_space(12.0);
            self.install_note(ui);
        }
    }

    fn working_page(&mut self, ui: &mut Ui) {
        let (fraction, status) = self
            .shared
            .lock()
            .map(|shared| (shared.fraction, shared.status.clone()))
            .unwrap_or_default();
        let title = match self.role {
            Role::Install if matches!(self.mode, Mode::Auto { relaunch: true, .. }) => {
                "正在更新 SightOCR"
            }
            Role::Install if self.installed_version_here().is_some() => "正在升级 SightOCR",
            Role::Install => "正在安装 SightOCR",
            Role::Uninstall => "正在卸载 SightOCR",
        };
        ui.add_space(64.0);
        self.logo(ui, 112.0);
        ui.add_space(26.0);
        heading(ui, title);
        ui.add_space(4.0);
        let status = if status.is_empty() {
            "正在准备"
        } else {
            &status
        };
        caption(ui, &format!("{status} · {:.0}%", fraction * 100.0), 13.0);
        ui.add_space(26.0);
        progress_bar(ui, fraction, 360.0);
    }

    fn result_page(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let p = colors(ui);
        let (kind, title, lines): (Badge, &str, Vec<String>) = match (&self.done, &self.failure) {
            (Some(Done::Installed(installed)), _) => {
                let mut lines = vec![match &installed.previous {
                    Some(previous) if engine::compare_versions(previous, VERSION).is_ne() => {
                        format!("SightOCR 已从 v{previous} 升级到 v{VERSION}，原有设置均已保留。")
                    }
                    Some(_) => format!("SightOCR v{VERSION} 已重新安装，原有设置均已保留。"),
                    None => format!("SightOCR v{VERSION} 已安装到 {}。", installed.dir.display()),
                }];
                lines.extend(
                    installed
                        .warnings
                        .iter()
                        .map(|warning| format!("注意：{warning}")),
                );
                let title = if installed.previous.is_some() {
                    "升级完成"
                } else {
                    "安装完成"
                };
                (Badge::Success, title, lines)
            }
            (Some(Done::Uninstalled(warnings)), _) => {
                let mut lines = vec![if self.remove_settings {
                    "SightOCR 及其设置已从此电脑删除。".to_owned()
                } else {
                    "设置已保留，重新安装后可继续使用。".to_owned()
                }];
                lines.extend(warnings.iter().map(|warning| format!("注意：{warning}")));
                (Badge::Success, "SightOCR 已卸载", lines)
            }
            (None, Some(failure)) => {
                let mut lines = vec![failure.message.clone()];
                if let Some(path) = crate::log::path() {
                    lines.push(format!("日志：{}", path.display()));
                }
                let title = match self.role {
                    Role::Install => "安装未完成",
                    Role::Uninstall => "卸载未完成",
                };
                (Badge::Error, title, lines)
            }
            (None, None) => return,
        };
        ui.add_space(64.0);
        badge(ui, kind);
        ui.add_space(20.0);
        heading(ui, title);
        ui.add_space(6.0);
        ui.allocate_ui_with_layout(vec2(460.0, 64.0), Layout::top_down(Align::Center), |ui| {
            for (index, line) in lines.iter().enumerate() {
                let size = if index == 0 { 14.0 } else { 12.0 };
                ui.add(egui::Label::new(RichText::new(line).size(size).color(p.muted)).wrap());
            }
        });
        ui.add_space(16.0);
        if self.done.is_some() {
            if self.role == Role::Install {
                checkbox(ui, &mut self.launch_now, "立即运行 SightOCR", 12.5);
                ui.add_space(8.0);
            }
            if button(ui, "完成", true, vec2(180.0, 40.0)).clicked() {
                self.finish(ctx);
            }
        } else if button(ui, "关闭", true, vec2(180.0, 40.0)).clicked() {
            self.close_failed(ctx);
        }
    }

    /// The centred "custom options" link at the bottom of the install pages.
    fn footer(&mut self, ui: &mut Ui) {
        if !matches!(self.mode, Mode::Interactive) || self.role != Role::Install {
            return;
        }
        let expand = match self.page {
            Page::Welcome => true,
            Page::Options => false,
            _ => return,
        };
        let label = if expand {
            "自定义选项"
        } else {
            "收起选项"
        };
        let clicked = ui
            .vertical_centered(|ui| options_link(ui, label, expand).clicked())
            .inner;
        if clicked && expand {
            self.page = Page::Options;
        } else if clicked {
            self.collapse_options();
        }
    }

    /// Enter presses the main button, Esc cancels, as in a Windows dialog.
    fn keyboard(&mut self, ctx: &egui::Context) {
        let (enter, escape) = ctx.input(|input| {
            (
                input.key_pressed(egui::Key::Enter),
                input.key_pressed(egui::Key::Escape),
            )
        });
        match self.page {
            Page::Welcome | Page::Options if enter => self.proceed(ctx),
            Page::Welcome if escape => self.close(ctx, exit::CANCELLED),
            Page::Options if escape => self.collapse_options(),
            Page::Finished if enter => self.finish(ctx),
            Page::Failed if enter || escape => self.close_failed(ctx),
            _ => {}
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.page == Page::Working {
            self.poll(ctx);
        }
        self.keyboard(ctx);
        if ctx.input(|input| input.viewport().close_requested()) {
            match self.page {
                Page::Working => ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose),
                Page::Welcome | Page::Options => {
                    self.exit_code.store(exit::CANCELLED, Ordering::SeqCst)
                }
                Page::Finished => self.exit_code.store(exit::OK, Ordering::SeqCst),
                Page::Failed => {
                    let code = self
                        .failure
                        .as_ref()
                        .map_or(exit::FATAL, |failure| failure.code);
                    self.exit_code.store(code, Ordering::SeqCst);
                }
            }
        }
        let p = palette(ctx.style().visuals.dark_mode);
        egui::TopBottomPanel::bottom("footer")
            .exact_height(64.0)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(p.window).inner_margin(Margin {
                left: 22,
                right: 22,
                top: 0,
                bottom: 22,
            }))
            .show(ctx, |ui| self.footer(ui));
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(p.window)
                    .inner_margin(Margin::symmetric(32, 0)),
            )
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| match self.page {
                    Page::Welcome => self.welcome_page(ui, ctx),
                    Page::Options => self.options_page(ui, ctx),
                    Page::Working => self.working_page(ui),
                    Page::Finished | Page::Failed => self.result_page(ui, ctx),
                });
            });
    }
}

fn window_handle() -> isize {
    // SAFETY: GetActiveWindow only reads the calling thread's active window.
    unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow() as isize }
}

fn icon_rgba() -> Option<(Vec<u8>, u32, u32)> {
    let image = image::load_from_memory(include_bytes!("../../assets/icon.png")).ok()?;
    let image = image
        .resize(256, 256, image::imageops::FilterType::Lanczos3)
        .to_rgba8();
    let (width, height) = image.dimensions();
    Some((image.into_raw(), width, height))
}
