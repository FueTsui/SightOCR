//! Small, interruptible UI motion helpers. Everything here is egui-painted only:
//! native RichEdit children are never faded or moved, so they cannot drift from
//! their layout rectangles. Motion is disabled when Windows "Show animations" is off.

use eframe::egui::{self, Color32, Context, Id};
use std::time::{Duration, Instant};

/// Hover/press colour transitions.
pub const FAST: f32 = 0.10;
/// Indicators that slide between tabs or segments.
pub const SLIDE: f32 = 0.18;
/// Content that fades in after a page, tab or message change.
pub const APPEAR: f32 = 0.22;

/// Windows' client-area animation preference, read once at startup.
pub fn reduced_motion() -> bool {
    static REDUCED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *REDUCED.get_or_init(|| {
        #[cfg(windows)]
        {
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION,
            };
            let mut enabled: i32 = 1;
            // SAFETY: SPI_GETCLIENTAREAANIMATION writes one BOOL to the provided local.
            let ok = unsafe {
                SystemParametersInfoW(
                    SPI_GETCLIENTAREAANIMATION,
                    0,
                    (&mut enabled as *mut i32).cast(),
                    0,
                )
            };
            ok != 0 && enabled == 0
        }
        #[cfg(not(windows))]
        {
            false
        }
    })
}

pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// 0 → 1 as `on` becomes true, and back. Instant when motion is reduced.
pub fn toggle(ctx: &Context, id: Id, on: bool, seconds: f32) -> f32 {
    if reduced_motion() {
        return if on { 1.0 } else { 0.0 };
    }
    ctx.animate_bool_with_time(id, on, seconds)
}

/// Animate a scalar (for example an indicator's x position) toward `target`.
pub fn value(ctx: &Context, id: Id, target: f32, seconds: f32) -> f32 {
    if reduced_motion() {
        return ctx.animate_value_with_time(id, target, 0.0);
    }
    ctx.animate_value_with_time(id, target, seconds)
}

/// Eased progress (0 → 1) since `key` last changed for this `id`.
/// Use it to fade in content when a tab, page or message changes.
pub fn appear(ctx: &Context, id: Id, key: u64) -> f32 {
    if reduced_motion() {
        return 1.0;
    }
    let now = Instant::now();
    let started = ctx.data_mut(|data| {
        let entry = data.get_temp_mut_or_insert_with(id, || (key, now));
        if entry.0 != key {
            *entry = (key, now);
        }
        entry.1
    });
    let t = now.duration_since(started).as_secs_f32() / APPEAR;
    if t < 1.0 {
        ctx.request_repaint();
    }
    ease_out(t)
}

/// Record a momentary event (for example "copied") for `id`.
pub fn flash(ctx: &Context, id: Id) {
    ctx.data_mut(|data| data.insert_temp(id.with("flash"), Instant::now()));
    ctx.request_repaint();
}

/// Whether a flash recorded for `id` is still within `duration`.
pub fn flashing(ctx: &Context, id: Id, duration: Duration) -> bool {
    let started = ctx.data(|data| data.get_temp::<Instant>(id.with("flash")));
    let Some(started) = started else {
        return false;
    };
    let elapsed = started.elapsed();
    if elapsed < duration {
        ctx.request_repaint_after(duration - elapsed);
        true
    } else {
        false
    }
}

pub fn mix(from: Color32, to: Color32, t: f32) -> Color32 {
    from.lerp_to_gamma(to, t.clamp(0.0, 1.0))
}

/// A stable hash for `appear` keys.
pub fn key(value: impl std::hash::Hash) -> u64 {
    egui::Id::new(value).value()
}
