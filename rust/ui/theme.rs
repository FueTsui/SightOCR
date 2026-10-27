//! Fluent-inspired typography, neutral surfaces, and compact Windows desktop controls.

use super::motion;
use eframe::egui::{
    self, pos2, vec2, Color32, Context, CornerRadius, FontId, Rect, Response, Sense, Shape, Stroke,
    StrokeKind, TextStyle, Ui, WidgetInfo, WidgetType,
};

#[derive(Clone, Copy)]
pub struct Palette {
    pub canvas: Color32,
    pub panel: Color32,
    pub panel_alt: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub subtle: Color32,
    pub border: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub accent_soft: Color32,
    pub sidebar: Color32,
    pub danger: Color32,
    pub warning: Color32,
    pub success: Color32,
}

impl Palette {
    pub fn get(ctx: &Context) -> Self {
        Self::for_dark(ctx.style().visuals.dark_mode)
    }

    fn for_dark(dark: bool) -> Self {
        if dark {
            Self {
                canvas: Color32::from_rgb(32, 32, 32),
                panel: Color32::from_rgb(39, 39, 39),
                panel_alt: Color32::from_rgb(45, 45, 45),
                text: Color32::from_rgb(245, 245, 245),
                muted: Color32::from_rgb(190, 190, 190),
                subtle: Color32::from_rgb(140, 140, 140),
                border: Color32::from_rgb(58, 58, 58),
                accent: Color32::from_rgb(96, 205, 255),
                accent_hover: Color32::from_rgb(86, 185, 230),
                accent_soft: Color32::from_rgb(36, 62, 77),
                sidebar: Color32::from_rgb(32, 32, 32),
                danger: Color32::from_rgb(255, 153, 164),
                warning: Color32::from_rgb(252, 225, 0),
                success: Color32::from_rgb(108, 203, 95),
            }
        } else {
            Self {
                canvas: Color32::from_rgb(248, 249, 252),
                panel: Color32::WHITE,
                panel_alt: Color32::from_rgb(249, 249, 249),
                text: Color32::from_rgb(26, 26, 26),
                muted: Color32::from_rgb(96, 96, 96),
                subtle: Color32::from_rgb(138, 138, 138),
                border: Color32::from_rgb(229, 229, 229),
                accent: Color32::from_rgb(0, 103, 192),
                accent_hover: Color32::from_rgb(25, 118, 199),
                accent_soft: Color32::from_rgb(230, 241, 251),
                sidebar: Color32::from_rgb(241, 243, 249),
                danger: Color32::from_rgb(196, 43, 28),
                warning: Color32::from_rgb(157, 93, 0),
                success: Color32::from_rgb(15, 123, 15),
            }
        }
    }
}

/// Configure both themes, so switching between light, dark, and system is immediate.
/// Font families are left intact to retain the application's CJK fallback fonts.
pub fn install(ctx: &Context) {
    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        let p = Palette::for_dark(theme == egui::Theme::Dark);
        ctx.style_mut_of(theme, |style| {
            style
                .text_styles
                .insert(TextStyle::Heading, FontId::proportional(28.0));
            style
                .text_styles
                .insert(TextStyle::Body, FontId::proportional(14.0));
            style
                .text_styles
                .insert(TextStyle::Button, FontId::proportional(14.0));
            style
                .text_styles
                .insert(TextStyle::Small, FontId::proportional(12.0));
            style
                .text_styles
                .insert(TextStyle::Monospace, FontId::monospace(13.0));
            style.spacing.item_spacing = vec2(8.0, 8.0);
            style.spacing.button_padding = vec2(12.0, 5.0);
            style.spacing.interact_size = vec2(32.0, 32.0);
            style.spacing.icon_width = 16.0;
            style.spacing.icon_width_inner = 8.0;
            style.spacing.icon_spacing = 8.0;
            style.spacing.combo_height = 340.0;
            // Windows 11 menus: a few pixels around rounded rows.
            style.spacing.menu_margin = egui::Margin::same(4);
            style.spacing.scroll = egui::style::ScrollStyle::thin();
            style.animation_time = if motion::reduced_motion() { 0.0 } else { 0.12 };

            let v = &mut style.visuals;
            v.override_text_color = Some(p.text);
            v.panel_fill = p.canvas;
            v.window_fill = p.panel;
            v.window_stroke = Stroke::new(1.0_f32, p.border);
            v.window_corner_radius = CornerRadius::same(8);
            v.menu_corner_radius = CornerRadius::same(8);
            v.popup_shadow = egui::Shadow {
                offset: [0, 8],
                blur: 24,
                spread: 0,
                color: Color32::from_black_alpha(if theme == egui::Theme::Dark { 96 } else { 30 }),
            };
            v.faint_bg_color = p.panel_alt;
            v.extreme_bg_color = p.panel;
            v.code_bg_color = p.panel_alt;
            v.hyperlink_color = p.accent;
            v.warn_fg_color = p.warning;
            v.error_fg_color = p.danger;
            v.selection.bg_fill = p.accent_soft;
            v.selection.stroke = Stroke::new(1.5_f32, p.accent);
            v.text_cursor.stroke = Stroke::new(1.5_f32, p.accent);
            v.interact_cursor = None;
            v.slider_trailing_fill = true;
            v.indent_has_left_vline = false;
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
                visual.bg_fill = p.panel_alt;
                visual.weak_bg_fill = p.panel;
            }
            v.widgets.noninteractive.bg_fill = p.panel;
            v.widgets.noninteractive.weak_bg_fill = p.panel;
            v.widgets.hovered.bg_fill = p.panel_alt;
            v.widgets.hovered.weak_bg_fill = p.panel_alt;
            v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, p.border);
            v.widgets.active.bg_fill = p.panel_alt;
            v.widgets.active.weak_bg_fill = p.panel_alt;
            v.widgets.active.bg_stroke = Stroke::new(1.0_f32, p.accent);
            v.widgets.active.fg_stroke = Stroke::new(1.2_f32, p.text);
            v.widgets.open.bg_fill = p.panel_alt;
            v.widgets.open.weak_bg_fill = p.panel_alt;
            v.widgets.open.bg_stroke = Stroke::new(1.0_f32, p.accent);
        });
    }
}

#[derive(Clone, Copy)]
pub enum Icon {
    Scan,
    Translate,
    Copy,
    Settings,
    Home,
    Pin,
    Close,
    Swap,
    ArrowRight,
    Check,
    Info,
    Text,
    Table,
    Formula,
    Plus,
    Folder,
}

/// Paint icons on a common 24-unit grid; they stay crisp at any Windows DPI.
pub fn draw_icon(ui: &Ui, icon: Icon, rect: Rect, color: Color32) {
    let painter = ui.painter();
    let scale = rect.width().min(rect.height()) / 24.0;
    let origin = rect.center() - vec2(12.0, 12.0) * scale;
    let pt = |x: f32, y: f32| origin + vec2(x, y) * scale;
    let stroke = Stroke::new((1.7 * scale).max(1.0), color);
    let line = |a: (f32, f32), b: (f32, f32)| {
        painter.line_segment([pt(a.0, a.1), pt(b.0, b.1)], stroke);
    };
    let path = |points: &[(f32, f32)]| {
        painter.add(Shape::line(
            points.iter().map(|&(x, y)| pt(x, y)).collect(),
            stroke,
        ));
    };
    let outline = |x: f32, y: f32, w: f32, h: f32, radius: u8| {
        painter.rect_stroke(
            Rect::from_min_size(pt(x, y), vec2(w, h) * scale),
            CornerRadius::same(radius),
            stroke,
            StrokeKind::Inside,
        );
    };
    let circle = |x: f32, y: f32, radius: f32| {
        painter.circle_stroke(pt(x, y), radius * scale, stroke);
    };
    match icon {
        Icon::Scan => {
            path(&[(8.0, 3.0), (3.0, 3.0), (3.0, 8.0)]);
            path(&[(16.0, 3.0), (21.0, 3.0), (21.0, 8.0)]);
            path(&[(3.0, 16.0), (3.0, 21.0), (8.0, 21.0)]);
            path(&[(16.0, 21.0), (21.0, 21.0), (21.0, 16.0)]);
            line((6.0, 12.0), (18.0, 12.0));
            line((8.0, 8.0), (16.0, 8.0));
            line((8.0, 16.0), (16.0, 16.0));
        }
        Icon::Translate => {
            line((3.0, 6.0), (14.0, 6.0));
            line((8.5, 3.0), (8.5, 6.0));
            path(&[(12.0, 6.0), (10.0, 10.0), (6.0, 14.0), (3.0, 16.0)]);
            path(&[(5.0, 9.0), (8.0, 12.0), (12.0, 15.0)]);
            path(&[(13.0, 21.0), (17.0, 11.0), (21.0, 21.0)]);
            line((14.5, 17.0), (19.5, 17.0));
        }
        Icon::Copy => {
            outline(8.0, 8.0, 13.0, 13.0, 2);
            path(&[
                (16.0, 5.0),
                (16.0, 3.0),
                (3.0, 3.0),
                (3.0, 16.0),
                (5.0, 16.0),
            ]);
        }
        Icon::Settings => {
            let mut points = Vec::with_capacity(33);
            for i in 0..=32 {
                let angle = i as f32 * std::f32::consts::TAU / 32.0;
                let radius = if i % 4 == 0 || i % 4 == 3 { 9.0 } else { 7.4 };
                points.push(pt(12.0 + angle.cos() * radius, 12.0 + angle.sin() * radius));
            }
            painter.add(Shape::line(points, stroke));
            circle(12.0, 12.0, 3.0);
        }
        Icon::Home => {
            path(&[(2.5, 10.0), (12.0, 3.0), (21.5, 10.0)]);
            path(&[
                (5.0, 9.0),
                (5.0, 21.0),
                (10.0, 21.0),
                (10.0, 14.0),
                (14.0, 14.0),
                (14.0, 21.0),
                (19.0, 21.0),
                (19.0, 9.0),
            ]);
        }
        Icon::Pin => {
            path(&[
                (8.0, 3.0),
                (16.0, 3.0),
                (15.0, 10.0),
                (18.0, 13.0),
                (18.0, 15.0),
                (6.0, 15.0),
                (6.0, 13.0),
                (9.0, 10.0),
                (8.0, 3.0),
            ]);
            line((12.0, 15.0), (12.0, 22.0));
        }
        Icon::Close => {
            line((6.0, 6.0), (18.0, 18.0));
            line((18.0, 6.0), (6.0, 18.0));
        }
        Icon::Swap => {
            path(&[(3.0, 8.0), (20.0, 8.0), (16.0, 4.0)]);
            path(&[(21.0, 16.0), (4.0, 16.0), (8.0, 20.0)]);
        }
        Icon::ArrowRight => {
            line((4.0, 12.0), (20.0, 12.0));
            path(&[(14.0, 6.0), (20.0, 12.0), (14.0, 18.0)]);
        }
        Icon::Check => path(&[(4.0, 12.0), (9.0, 17.0), (20.0, 6.0)]),
        Icon::Info => {
            circle(12.0, 12.0, 9.0);
            painter.circle_filled(pt(12.0, 7.5), 1.0 * scale, color);
            line((12.0, 11.0), (12.0, 17.0));
        }
        Icon::Text => {
            path(&[(4.0, 7.0), (4.0, 4.0), (20.0, 4.0), (20.0, 7.0)]);
            line((12.0, 4.0), (12.0, 20.0));
            line((8.0, 20.0), (16.0, 20.0));
        }
        Icon::Table => {
            outline(3.0, 3.0, 18.0, 18.0, 2);
            line((3.0, 9.0), (21.0, 9.0));
            line((3.0, 15.0), (21.0, 15.0));
            line((9.0, 3.0), (9.0, 21.0));
        }
        Icon::Formula => {
            path(&[
                (3.0, 13.0),
                (6.0, 13.0),
                (9.0, 20.0),
                (13.0, 4.0),
                (22.0, 4.0),
            ]);
            line((16.0, 11.0), (21.0, 17.0));
            line((21.0, 11.0), (16.0, 17.0));
        }
        Icon::Plus => {
            line((12.0, 3.0), (12.0, 21.0));
            line((3.0, 12.0), (21.0, 12.0));
        }
        Icon::Folder => {
            path(&[
                (3.0, 19.0),
                (3.0, 5.0),
                (9.0, 5.0),
                (11.0, 7.0),
                (21.0, 7.0),
                (21.0, 19.0),
                (3.0, 19.0),
            ]);
            line((3.0, 10.5), (21.0, 10.5));
        }
    }
}

#[derive(Clone, Copy)]
pub enum ButtonKind {
    Primary,
    Secondary,
    Ghost,
}

pub fn button(ui: &mut Ui, label: &str, icon: Icon, kind: ButtonKind) -> Response {
    button_with_feedback(ui, label, icon, kind, None)
}

/// A button that can briefly show a confirmation (for example "已复制") after it was used.
/// `feedback` replaces the label and icon while it is `Some`.
pub fn button_with_feedback(
    ui: &mut Ui,
    label: &str,
    icon: Icon,
    kind: ButtonKind,
    feedback: Option<&str>,
) -> Response {
    let p = Palette::get(ui.ctx());
    let enabled = ui.is_enabled();
    let dark = ui.visuals().dark_mode;
    let primary = matches!(kind, ButtonKind::Primary);
    let confirmed = feedback.is_some();
    let fg = if !enabled {
        p.subtle
    } else if primary {
        if dark {
            p.canvas
        } else {
            Color32::WHITE
        }
    } else if confirmed {
        p.success
    } else {
        p.text
    };
    let text = feedback.unwrap_or(label);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), FontId::proportional(14.0), fg);
    // Keep the width stable when the feedback text is shorter than the label.
    let width = ui
        .painter()
        .layout_no_wrap(label.to_owned(), FontId::proportional(14.0), fg)
        .size()
        .x
        .max(galley.size().x);
    let (rect, response) = ui.allocate_exact_size(vec2(width + 48.0, 32.0), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, label));
    if ui.is_rect_visible(rect) {
        let ctx = ui.ctx();
        let hover = motion::toggle(
            ctx,
            response.id.with("hover"),
            enabled && response.hovered(),
            motion::FAST,
        );
        let press = motion::toggle(
            ctx,
            response.id.with("press"),
            enabled && response.is_pointer_button_down_on(),
            0.06,
        );
        let (rest, hovered, pressed, stroke) = match kind {
            ButtonKind::Primary => (
                p.accent,
                p.accent_hover,
                p.accent_hover.gamma_multiply(0.85),
                Stroke::new(1.0_f32, Color32::from_black_alpha(14)),
            ),
            ButtonKind::Secondary => (
                p.panel,
                p.panel_alt,
                p.canvas,
                Stroke::new(1.0_f32, motion::mix(p.border, p.subtle, hover * 0.5)),
            ),
            ButtonKind::Ghost => (
                Color32::TRANSPARENT,
                hover_fill(dark, false),
                hover_fill(dark, true),
                Stroke::NONE,
            ),
        };
        let mut fill = motion::mix(motion::mix(rest, hovered, hover), pressed, press);
        if !enabled {
            fill = if matches!(kind, ButtonKind::Ghost) {
                Color32::TRANSPARENT
            } else {
                p.panel_alt
            };
        }
        // A one-pixel settle while pressed gives tactile feedback without layout shift.
        let body = rect.translate(vec2(0.0, press));
        ui.painter().rect(body, 4, fill, stroke, StrokeKind::Inside);
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect.expand(2.0),
                6,
                Stroke::new(2.0_f32, p.text),
                StrokeKind::Outside,
            );
        }
        draw_icon(
            ui,
            if confirmed { Icon::Check } else { icon },
            Rect::from_center_size(pos2(body.left() + 20.0, body.center().y), vec2(16.0, 16.0)),
            fg,
        );
        ui.painter().galley(
            pos2(body.left() + 36.0, body.center().y - galley.size().y / 2.0),
            galley,
            fg,
        );
    }
    response
}

pub fn icon_button(ui: &mut Ui, icon: Icon, tooltip: &str, selected: bool) -> Response {
    icon_button_with_feedback(ui, icon, tooltip, selected, false)
}

/// Icon button that shows a check mark while `confirmed` (for example right after copying).
pub fn icon_button_with_feedback(
    ui: &mut Ui,
    icon: Icon,
    tooltip: &str,
    selected: bool,
    confirmed: bool,
) -> Response {
    let p = Palette::get(ui.ctx());
    let enabled = ui.is_enabled();
    let (rect, response) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::click());
    response.widget_info(|| {
        WidgetInfo::selected(WidgetType::Button, ui.is_enabled(), selected, tooltip)
    });
    if ui.is_rect_visible(rect) {
        let dark = ui.visuals().dark_mode;
        let hover = motion::toggle(
            ui.ctx(),
            response.id.with("hover"),
            enabled && response.hovered(),
            motion::FAST,
        );
        let press = motion::toggle(
            ui.ctx(),
            response.id.with("press"),
            enabled && response.is_pointer_button_down_on(),
            0.06,
        );
        let chosen = motion::toggle(
            ui.ctx(),
            response.id.with("selected"),
            enabled && selected,
            motion::FAST,
        );
        let idle = motion::mix(
            motion::mix(Color32::TRANSPARENT, hover_fill(dark, false), hover),
            hover_fill(dark, true),
            press,
        );
        // Toggled on: the raised tile with an accent underline of Windows 11
        // command bars (as the selected mode in Snipping Tool), not a tinted fill.
        let tile = if dark { p.panel_alt } else { p.panel };
        let fill = motion::mix(idle, tile, chosen);
        ui.painter().rect_filled(rect, 4, fill);
        if chosen > 0.0 {
            ui.painter()
                .extend(raised_tile(rect, dark, Color32::TRANSPARENT, chosen));
            let bar = Rect::from_center_size(
                pos2(rect.center().x, rect.bottom() - 2.5),
                vec2(14.0 * chosen, 3.0),
            );
            ui.painter().rect_filled(bar, 2, p.accent);
        }
        if response.has_focus() {
            ui.painter()
                .rect_stroke(rect, 4, Stroke::new(2.0_f32, p.text), StrokeKind::Inside);
        }
        let fg = if !enabled {
            p.subtle
        } else if confirmed {
            p.success
        } else {
            p.text
        };
        draw_icon(
            ui,
            if confirmed { Icon::Check } else { icon },
            Rect::from_center_size(rect.center() + vec2(0.0, press * 0.5), vec2(16.0, 16.0)),
            fg,
        );
    }
    response.on_hover_text(if confirmed { "已复制" } else { tooltip })
}

/// A raised Windows 11 tile: a faint 1px outline whose bottom edge is a
/// little darker (the "elevation border"), rather than a drop shadow.
/// `strength` fades the edges in and out with the selection.
pub fn raised_tile(rect: Rect, dark: bool, fill: Color32, strength: f32) -> Vec<Shape> {
    let (edge, bottom) = if dark {
        (Color32::from_white_alpha(18), Color32::from_black_alpha(80))
    } else {
        (Color32::from_black_alpha(16), Color32::from_black_alpha(44))
    };
    vec![
        Shape::rect_filled(rect, 4, fill),
        Shape::rect_stroke(
            rect,
            4,
            Stroke::new(1.0_f32, edge.gamma_multiply(strength)),
            StrokeKind::Inside,
        ),
        Shape::line_segment(
            [
                pos2(rect.left() + 3.0, rect.bottom() - 0.5),
                pos2(rect.right() - 3.0, rect.bottom() - 0.5),
            ],
            Stroke::new(1.0_f32, bottom.gamma_multiply(strength)),
        ),
    ]
}

/// An accent bar that slides to the selected item's `rect`, stretching slightly while
/// it travels. Paint it after laying out every item of the group.
pub fn sliding_underline(ui: &Ui, id: &str, rect: Rect, width: f32, color: Color32) {
    let id = egui::Id::new(id);
    let target = rect.center().x;
    let x = motion::value(ui.ctx(), id.with("x"), target, motion::SLIDE);
    let travel = (target - x).abs();
    let bar = Rect::from_center_size(
        pos2(x, rect.bottom() - 3.0),
        vec2(width + travel.min(48.0) * 0.6, 3.0),
    );
    ui.painter().rect_filled(bar, 2, color);
}

fn hover_fill(dark: bool, pressed: bool) -> Color32 {
    if dark {
        Color32::from_white_alpha(if pressed { 20 } else { 14 })
    } else {
        Color32::from_black_alpha(if pressed { 16 } else { 9 })
    }
}

/// `text` shortened with "…" so it fits `max_width` in the button font.
pub fn elide(ui: &Ui, text: &str, max_width: f32) -> String {
    let font = TextStyle::Button.resolve(ui.style());
    let width = |candidate: &str| {
        ui.fonts(|fonts| {
            fonts
                .layout_no_wrap(candidate.to_owned(), font.clone(), Color32::WHITE)
                .size()
                .x
        })
    };
    if width(text) <= max_width {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    for keep in (1..chars.len()).rev() {
        let candidate = format!("{}…", chars[..keep].iter().collect::<String>().trim_end());
        if width(&candidate) <= max_width {
            return candidate;
        }
    }
    "…".to_owned()
}

/// A small group caption inside a menu, aligned with the row text.
pub fn menu_heading(ui: &mut Ui, text: &str) {
    let p = Palette::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.label(egui::RichText::new(text).size(12.0).color(p.muted));
    });
}

/// Keyboard state of the open dropdown menu, shared with its rows this frame.
#[derive(Clone, Copy, Default)]
struct MenuKeys {
    /// Row under the keyboard (or the mouse, whichever moved last).
    highlight: Option<usize>,
    /// Rows laid out so far this frame.
    next: usize,
    moved: bool,
    activate: bool,
    activated: bool,
}

fn menu_keys_id() -> egui::Id {
    egui::Id::new("sightocr_menu_keys")
}

/// A Windows 11 menu row: rounded, grey under the pointer or keyboard
/// highlight; the selected row keeps a grey fill and a short accent bar.
/// `changed()` reports the row was chosen (click, Enter or Space).
pub fn menu_item(ui: &mut Ui, selected: bool, label: impl Into<egui::WidgetText>) -> Response {
    let p = Palette::get(ui.ctx());
    let enabled = ui.is_enabled();
    let dark = ui.visuals().dark_mode;
    let galley = label.into().into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        TextStyle::Body,
    );
    let width = ui.available_width().max(galley.size().x + 28.0);
    let (rect, mut response) = ui.allocate_exact_size(vec2(width, 32.0), Sense::click());
    response.widget_info(|| {
        WidgetInfo::selected(
            WidgetType::SelectableLabel,
            enabled,
            selected,
            galley.text(),
        )
    });

    // One highlight, moved by whichever input was used last, as in Windows menus.
    let mut keys = ui
        .ctx()
        .data(|data| data.get_temp::<MenuKeys>(menu_keys_id()));
    let hot = match keys.as_mut() {
        Some(keys) => {
            let index = keys.next;
            keys.next += 1;
            if keys.highlight.is_none() && selected {
                keys.highlight = Some(index);
            }
            let pointer_moved = ui.input(|input| input.pointer.delta() != egui::Vec2::ZERO);
            if response.hovered() && pointer_moved {
                keys.highlight = Some(index);
            }
            let hot = keys.highlight == Some(index);
            if hot && keys.moved {
                response.scroll_to_me(None);
            }
            if enabled && hot && keys.activate && !keys.activated {
                keys.activated = true;
                response.mark_changed();
            }
            hot
        }
        None => response.hovered(),
    };
    if let Some(keys) = keys {
        ui.ctx()
            .data_mut(|data| data.insert_temp(menu_keys_id(), keys));
    }
    if enabled && response.clicked() {
        response.mark_changed();
    }

    if ui.is_rect_visible(rect) {
        let lit = motion::toggle(
            ui.ctx(),
            response.id.with("hover"),
            enabled && (hot || selected),
            motion::FAST,
        );
        let fill = if enabled && response.is_pointer_button_down_on() {
            hover_fill(dark, true)
        } else {
            motion::mix(Color32::TRANSPARENT, hover_fill(dark, false), lit)
        };
        ui.painter().rect_filled(rect, 4, fill);
        if selected {
            let bar =
                Rect::from_center_size(pos2(rect.left() + 1.5, rect.center().y), vec2(3.0, 16.0));
            ui.painter().rect_filled(bar, 2, p.accent);
        }
        let color = if enabled { p.text } else { p.subtle };
        ui.painter().galley(
            pos2(rect.left() + 12.0, rect.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
    }
    response
}

/// [`menu_item`] that sets `current` to `value` when chosen, like `selectable_value`.
pub fn menu_option<V: PartialEq>(
    ui: &mut Ui,
    current: &mut V,
    value: V,
    label: impl Into<egui::WidgetText>,
) -> Response {
    let response = menu_item(ui, *current == value, label);
    if response.changed() {
        *current = value;
    }
    response
}

/// A dropdown with a Windows 11 menu. `flat` is the command-bar style (no
/// border or fill until hovered, as in Snipping Tool); form fields keep their
/// border. While open: Up/Down/Home/End move the highlight, Enter or Space
/// chooses, Esc closes.
pub fn dropdown<R>(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash,
    flat: bool,
    configure: impl FnOnce(egui::ComboBox) -> egui::ComboBox,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<Option<R>> {
    let dark = ui.visuals().dark_mode;
    let p = Palette::get(ui.ctx());
    ui.scope(|ui| {
        if flat {
            let visuals = ui.visuals_mut();
            for (state, fill) in [
                (&mut visuals.widgets.inactive, Color32::TRANSPARENT),
                (&mut visuals.widgets.hovered, hover_fill(dark, false)),
                (&mut visuals.widgets.active, hover_fill(dark, true)),
                (&mut visuals.widgets.open, hover_fill(dark, true)),
            ] {
                state.weak_bg_fill = fill;
                state.bg_fill = fill;
                state.bg_stroke = Stroke::NONE;
            }
            // While open, keep the button neutral rather than accent-tinted.
            visuals.selection.bg_fill = hover_fill(dark, true);
            visuals.selection.stroke = Stroke::new(1.0_f32, p.text);
        }
        // ComboBox hashes its salt into an Id first; match that exactly.
        let combo_id = ui.make_persistent_id(egui::Id::new(&id_salt));
        let nav_id = combo_id.with("keys");
        // Debug builds only: SIGHTOCR_PREVIEW_MENU=<id> opens that menu once,
        // so UI previews can show an open dropdown.
        #[cfg(debug_assertions)]
        if std::env::var("SIGHTOCR_PREVIEW_MENU")
            .is_ok_and(|wanted| egui::Id::new(wanted.as_str()) == egui::Id::new(&id_salt))
            && !ui
                .ctx()
                .data(|data| data.get_temp::<bool>(nav_id.with("preview")))
                .unwrap_or(false)
        {
            ui.memory_mut(|memory| memory.open_popup(combo_id.with("popup")));
            ui.ctx()
                .data_mut(|data| data.insert_temp(nav_id.with("preview"), true));
        }
        let open = egui::ComboBox::is_open(ui.ctx(), combo_id);
        // Take the menu keys before the dropdown button sees them, so Enter
        // chooses a row instead of toggling the menu shut.
        let pressed = |ui: &mut Ui, key| {
            open && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, key))
        };
        let down = pressed(ui, egui::Key::ArrowDown);
        let up = pressed(ui, egui::Key::ArrowUp);
        let home = pressed(ui, egui::Key::Home);
        let end = pressed(ui, egui::Key::End);
        let enter = pressed(ui, egui::Key::Enter);
        let space = pressed(ui, egui::Key::Space);
        let (mut highlight, count) = if open {
            ui.ctx()
                .data(|data| data.get_temp::<(Option<usize>, usize)>(nav_id))
                .unwrap_or((None, 0))
        } else {
            (None, 0)
        };
        if count > 0 {
            let last = count - 1;
            if down {
                highlight = Some(highlight.map_or(0, |row| (row + 1).min(last)));
            }
            if up {
                highlight = Some(highlight.map_or(last, |row| row.saturating_sub(1)));
            }
            if home {
                highlight = Some(0);
            }
            if end {
                highlight = Some(last);
            }
        }
        let keys = MenuKeys {
            highlight,
            next: 0,
            moved: down || up || home || end,
            activate: enter || space,
            activated: false,
        };
        // The menu is a little wider than its button (8px each side), using
        // the button width from the previous frame so it never creeps wider.
        let width_id = combo_id.with("button_width");
        let button_width = ui
            .ctx()
            .data(|data| data.get_temp::<f32>(width_id))
            .unwrap_or(0.0);
        let inner = configure(egui::ComboBox::from_id_salt(&id_salt))
            .icon(combo_icon)
            .show_ui(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.set_min_width(button_width + 8.0);
                ui.ctx()
                    .data_mut(|data| data.insert_temp(menu_keys_id(), keys));
                let result = add_contents(ui);
                let keys = ui
                    .ctx()
                    .data_mut(|data| data.remove_temp::<MenuKeys>(menu_keys_id()))
                    .unwrap_or_default();
                ui.ctx()
                    .data_mut(|data| data.insert_temp(nav_id, (keys.highlight, keys.next)));
                if keys.activated {
                    ui.memory_mut(|memory| memory.close_popup());
                }
                result
            });
        if !open {
            ui.ctx()
                .data_mut(|data| data.remove_temp::<(Option<usize>, usize)>(nav_id));
        }
        let width = inner.response.rect.width();
        ui.ctx().data_mut(|data| data.insert_temp(width_id, width));
        inner
    })
    .inner
}

/// Use with `ComboBox::icon` for a Fluent chevron instead of egui's filled triangle.
pub fn combo_icon(
    ui: &Ui,
    rect: Rect,
    visuals: &egui::style::WidgetVisuals,
    is_open: bool,
    _above_or_below: egui::AboveOrBelow,
) {
    let center = rect.center();
    // Flip the chevron smoothly instead of snapping when the popup opens.
    let open = motion::toggle(
        ui.ctx(),
        egui::Id::new(("combo_chevron", rect.min.x as i32, rect.min.y as i32)),
        is_open,
        motion::FAST,
    );
    let direction = 1.0 - 2.0 * open;
    ui.painter().add(Shape::line(
        vec![
            center + vec2(-4.0, -2.0 * direction),
            center + vec2(0.0, 2.0 * direction),
            center + vec2(4.0, -2.0 * direction),
        ],
        Stroke::new(1.25_f32, visuals.fg_stroke.color),
    ));
}

#[cfg(test)]
mod menu_tests {
    use super::*;
    use std::cell::Cell;

    /// One frame with a three-row dropdown; returns the dropdown button's rect.
    fn frame(ctx: &Context, value: &mut usize, events: Vec<egui::Event>) -> Rect {
        let rect = Cell::new(Rect::NOTHING);
        let input = egui::RawInput {
            events,
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 400.0))),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let response = dropdown(
                    ui,
                    "menu_test",
                    true,
                    |combo| combo.width(120.0).selected_text("choice"),
                    |ui| {
                        for row in 0..3 {
                            menu_option(ui, value, row, format!("row {row}"));
                        }
                    },
                );
                rect.set(response.response.rect);
            });
        });
        rect.get()
    }

    fn key(key: egui::Key) -> Vec<egui::Event> {
        vec![egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]
    }

    fn click(at: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(at),
            egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    fn open(ctx: &Context, value: &mut usize) {
        let button = frame(ctx, value, Vec::new());
        frame(ctx, value, click(button.center(), true));
        frame(ctx, value, click(button.center(), false));
        frame(ctx, value, Vec::new());
        assert!(
            ctx.memory(|memory| memory.any_popup_open()),
            "menu opens on click"
        );
    }

    #[test]
    fn long_names_are_shortened_with_an_ellipsis() {
        let ctx = Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                assert_eq!(elide(ui, "本地 OCR", 120.0), "本地 OCR");
                let long = elide(ui, "公司内网统一模型网关（视觉识别专用）", 120.0);
                assert!(long.ends_with('…') && long.chars().count() > 2, "{long}");
                let font = TextStyle::Button.resolve(ui.style());
                let width = ui.fonts(|fonts| {
                    fonts
                        .layout_no_wrap(long.clone(), font, Color32::WHITE)
                        .size()
                        .x
                });
                assert!(width <= 120.0, "{long} is {width}px");
            });
        });
    }

    #[test]
    fn arrow_keys_and_enter_choose_like_a_windows_menu() {
        let ctx = Context::default();
        let mut value = 0;
        open(&ctx, &mut value);
        // The highlight starts on the current choice; Down moves to the next row.
        frame(&ctx, &mut value, key(egui::Key::ArrowDown));
        frame(&ctx, &mut value, key(egui::Key::Enter));
        assert_eq!(value, 1, "Enter chooses the highlighted row");
        frame(&ctx, &mut value, Vec::new());
        assert!(
            !ctx.memory(|memory| memory.any_popup_open()),
            "choosing closes the menu"
        );

        open(&ctx, &mut value);
        frame(&ctx, &mut value, key(egui::Key::End));
        frame(&ctx, &mut value, key(egui::Key::ArrowDown));
        frame(&ctx, &mut value, key(egui::Key::Space));
        assert_eq!(
            value, 2,
            "End then Down stays on the last row; Space chooses"
        );

        open(&ctx, &mut value);
        frame(&ctx, &mut value, key(egui::Key::ArrowUp));
        frame(&ctx, &mut value, key(egui::Key::Escape));
        frame(&ctx, &mut value, Vec::new());
        assert_eq!(value, 2, "Esc closes without choosing");
        assert!(!ctx.memory(|memory| memory.any_popup_open()));
    }
}
