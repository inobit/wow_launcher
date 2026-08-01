use iced::widget::{button, column, container, row, scrollable, space, text};
use iced::{never, widget::rich_text, widget::span, Alignment, Color, Element, Font, Length};

use crate::app::{Message, State};
use crate::service::{ServiceKind, Status};
use crate::theme;

pub fn view<'a>(state: &'a State) -> Element<'a, Message> {
    let tabs = row![
        tab_button(ServiceKind::Mysql, state),
        tab_button(ServiceKind::Auth, state),
        tab_button(ServiceKind::World, state),
        space().width(Length::Fill),
        clear_button(state.active_terminal),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .width(Length::Fill);

    let lines = state.log(state.active_terminal);

    let body = if lines.is_empty() {
        column![container(
            text("等待输出…").size(13).color(theme::TERM_MUTED)
        )
        .padding([20, 16])]
        .width(Length::Fill)
    } else {
        let mut col = column![].width(Length::Fill);
        for line in lines {
            col = col.push(render_line(line));
        }
        col.spacing(1)
    };

    let scrolled = scrollable(body)
        .direction(scrollable::Direction::Vertical(
            scrollable::Scrollbar::new().width(10.0).margin(4.0),
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .auto_scroll(true);

    let term = container(scrolled)
        .padding(12)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(iced::Background::Color(theme::TERM_BG)),
            border: iced::Border::default().rounded(8.0).color(theme::CARD_BORDER).width(1.0),
            ..Default::default()
        });

    column![tabs, space().height(10), term]
        .spacing(0)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn tab_button<'a>(kind: ServiceKind, state: &'a State) -> Element<'a, Message> {
    let is_active = state.active_terminal == kind;
    let dot_color = match state.status[kind.index()] {
        Status::Running => theme::SUCCESS,
        Status::Starting | Status::Stopping | Status::Waiting => theme::WARNING,
        Status::Error => theme::DANGER,
        Status::Stopped => theme::CARD_BORDER,
    };
    let label_color = if is_active { theme::TEXT } else { theme::TEXT_MUTED };
    let content = rich_text([
        span("\u{25CF} ").color(dot_color).font(Font::MONOSPACE),
        span(kind.label()).color(label_color),
    ])
    .size(13.0)
    .font(Font::MONOSPACE)
    .on_link_click(never);

    button(content)
        .padding([8, 18])
        .style(theme::tab_button_style(is_active))
        .on_press(Message::ServiceSelected(kind))
        .into()
}

fn clear_button<'a>(kind: ServiceKind) -> Element<'a, Message> {
    container(
        button(text("清空").size(12))
            .style(theme::ghost_button_style())
            .padding([6, 12])
            .on_press(Message::ClearLog(kind)),
    )
    .into()
}

// ===== 终端行渲染 =====

/// 渲染一行日志: 解析 ANSI SGR 颜色码, 无颜色码时按关键字归类着色
fn render_line(line: &str) -> Element<'static, Message> {
    let fallback = classify_color(line);
    let spans = parse_ansi(line)
        .into_iter()
        .map(|(text, color)| span(text).color(color.unwrap_or(fallback)).font(Font::MONOSPACE))
        .collect::<Vec<_>>();

    rich_text(spans)
        .size(13.0)
        .font(Font::MONOSPACE)
        .on_link_click(never)
        .into()
}

/// 把一行文本按 ANSI SGR 转义序列切分为 (文本, 颜色) 片段, 并剥掉所有控制码
fn parse_ansi(line: &str) -> Vec<(String, Option<Color>)> {
    let mut segments: Vec<(String, Option<Color>)> = Vec::new();
    let mut current = String::new();
    let mut color: Option<Color> = None;

    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    // 收集参数直到最终字节 (字母)
                    let mut params = String::new();
                    let mut final_byte = ' ';
                    while let Some(&d) = chars.peek() {
                        if d.is_ascii_alphabetic() {
                            final_byte = d;
                            chars.next();
                            break;
                        }
                        params.push(d);
                        chars.next();
                    }
                    if final_byte == 'm' {
                        color = parse_sgr(&params, color);
                    }
                    // 其他 CSI 序列(光标移动等)直接丢弃
                }
                Some(']') => {
                    // OSC 序列(如 ]0;title): 跳到 BEL 或 ST 结束
                    chars.next();
                    while let Some(&d) = chars.peek() {
                        chars.next();
                        if d == '\u{07}' {
                            break;
                        }
                        if d == '\u{1b}' {
                            let _ = chars.next();
                            break;
                        }
                    }
                }
                _ => current.push(c),
            }
        } else {
            current.push(c);
        }
    }

    if !current.is_empty() {
        segments.push((current, color));
    }
    segments
}

fn parse_sgr(params: &str, previous: Option<Color>) -> Option<Color> {
    let parts: Vec<&str> = params.split(';').collect();
    let mut color = previous;
    let mut i = 0;
    while i < parts.len() {
        match parts[i] {
            "" | "0" | "39" => color = None, // 重置
            "1" | "2" | "3" | "4" | "7" | "9" => {} // 样式: 忽略
            "38" => {
                // 38;5;n 256 色 / 38;2;r;g;b 真彩色
                if parts.get(i + 1) == Some(&"5") {
                    if let Some(n) = parts.get(i + 2).and_then(|s| s.parse::<usize>().ok()) {
                        if n < 16 {
                            color = Some(theme::TERM_ANSI[n]);
                        }
                    }
                    i += 2;
                } else if parts.get(i + 1) == Some(&"2") {
                    let rgb: Vec<Option<u8>> = (i + 2..i + 5)
                        .map(|j| parts.get(j).and_then(|s| s.parse::<u8>().ok()))
                        .collect();
                    if rgb.iter().all(|v| v.is_some()) {
                        color = Some(iced::Color::from_rgb8(
                            rgb[0].unwrap(),
                            rgb[1].unwrap(),
                            rgb[2].unwrap(),
                        ));
                    }
                    i += 4;
                }
            }
            _ => {
                if let Ok(n) = parts[i].parse::<usize>() {
                    match n {
                        30..=37 => color = Some(theme::TERM_ANSI[n - 30]),
                        90..=97 => color = Some(theme::TERM_ANSI[n - 90 + 8]),
                        _ => {}
                    }
                }
            }
        }
        i += 1;
    }
    color
}

/// 无 ANSI 颜色时, 根据日志关键字分类着色
fn classify_color(line: &str) -> Color {
    let lower = line.to_lowercase();
    if lower.contains("error") || lower.contains("fatal") || lower.contains("failed") {
        theme::TERM_RED
    } else if lower.contains("warning") || lower.contains("warn") {
        theme::TERM_YELLOW
    } else if lower.contains("[system]") {
        theme::TERM_CYAN
    } else if lower.contains("note")
        || lower.contains("info")
        || lower.contains("load")
        || lower.contains("connect")
    {
        theme::TERM_BLUE
    } else if lower.contains("debug") || lower.contains("trace") {
        theme::TERM_MUTED
    } else if lower.contains("ready")
        || lower.contains("success")
        || lower.contains("started")
        || lower.contains("up to date")
    {
        theme::TERM_GREEN
    } else {
        theme::TERM_DEFAULT
    }
}
