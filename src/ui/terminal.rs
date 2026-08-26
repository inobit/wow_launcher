use std::sync::atomic::Ordering;

use iced::widget::{button, column, container, rich_text, row, space, span, stack, text};
use iced::{never, Alignment, Color, Element, Font, Length};
use iced::widget::text::Wrapping;

use crate::app::{Message, State};
use crate::process::TermHandle;
use crate::service::{ServiceKind, Status};
use crate::theme;

// ===== 渲染模型 =====
//
// 终端体是"固定 rows 行 rich_text"的网格快照视图(不使用 scrollable):
// 输出由 vt100 全屏仿真解析, UI 按版本号提取可见网格渲染。
//
// 已知限制(有意接受的简化):
// 1. italic/dim 属性丢弃(rich_text 无对应支持; 背景色/反显/bold/underline 均已渲染);
// 2. 中文双宽字符按 1 格渲染, 可能轻微错位;
// 3. 行列估算为近似值(±1~2 列), 对 ConPTY 无实质影响。

/// 相同样式连续 cell 合并后的运行段(fg/bg 为应用反显与光标后的生效颜色)
#[derive(Debug)]
pub struct CellRun {
    pub text: String,
    pub fg: Color,
    /// None = 默认背景(透明, 露出终端底色)
    pub bg: Option<Color>,
    pub bold: bool,
    pub underline: bool,
}

/// 一次可见网格的提取快照(State 缓存, 按 version/offset 失效)
pub struct GridSnapshot {
    /// 提取时的 handle.version
    pub version: u64,
    /// 提取时的查看位置(0=实时屏)
    pub offset: usize,
    /// 恰好 rows 行; 行尾空白 run 已剪除
    pub rows: Vec<Vec<CellRun>>,
    /// (row, col), 仅实时视图(offset==0)时有值
    /// (光标渲染已通过提取时的 inverse run 实现, 此字段保留备用)
    #[allow(dead_code)]
    pub cursor: Option<(usize, usize)>,
}

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

    let idx = state.active_terminal.index();
    let has_snapshot = state.grids[idx].is_some();
    let has_terminal = state.terminals[idx].is_some();

    let body: Element<'a, Message> = if !has_snapshot && !has_terminal {
        // 服务从未有输出: 占位提示
        column![container(
            text("等待输出…").size(13).color(theme::TERM_MUTED)
        )
        .padding([20, 16])]
        .width(Length::Fill)
        .into()
    } else {
        let mut col = column![];
        if let Some(snap) = &state.grids[idx] {
            for runs in &snap.rows {
                col = col.push(render_row(runs));
            }
        }
        let grid: Element<'a, Message> = col.width(Length::Fill).spacing(0).into();

        let errors = &state.errors[idx];
        if errors.is_empty() {
            grid
        } else {
            // 错误通知(启动失败/一键启动失败等)叠加在网格底部——不占网格行高,
            // 否则行数估算需为其预留高度, 无错误时白白损失终端行
            let mut ecol = column![];
            let extra = errors.len().saturating_sub(5);
            if extra > 0 {
                ecol = ecol.push(
                    text(format!("… 另有 {extra} 条更早的错误"))
                        .size(12)
                        .color(theme::TERM_MUTED),
                );
            }
            for e in errors.iter().skip(extra) {
                ecol = ecol.push(text(e).size(12).color(theme::TERM_RED));
            }
            let card = container(ecol.spacing(2))
                .padding(6)
                .width(Length::Fill)
                .style(|_| container::Style {
                    background: Some(iced::Background::Color(Color {
                        a: 0.92,
                        ..theme::TERM_BG
                    })),
                    border: iced::Border::default().rounded(6.0),
                    ..Default::default()
                });
            stack![
                grid,
                container(card)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .align_x(Alignment::Start)
                    .align_y(Alignment::End)
                    .padding(4),
            ]
            .into()
        }
    };

    let term = container(body)
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
            .on_press(Message::ClearTerminal(kind)),
    )
    .into()
}

// ===== 网格渲染 =====

/// 渲染一行: 每个 run 一个 span; 背景/反显/光标在提取期已折算为生效 fg/bg。
/// 必须关闭换行: 网格行宽由估算保证不溢出, 一旦换行纵向对齐会整体错位。
fn render_row(runs: &[CellRun]) -> Element<'static, Message> {
    if runs.is_empty() {
        // 空行占一个行高, 保持网格行对齐
        return rich_text([span(" ").font(Font::MONOSPACE)])
            .size(13.0)
            .font(Font::MONOSPACE)
            .line_height(1.3) // 与 process.rs CELL_H=13*1.3 对齐, 避免行高漂移致底行被裁
            .wrapping(Wrapping::None)
            .on_link_click(never)
            .into();
    }
    let spans = runs
        .iter()
        .map(|run| {
            let font = if run.bold {
                Font {
                    weight: iced::font::Weight::Bold,
                    ..Font::MONOSPACE
                }
            } else {
                Font::MONOSPACE
            };
            let mut s = span(run.text.clone()).color(run.fg).font(font);
            if let Some(bg) = run.bg {
                s = s.background(bg);
            }
            if run.underline {
                s = s.underline(true);
            }
            s
        })
        .collect::<Vec<_>>();

    rich_text(spans)
        .size(13.0)
        .font(Font::MONOSPACE)
        .line_height(1.3) // iced 默认就是 1.3(13*1.3=16.9px); 显式写死避免与估算漂移
        .wrapping(Wrapping::None)
        .on_link_click(never)
        .into()
}

// ===== 网格提取 =====

/// 锁定 parser, 把当前查看位置的可见网格提取为 CellRun 快照。
/// 必须在持锁期间一次性完成(含 set_scrollback 切换), 调用方不要在循环外反复加锁。
pub fn snapshot_grid(handle: &TermHandle, want_offset: usize) -> GridSnapshot {
    let mut p = handle.parser.lock().unwrap();
    // 1. 探测回滚总行数(clamp 技巧: set_scrollback(usize::MAX) 后读回 clamp 值)
    p.screen_mut().set_scrollback(usize::MAX);
    let total_scrollback = p.screen().scrollback();
    // 2. 定位查看位置
    let offset = want_offset.min(total_scrollback);
    p.screen_mut().set_scrollback(offset);

    // 3. 逐 cell 提取, 同样式合并为 run
    let (rows_n, cols_n) = p.screen().size();
    let cursor = p.screen().cursor_position();

    let mut out: Vec<Vec<CellRun>> = Vec::with_capacity(rows_n as usize);
    for r in 0..rows_n {
        let mut runs: Vec<CellRun> = Vec::new();
        // 光标 run 不与相邻同样式 cell 合并(光标样式是位置语义而非内容语义)
        let mut prev_cursor = false;
        for c in 0..cols_n {
            let Some(cell) = p.screen().cell(r, c) else { continue };
            let raw = cell.contents();
            // 空白 cell(未写入)按普通空格参与合并, 行尾统一剪除
            let mut text: String = if raw.is_empty() { " ".into() } else { raw.into() };

            // 光标: 仅实时视图且未隐藏光标时, 光标格拆独立 run
            let on_cursor = offset == 0
                && !p.screen().hide_cursor()
                && (r, c) == cursor;

            let mut fg = map_fg(cell.fgcolor());
            let mut bg = map_bg(cell.bgcolor());
            // SGR 反显: 交换前景/背景(默认前景=TERM_DEFAULT, 默认背景=终端底色)
            if cell.inverse() {
                let old_fg = fg;
                fg = bg.unwrap_or(theme::TERM_BG);
                bg = Some(old_fg);
            }
            // 块状光标: 通体用 █ 实心块 + 该格前景色渲染。不依赖 span 背景高亮
            // (部分后端对空白格的背景盒不渲染), 保证光标无论底下是什么都可见
            if on_cursor {
                text = "\u{2588}".to_string();
            }
            let bold = cell.bold();
            let underline = cell.underline();

            let mergeable = !on_cursor
                && !prev_cursor
                && matches!(
                    &runs.last(),
                    Some(run) if run.bold == bold
                        && run.underline == underline
                        && run.fg == fg
                        && run.bg == bg
                );
            if mergeable {
                runs.last_mut().unwrap().text.push_str(&text);
            } else {
                runs.push(CellRun {
                    text,
                    fg,
                    bg,
                    bold,
                    underline,
                });
            }
            prev_cursor = on_cursor;
        }
        // 行尾剪除: pop 掉末尾纯空白且无背景的 run(保留行首/行中缩进;
        // 带背景的空白可见需保留; 光标已是 █ 实心块, 必不会被剪)
        while runs
            .last()
            .is_some_and(|run| run.bg.is_none() && run.text.chars().all(|ch| ch == ' '))
        {
            runs.pop();
        }
        out.push(runs);
    }

    // 4. 还原实时视图(重要: 否则下次提取位置错乱)
    p.screen_mut().set_scrollback(0);

    GridSnapshot {
        version: handle.version.load(Ordering::Acquire),
        offset,
        rows: out,
        cursor: (offset == 0).then(|| (cursor.0 as usize, cursor.1 as usize)),
    }
}

// ===== 颜色映射 =====

/// xterm 256 色: 0-15 用主题 ANSI 表, 16-231 为 6×6×6 色立方, 232-255 为 24 级灰度
fn xterm256(idx: u8) -> Color {
    match idx {
        0..=15 => theme::TERM_ANSI[idx as usize],
        16..=231 => {
            let i = idx - 16;
            // 分量公式: v == 0 → 0, 否则 55 + 40v
            let comp = |v: u16| if v == 0 { 0u8 } else { (55 + 40 * v) as u8 };
            let (r, g, b) = ((i / 36) as u16, ((i % 36) / 6) as u16, (i % 6) as u16);
            Color::from_rgb8(comp(r), comp(g), comp(b))
        }
        _ => {
            let g = (8 + (idx - 232) as u16 * 10).clamp(0, 255) as u8;
            Color::from_rgb8(g, g, g)
        }
    }
}

/// 前景映射: ANSI 黑(Idx 0)在 TERM_BG 深色底上不可见, 替换为 Tokyo Night 注释灰
fn map_fg(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => theme::TERM_DEFAULT,
        vt100::Color::Idx(0) => theme::TERM_FG_BLACK,
        other => map_color(other),
    }
}

fn map_color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => theme::TERM_DEFAULT,
        vt100::Color::Idx(n) => xterm256(n),
        vt100::Color::Rgb(r, g, b) => Color::from_rgb8(r, g, b),
    }
}

/// 默认背景映射为 None(透明, 露出终端底色), 其余映射为具体颜色
fn map_bg(c: vt100::Color) -> Option<Color> {
    match c {
        vt100::Color::Default => None,
        other => Some(map_color(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::TermHandle;
    use std::sync::atomic::AtomicU64;
    use std::sync::{Arc, Mutex};

    fn handle(p: vt100::Parser) -> TermHandle {
        let (tx, _rx) = std::sync::mpsc::sync_channel(1);
        TermHandle {
            cmds: tx,
            parser: Arc::new(Mutex::new(p)),
            version: Arc::new(AtomicU64::new(0)),
        }
    }

    #[test]
    fn cursor_on_blank_cell_renders_solid_block() {
        let mut p = vt100::Parser::new(5, 20, 100);
        p.process(b"AC> ");
        // 光标应位于 (0,4), 该格为空白
        assert_eq!(p.screen().cursor_position(), (0, 4));
        assert!(!p.screen().hide_cursor());
        let h = handle(p);
        let snap = snapshot_grid(&h, 0);
        let row = &snap.rows[0];
        let last = row.last().expect("cursor run");
        // 空白格光标 = 实心块 █ + 该格前景色
        assert_eq!(last.text, "\u{2588}", "光标格应为实心块: {row:?}");
        assert_eq!(last.fg, theme::TERM_DEFAULT);
    }

    /// 滚动语义回归: 光标只在实时屏(offset=0)渲染; 历史(offset>0)不渲染光标。
    /// 且当输出超出视口高度时 scrollback 才能滚动(不足时无可滚动内容)。
    #[test]
    fn cursor_only_renders_on_live_screen_and_history_needs_scrollback() {
        let mut p = vt100::Parser::new(5, 20, 300); // 视口 5 行
        // 写入 12 行内容, 超出 5 行视口 → 前 7 行进入 scrollback
        for i in 0..12u8 {
            p.process(format!("LINE-{i:02}").as_bytes());
            // 移动到下一行; 到底后回车会滚屏
            p.process(b"\r\n");
        }
        let h = handle(p);

        // 探 total: 内容超出视口 → scrollback 应有存量
        let total = {
            let mut g = h.parser.lock().unwrap();
            g.screen_mut().set_scrollback(usize::MAX);
            let t = g.screen().scrollback();
            g.screen_mut().set_scrollback(0);
            t
        };
        assert!(total >= 1, "内容超出 5 行视口后应产生回滚, 实际 {total}");

        // offset=0(实时屏): 应含块状光标(实心块 █)
        let live = snapshot_grid(&h, 0);
        let has_cursor_block = live.rows.last().unwrap().iter().any(|r| r.text.contains("\u{2588}"));
        assert!(has_cursor_block, "实时屏应渲染块状光标");
        let live_first = live.rows[0].iter().map(|r| r.text.as_str()).collect::<String>();
        let live_had_earliest = live_first.contains("LINE-00")
            || live.rows[1..].iter().flatten().any(|r| r.text.contains("LINE-00"));
        assert!(!live_had_earliest, "最早的 LINE-00 应已滚出实时屏");

        // 滚到最旧一屏(offset=total): 进入历史, 不渲染光标, 且能看到更早内容
        let hist = snapshot_grid(&h, total);
        let no_cursor = hist.rows.iter().flatten().all(|r| !r.text.contains("\u{2588}"));
        assert!(no_cursor, "历史视图不应渲染光标");
        let hist_all = hist.rows.iter().flatten().map(|r| r.text.as_str()).collect::<String>();
        assert!(
            hist_all.contains("LINE-00") || hist_all.contains("LINE-01"),
            "历史顶部应出现最早内容 LINE-00/01, 实际 {hist_all:?}"
        );
        // 历史视图与实时视图看到的内容不同(证明滚动确实改变了视图)
        let hist_first = hist.rows[0].iter().map(|r| r.text.as_str()).collect::<String>();
        assert_ne!(hist_first, live_first, "滚动后首行应变化");
    }
}
