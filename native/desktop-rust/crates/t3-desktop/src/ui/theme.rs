//! T3 Code's dark theme, measured from the desktop app.

use gpui::{Hsla, Rgba, rgb, rgba};

use crate::view_model::Tone;

fn color(hex: u32) -> Hsla {
    rgb(hex).into()
}

/// White at `alpha` (0–1), the way the web theme layers borders and fills over black.
fn white(alpha: f32) -> Hsla {
    Rgba {
        r: 1.,
        g: 1.,
        b: 1.,
        a: alpha,
    }
    .into()
}

pub fn background() -> Hsla {
    color(0x000000)
}
pub fn sidebar() -> Hsla {
    color(0x020202)
}
pub fn sidebar_border() -> Hsla {
    color(0x1a1a1a)
}
pub fn text() -> Hsla {
    color(0xf5f5f5)
}
/// Assistant prose: the foreground at 90%.
pub fn prose() -> Hsla {
    text().opacity(0.9)
}
pub fn text_muted() -> Hsla {
    color(0xa1a1a1)
}
pub fn sidebar_muted() -> Hsla {
    color(0x9a9a9a)
}
pub fn text_faint() -> Hsla {
    color(0x6b6b6b)
}
pub fn border() -> Hsla {
    white(0.08)
}
pub fn input() -> Hsla {
    white(0.11)
}
pub fn card() -> Hsla {
    color(0x111111)
}
pub fn message() -> Hsla {
    color(0x1e1e1e)
}
pub fn composer() -> Hsla {
    color(0x121212)
}
pub fn composer_border() -> Hsla {
    color(0x242424)
}
pub fn row_hover() -> Hsla {
    white(0.06)
}
pub fn row_selected() -> Hsla {
    color(0x282929)
}
pub fn button_hover() -> Hsla {
    white(0.08)
}
pub fn code_background() -> Hsla {
    white(0.06)
}
pub fn primary() -> Hsla {
    color(0x346bf1)
}
pub fn destructive() -> Hsla {
    color(0xfb414a)
}
pub fn danger() -> Hsla {
    color(0xff6467)
}
pub fn success() -> Hsla {
    color(0x00d492)
}
pub fn info() -> Hsla {
    color(0x50a2ff)
}
pub fn warning() -> Hsla {
    color(0xfe9a00)
}
pub fn accent() -> Hsla {
    info()
}
pub fn selection() -> Hsla {
    rgba(0x346bf166).into()
}
pub fn scrim() -> Hsla {
    rgba(0x000000a6).into()
}

/// A project color from [`crate::sidebar::PROJECT_COLORS`] at `alpha`.
pub fn tint(hex: u32, alpha: f32) -> Hsla {
    color(hex).opacity(alpha)
}

pub fn tone(tone: Tone) -> Hsla {
    match tone {
        Tone::Info => info(),
        Tone::Warning => warning(),
        Tone::Error => danger(),
    }
}

pub const SANS: &str = ".SystemUIFont";

/// SF Mono on macOS (registered at startup), the platform monospace elsewhere.
pub fn mono() -> &'static str {
    if cfg!(target_os = "macos") {
        ".SF NS Mono"
    } else if cfg!(target_os = "windows") {
        "Consolas"
    } else {
        "DejaVu Sans Mono"
    }
}

/// Makes SF Mono available by family name; it ships with macOS but is hidden.
pub fn register_fonts(cx: &mut gpui::App) {
    if cfg!(target_os = "macos") {
        let fonts: Vec<_> = ["SFNSMono.ttf", "SFNSMonoItalic.ttf"]
            .iter()
            .filter_map(|name| std::fs::read(format!("/System/Library/Fonts/{name}")).ok())
            .map(std::borrow::Cow::Owned)
            .collect();
        if let Err(error) = cx.text_system().add_fonts(fonts) {
            tracing::debug!(%error, "SF Mono is unavailable");
        }
    }
}
