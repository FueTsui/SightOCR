//! Fluent-inspired typography, neutral surfaces, and compact Windows desktop controls.

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
            style.spacing.combo_height = 220.0;
            style.spacing.scroll = egui::style::ScrollStyle::thin();
            style.animation_time = 0.12;

            let v = &mut style.visuals;
            v.override_text_color = Some(p.text);
            v.panel_fill = p.canvas;
            v.window_fill = p.panel;
            v.window_stroke = Stroke::new(1.0_f32, p.border);
            v.window_corner_radius = CornerRadius::same(8);
            v.menu_corner_radius = CornerRadius::same(8);
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
    }
}

#[derive(Clone, Copy)]
pub enum ButtonKind {
    Primary,
    Secondary,
    Ghost,
}

pub fn button(ui: &mut Ui, label: &str, icon: Icon, kind: ButtonKind) -> Response {
    let p = Palette::get(ui.ctx());
    let enabled = ui.is_enabled();
    let dark = ui.visuals().dark_mode;
    let primary = matches!(kind, ButtonKind::Primary);
    let fg = if !enabled {
        p.subtle
    } else if primary {
        if dark {
            p.canvas
        } else {
            Color32::WHITE
        }
    } else {
        p.text
    };
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), FontId::proportional(14.0), fg);
    let (rect, response) =
        ui.allocate_exact_size(vec2(galley.size().x + 48.0, 32.0), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, label));
    if ui.is_rect_visible(rect) {
        let hovered = enabled && response.hovered();
        let pressed = enabled && response.is_pointer_button_down_on();
        let (mut fill, stroke) = match kind {
            ButtonKind::Primary => (
                if pressed {
                    p.accent_hover.gamma_multiply(0.85)
                } else if hovered {
                    p.accent_hover
                } else {
                    p.accent
                },
                Stroke::new(1.0_f32, Color32::from_black_alpha(14)),
            ),
            ButtonKind::Secondary => (
                if pressed {
                    p.canvas
                } else if hovered {
                    p.panel_alt
                } else {
                    p.panel
                },
                Stroke::new(1.0_f32, p.border),
            ),
            ButtonKind::Ghost => (
                if pressed || hovered {
                    hover_fill(dark, pressed)
                } else {
                    Color32::TRANSPARENT
                },
                Stroke::NONE,
            ),
        };
        if !enabled {
            fill = if matches!(kind, ButtonKind::Ghost) {
                Color32::TRANSPARENT
            } else {
                p.panel_alt
            };
        }
        ui.painter().rect(rect, 4, fill, stroke, StrokeKind::Inside);
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
            icon,
            Rect::from_center_size(pos2(rect.left() + 20.0, rect.center().y), vec2(16.0, 16.0)),
            fg,
        );
        ui.painter().galley(
            pos2(rect.left() + 36.0, rect.center().y - galley.size().y / 2.0),
            galley,
            fg,
        );
    }
    response
}

pub fn icon_button(ui: &mut Ui, icon: Icon, tooltip: &str, selected: bool) -> Response {
    let p = Palette::get(ui.ctx());
    let enabled = ui.is_enabled();
    let (rect, response) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::click());
    response.widget_info(|| {
        WidgetInfo::selected(WidgetType::Button, ui.is_enabled(), selected, tooltip)
    });
    if ui.is_rect_visible(rect) {
        let hovered = enabled && response.hovered();
        let pressed = enabled && response.is_pointer_button_down_on();
        let fill = if enabled && selected {
            p.accent_soft
        } else if hovered || pressed {
            hover_fill(ui.visuals().dark_mode, pressed)
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, 4, fill);
        if response.has_focus() {
            ui.painter()
                .rect_stroke(rect, 4, Stroke::new(2.0_f32, p.text), StrokeKind::Inside);
        }
        let fg = if !enabled {
            p.subtle
        } else if selected {
            p.accent
        } else {
            p.text
        };
        draw_icon(
            ui,
            icon,
            Rect::from_center_size(rect.center(), vec2(16.0, 16.0)),
            fg,
        );
    }
    response.on_hover_text(tooltip)
}

fn hover_fill(dark: bool, pressed: bool) -> Color32 {
    if dark {
        Color32::from_white_alpha(if pressed { 20 } else { 14 })
    } else {
        Color32::from_black_alpha(if pressed { 16 } else { 9 })
    }
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
    let direction = if is_open { -1.0 } else { 1.0 };
    ui.painter().add(Shape::line(
        vec![
            center + vec2(-4.0, -2.0 * direction),
            center + vec2(0.0, 2.0 * direction),
            center + vec2(4.0, -2.0 * direction),
        ],
        Stroke::new(1.25_f32, visuals.fg_stroke.color),
    ));
}
