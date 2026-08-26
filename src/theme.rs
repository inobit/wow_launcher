use iced::widget::{button, container};
use iced::{Color, Theme};

pub const BG: Color = Color::from_rgb8(0x1F, 0x1F, 0x1F);
pub const SIDEBAR: Color = Color::from_rgb8(0x18, 0x18, 0x18);
pub const CARD: Color = Color::from_rgb8(0x2B, 0x2B, 0x2B);
pub const CARD_BORDER: Color = Color::from_rgb8(0x3D, 0x3D, 0x3D);
pub const ACCENT: Color = Color::from_rgb8(0x60, 0xCD, 0xFF);
pub const ACCENT_SOFT: Color = Color::from_rgb8(0x2A, 0x47, 0x66);
pub const SUCCESS: Color = Color::from_rgb8(0x6F, 0xCE, 0x9A);
pub const DANGER: Color = Color::from_rgb8(0xFF, 0x99, 0x74);
pub const WARNING: Color = Color::from_rgb8(0xFF, 0xC9, 0x7A);
pub const TEXT: Color = Color::from_rgb(0.95, 0.95, 0.95);
pub const TEXT_MUTED: Color = Color::from_rgb(0.72, 0.72, 0.72);

// ===== 终端 Tokyo Night 配色 =====
pub const TERM_BG: Color = Color::from_rgb8(0x16, 0x16, 0x1E);
pub const TERM_DEFAULT: Color = Color::from_rgb8(0xC0, 0xCA, 0xF5);
pub const TERM_MUTED: Color = Color::from_rgb8(0x9A, 0xA5, 0xCE);
pub const TERM_RED: Color = Color::from_rgb8(0xF7, 0x76, 0x8E);
/// ANSI 黑作前景时的替代色: 纯黑在 TERM_BG 上完全不可见, 用 Tokyo Night 注释灰保证可读
/// (仅前景路径使用; 背景路径仍用 TERM_ANSI[0] 真黑)
pub const TERM_FG_BLACK: Color = Color::from_rgb8(0x56, 0x5F, 0x89);

/// ANSI 16 色映射到 Tokyo Night 调色板(索引 0-15)
pub const TERM_ANSI: [Color; 16] = [
    Color::from_rgb8(0x16, 0x16, 0x1E), // 0  black
    Color::from_rgb8(0xF7, 0x76, 0x8E), // 1  red
    Color::from_rgb8(0x9E, 0xCE, 0x6A), // 2  green
    Color::from_rgb8(0xE0, 0xAF, 0x68), // 3  yellow
    Color::from_rgb8(0x7A, 0xA2, 0xF7), // 4  blue
    Color::from_rgb8(0xBB, 0x9A, 0xF7), // 5  magenta
    Color::from_rgb8(0x7D, 0xCF, 0xFF), // 6  cyan
    Color::from_rgb8(0xC0, 0xCA, 0xF5), // 7  white
    Color::from_rgb8(0x41, 0x48, 0x68), // 8  bright black
    Color::from_rgb8(0xFF, 0x7A, 0x93), // 9  bright red
    Color::from_rgb8(0xB9, 0xF2, 0x7C), // 10 bright green
    Color::from_rgb8(0xFF, 0x9E, 0x64), // 11 bright yellow
    Color::from_rgb8(0x7A, 0xA2, 0xF7), // 12 bright blue
    Color::from_rgb8(0xBB, 0x9A, 0xF7), // 13 bright magenta
    Color::from_rgb8(0x7D, 0xCF, 0xFF), // 14 bright cyan
    Color::from_rgb8(0xFF, 0xFF, 0xFF), // 15 bright white
];

pub fn theme() -> Theme {
    Theme::Dark
}

pub fn card_style() -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(CARD)),
        border: iced::Border::default().rounded(8.0).color(CARD_BORDER).width(1.0),
        text_color: Some(TEXT),
        ..Default::default()
    }
}

pub fn sidebar_style() -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(SIDEBAR)),
        text_color: Some(TEXT),
        ..Default::default()
    }
}

fn soft_hover(status: button::Status) -> Option<iced::Background> {
    match status {
        button::Status::Hovered | button::Status::Pressed => {
            Some(iced::Background::Color(Color::from_rgb8(0x33, 0x33, 0x33)))
        }
        _ => None,
    }
}

// 禁用态统一用深灰底 + 灰文字，与启用态形成明显区分
fn disabled_style(style: &mut button::Style) {
    style.background = Some(iced::Background::Color(Color::from_rgb8(0x33, 0x33, 0x33)));
    style.text_color = Color::from_rgb8(0x77, 0x77, 0x77);
    style.border = iced::Border::default().rounded(6.0);
}

pub fn nav_button_style(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let mut style = button::Style::default();
        if active {
            style.background = Some(iced::Background::Color(ACCENT_SOFT));
            style.text_color = TEXT;
            style.border = iced::Border::default().rounded(6.0).color(ACCENT).width(1.0);
        } else {
            style.background = soft_hover(status);
            style.text_color = TEXT_MUTED;
        }
        style
    }
}

pub fn accent_button_style() -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let mut style = button::Style::default();
        if status == button::Status::Disabled {
            disabled_style(&mut style);
            return style;
        }
        let bg = match status {
            button::Status::Hovered => Color::from_rgb8(0x4D, 0xB4, 0xE0),
            button::Status::Pressed => Color::from_rgb8(0x39, 0x8A, 0xAC),
            _ => ACCENT,
        };
        style.background = Some(iced::Background::Color(bg));
        style.text_color = Color::BLACK;
        style.border = iced::Border::default().rounded(6.0);
        style
    }
}

pub fn danger_button_style() -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let mut style = button::Style::default();
        if status == button::Status::Disabled {
            disabled_style(&mut style);
            return style;
        }
        let bg = match status {
            button::Status::Hovered => Color::from_rgb8(0xE0, 0x7A, 0x4F),
            button::Status::Pressed => Color::from_rgb8(0xB5, 0x5E, 0x3A),
            _ => DANGER,
        };
        style.background = Some(iced::Background::Color(bg));
        style.text_color = Color::BLACK;
        style.border = iced::Border::default().rounded(6.0);
        style
    }
}

pub fn ghost_button_style() -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let mut style = button::Style::default();
        if status == button::Status::Disabled {
            style.text_color = Color::from_rgb8(0x66, 0x66, 0x66);
            style.border = iced::Border::default().rounded(6.0).color(CARD_BORDER).width(1.0);
            return style;
        }
        style.background = soft_hover(status);
        style.text_color = TEXT;
        style.border = iced::Border::default().rounded(6.0).color(CARD_BORDER).width(1.0);
        style
    }
}

pub fn tab_button_style(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let mut style = button::Style::default();
        if active {
            style.background = Some(iced::Background::Color(CARD));
            style.text_color = TEXT;
            style.border = iced::Border::default().rounded(6.0).color(ACCENT).width(2.0);
        } else {
            style.background = soft_hover(status);
            style.text_color = TEXT_MUTED;
        }
        style
    }
}