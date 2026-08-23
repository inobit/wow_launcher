# 终端交互输入 + 全屏仿真 实施方案（修订版 v2）

> 本文档为唯一实施依据，目标读者是执行实施的 AI 助手。所有 API 签名均已对照
> crates.io / docs.rs 核实（核实日期依据 crate 最新版）。执行时如遇本文档与
> 编译器报错冲突，以编译器为准并在完成报告中记录偏差。
>
> **运行环境为 Windows 10/11**（ConPTY、taskkill、crt-static）。开发机为 Linux，
> 只能做编译级验证；标注【Win】的行为必须到 Windows 上真机验证。

## 0. 与 v1 方案的差异摘要

| # | 变更 | 原因 |
|---|------|------|
| 1 | 渲染架构从"iced scrollable + 全历史 rich_text 行列表"改为"**固定 rows×cols 网格视图** + vt100 内部 scrollback 导航" | vt100 0.16 无连续读取全部历史的公开 API（`Screen::scrollback()` 只返回当前查看位置）；全量提取数千行 span 每帧重建性能不可控；iced 0.14 的 `auto_scroll(true)` 实际是鼠标中键自动滚动，不是吸底 |
| 2 | 读线程保留向 UI 发消息（`TerminalData`） | 保住现有"订阅 drop → push_msg 断连 → child.kill()"停止兜底 |
| 3 | 清空 = 向共享 Parser 注入 `ESC[2J ESC[3J ESC[H`，不替换 Arc | UI 替换 Arc 后读线程仍写旧 Parser，清空一次即永久失显 |
| 4 | `TermHandle` 整体手写 Debug | `Mutex<vt100::Parser>` 是否 Debug 未定，避免 Message derive(Debug) 编译失败 |
| 5 | resize 增加 200ms 防抖任务；初始尺寸不进 `ServiceRecipe` | 拖拽高频触发洪泛；recipe 是订阅身份，尺寸入参会因 resize 触发进程重启 |
| 6 | 键盘编码补全：DECCKM 应用光标模式（SS3）、修饰键参数化 CSI、Alt 前缀、Shift+PgUp/PgDn 本地滚动 | 世界服等控制台常用方向键/功能键 |

## 1. 目标

终端页从只读日志升级为可交互终端：

- 键盘输入直接写入 ConPTY 子进程（worldserver / authserver / mysql 控制台命令）；
- 输出经 vt100 全屏网格仿真渲染（支持光标定位、清屏、颜色、粗体/下划线/反显）；
- PTY 尺寸随窗口自适应（带防抖）;
- 支持翻阅最近 2000 行回滚缓冲（滚轮 / Shift+PgUp/PgDn）；
- 不采用 input 输入框方案；客户端 wow.exe 不接入（保持现状）。

## 2. 已核实的依赖事实（实施时直接引用，勿再猜测）

### 2.1 vt100（新增依赖）

```toml
vt100 = "0.16"        # crates.io 最新 0.16.2，内部依赖 vte ^0.15.0
```

已核实的 API（docs.rs/vt100/0.16.2）：

```rust
// Parser
Parser::new(rows: u16, cols: u16, scrollback_len: usize) -> Self
fn process(&mut self, bytes: &[u8])          // 喂原始字节流
fn screen(&self) -> &Screen
fn screen_mut(&mut self) -> &mut Screen      // 注意：Parser 自身没有 set_size！

// Screen
fn set_size(&mut self, rows: u16, cols: u16) // resize 用这个
fn size(&self) -> (u16, u16)                 // (rows, cols)
fn set_scrollback(&mut self, rows: usize)    // 设置查看位置，超界自动 clamp
fn scrollback(&self) -> usize                // 返回"当前查看位置"，0=实时屏幕
                                             // ⚠ 不是总行数！
fn cursor_position(&self) -> (u16, u16)
fn cell(&self, row: u16, col: u16) -> Option<&Cell>
fn application_cursor(&self) -> bool         // DECCKM，键盘编码要用
fn hide_cursor(&self) -> bool
fn row_wrapped(&self, row: u16) -> bool      // 本方案不用，列出防混淆

// Cell
fn contents(&self) -> &str                   // 空格单元格返回 "" 或 " "
fn has_contents(&self) -> bool
fn fgcolor(&self) -> Color
fn bgcolor(&self) -> Color
fn bold(&self) -> bool
fn underline(&self) -> bool
fn inverse(&self) -> bool                    // italic/dim 存在但本方案忽略

// Color 枚举
enum Color { Default, Idx(u8), Rgb(u8, u8, u8) }
```

**scrollback 总行数的获取技巧**（`set_scrollback` 会 clamp 到实际大小）：

```rust
screen_mut().set_scrollback(usize::MAX);
let total = screen.scrollback();   // clamp 后的值 = 当前可用回滚总行数
```

### 2.2 iced 0.14（现有版本，勿升级）

```rust
// event：listen_with 只接受函数指针，无法捕获状态（现有 app.rs:405 已在用）
event::listen_with(f: fn(Event, event::Status, window::Id) -> Option<Message>)
// listen_with 过滤掉 RedrawRequested/SystemThemeChanged/PlatformSpecific，
// 键盘、鼠标滚轮、窗口 Resized 事件都会到达

// keyboard::Event（iced::keyboard）
enum Event {
    KeyPressed { key: Key, modified_key: Key, physical_key: Physical,
                 location: Location, modifiers: Modifiers,
                 text: Option<SmolStr>, repeat: bool },
    KeyReleased { .. }, ModifiersChanged(Modifiers),
}
enum Key { Named(key::Named), Character(String), Unidentified }

// scrollable：
//   auto_scroll(bool) 是"鼠标中键按住拖动滚动"，与吸底无关 → 本方案完全不用它
//   本方案不使用 scrollable，改用固定网格（见 §5）
// Task 操作（备用，本方案主路径不用）：
scrollable::snap_to(id, offset) / scrollable::snap_to_end(id) -> Task<Message>

// window 尺寸：iced::Size { width: f32, height: f32 }，WindowEvent::Resized(Size)
```

winit 在 Windows 上 `KeyPressed.text` 来自 WM_CHAR 路径，可打印字符可用【Win】。

### 2.3 portable-pty 0.8（已在用，现状确认）

- `pair.master.take_writer() -> Result<Box<dyn Write + Send>>`——现 `process.rs:130`
  拿到即丢弃（`let _writer`），这是输入通路的唯一挂点；需 `write_all` 后 `flush`。
- `MasterPty::resize(&self, PtySize) -> Result<()>`——`&self`，可经
  `Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>`（现有 `master_holder`）跨线程调用。
- `try_clone_reader()` 同步阻塞 `Read`，现状不变。

## 3. 总体架构

```
输入：键盘/滚轮事件 → listen_with(fn指针全局转发)
      → update() 按 active_nav==Terminal && 服务Running 过滤
      → encode_key 编码为字节 → TermCmd::Input 经 sync_channel
      → 专用写线程 → PTY master writer（write_all + flush）

输出：PTY reader 线程(阻塞read 8KB) → parser.lock().process(bytes)
      → version.fetch_add(1) → push_msg(Message::TerminalData(kind))
      → UI 收到后若为活动页签则锁定 parser 提取可见网格快照存入 State
      → view() 用快照渲染 rows 行 rich_text（无 scrollable）

尺寸：listen_with 捕获 WindowEvent::Resized → 200ms 防抖任务
      → TermCmd::Resize → 写线程: master.resize + parser.screen_mut().set_size

停止：desired=false → UI drop 订阅 → sender 关闭
      → 读线程 push_msg 失败 → child.kill()（原有兜底，必须保留）
      → 同时 update() 侧 kill_tree 杀进程树（原有逻辑不变）
```

线程与锁职责（全部在 `build_service_stream` 内创建）：

| 线程 | 持有 | 操作的锁 | 职责 |
|------|------|----------|------|
| 主 async task | sender | — | 每 50ms 轮询读线程存活（现状不变） |
| 监控线程 | child_holder, master_holder | 先 child_holder 后 master_holder（不嵌套） | 100ms try_wait；退出后 `*master_holder=None` 关 HPCON（现状不变） |
| **写线程（新）** | rx, writer, master_holder | master_holder 短暂持有；parser 短暂持有 | recv 循环处理 Input/Resize；通道关闭或写失败则退出 |
| **读线程（改）** | reader, tx, child_holder | parser 短暂持有；child_holder 在 push_msg 内 | 字节块喂 parser；每块发一条 `TerminalData`；EOF 发 `ServiceStopped` |

**锁序约定（防死锁，必须遵守）**：任何时刻只嵌套 `child_holder → 无`、
`master_holder → 无`、`parser → 无` 三种单层持有；禁止同时持有两把锁。
现有监控线程"退出后先释放 child_holder 再拿 master_holder"的写法保持不变。

## 4. process.rs 改动明细

### 4.1 新类型

```rust
use std::sync::atomic::{AtomicU64, Ordering};

/// 终端控制命令（UI → 写线程）
#[derive(Debug)]
pub enum TermCmd {
    /// 键盘编码后的输入字节，原样写入 PTY
    Input(Vec<u8>),
    /// 调整 PTY 与解析器尺寸
    Resize(PtySize),
}

/// UI 持有的终端句柄。整体手写 Debug（vt100::Parser 的 Debug 实现不作假设）
#[derive(Clone)]
pub struct TermHandle {
    /// 有界通道容量 512：sync_channel(512)，满时 send 阻塞（天然背压）
    pub cmds: std::sync::mpsc::SyncSender<TermCmd>,
    /// 屏幕状态，reader 写 / UI 读，共用一把锁
    pub parser: Arc<Mutex<vt100::Parser>>,
    /// 数据版本号：reader 每次 process 后 +1，Resize 时也 +1；UI 据此刷新快照缓存
    pub version: Arc<AtomicU64>,
}

impl std::fmt::Debug for TermHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TermHandle").finish_non_exhaustive()
    }
}
```

注意：`PtySize` 若未实现 Debug 则给 `TermCmd` 手写 Debug（编译器会提示）。

### 4.2 初始尺寸（模块级函数，供初始 openpty 与 resize 共用）

```rust
/// 网格字体常量：MONOSPACE 13px 下 Consolas/Cascadia 的近似度量
const CELL_W: f32 = 7.2;    // 字符 advance
const CELL_H: f32 = 15.6;   // 行高

/// 侧边栏 208 + 终端容器左右 padding 24 + 滚动条预留 14
const CHROME_W: f32 = 208.0 + 24.0 + 14.0;
/// 页签行高约 34 + 页签下间距 10 + 终端容器上下 padding 24 + 余量 4
const CHROME_H: f32 = 34.0 + 10.0 + 24.0 + 4.0;

/// 由窗口尺寸估算行列数（近似值即可，±1~2 列对 ConPTY 无实质影响）
pub fn grid_size_for_window(w: f32, h: f32) -> (u16, u16) {
    let cols = ((w - CHROME_W) / CELL_W).floor().clamp(40.0, 240.0) as u16;
    let rows = ((h - CHROME_H) / CELL_H).floor().clamp(10.0, 60.0) as u16;
    (rows, cols)
}
```

`openpty` 时使用 `grid_size_for_window(app::WINDOW_W, app::WINDOW_H)`（在 app.rs
定义 `pub const WINDOW_W: f32 = 1100.0; pub const WINDOW_H: f32 = 720.0;`，
与 main.rs 窗口设置保持一致）。**初始尺寸不得放进 `ServiceRecipe`**——recipe 是
订阅身份，字段变化会导致订阅重建、进程重启。

### 4.3 build_service_stream 重写

保留：openpty 失败/spawn 失败 → `ServiceStartFailed`；`drop(pair.slave)`；
pid 提取 + `job::assign`；`ServiceStarted`；监控线程；主 task 轮询循环。
删除：读线程内的逐行拆分逻辑（`\n` 分行、trim、空行过滤全部移除）。

新流程（按顺序）：

```rust
stream::channel(256, async move |mut sender| {
    let (rows, cols) = grid_size_for_window(crate::app::WINDOW_W, crate::app::WINDOW_H);
    // 1. openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
    //    （失败 → ServiceStartFailed，return，同现状）
    // 2. CommandBuilder + cwd + spawn_command + drop(slave)（同现状）
    // 3. pid + job::assign + ServiceStarted（同现状）
    // 4. try_clone_reader（失败 → kill + ServiceStartFailed，同现状）
    // 5. let mut writer = pair.master.take_writer()（失败 → kill + ServiceStartFailed）
    //
    // 6. 新建共享状态：
    //    let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 2000)));
    //    let version = Arc::new(AtomicU64::new(0));
    //    let (tx_cmd, rx_cmd) = std::sync::mpsc::sync_channel::<TermCmd>(512);
    //
    // 7. child_holder / master_holder（同现状，先建好再发 Ready，保证句柄完整）
    //
    // 8. 写线程：
    //    std::thread::spawn(move || loop {
    //        match rx_cmd.recv() {
    //            Ok(TermCmd::Input(bytes)) => {
    //                if writer.write_all(&bytes).or_else(|e| Err(e))
    //                    .and_then(|_| writer.flush()).is_err() { break; }
    //            }
    //            Ok(TermCmd::Resize(size)) => {
    //                // 锁序：master_holder 单独短暂持有，随后 parser 单独短暂持有
    //                if let Some(m) = master_holder.lock().unwrap().as_ref() {
    //                    let _ = m.resize(size);
    //                }
    //                parser.lock().unwrap()
    //                      .screen_mut().set_size(size.rows, size.cols);
    //                version.fetch_add(1, Ordering::Release);
    //            }
    //            Err(_) => break, // 所有 SyncSender 已 drop（订阅结束）
    //        }
    //    });
    //
    // 9. 经 push_msg 发送 Message::ServiceTerminalReady(kind, TermHandle {
    //        cmds: tx_cmd.clone(), parser: parser.clone(), version: version.clone(),
    //    })；断连（false）则 return
    //
    // 10. 读线程（替换原逐行逻辑）：
    //     let mut buf = [0u8; 8192];
    //     loop {
    //         match reader.read(&mut buf) {
    //             Ok(0) => break,
    //             Ok(n) => {
    //                 parser.lock().unwrap().process(&buf[..n]);
    //                 version.fetch_add(1, Ordering::Release);
    //                 // 关键：仍走 push_msg 保住断连检测（通道关 → child.kill()）
    //                 let mut guard = child_holder.lock().unwrap();
    //                 if !push_msg(&mut out_sender,
    //                              Message::TerminalData(reader_kind),
    //                              guard.as_deref_mut().unwrap()) { return; }
    //             }
    //             Err(_) => break,
    //         }
    //     }
    //     // EOF：发 ServiceStopped（同现状）
    //
    // 11. 主 task 保持"轮询读线程存活直到结束"（同现状）
})
```

`push_msg` 函数本身不改。`service_subscription` / `ServiceRecipe` 签名不变。

## 5. ui/terminal.rs 重写（渲染模型）

### 5.1 数据结构（定义在本文件，State 持有）

```rust
/// 相同样式连续 cell 合并后的运行段
pub struct CellRun {
    pub text: String,
    pub fg: iced::Color,
    pub bg: Option<iced::Color>,   // None = 透明（露出 TERM_BG）
    pub bold: bool,
    pub underline: bool,
    pub inverse: bool,
}

/// 一次可见网格的提取快照（缓存）
pub struct GridSnapshot {
    pub version: u64,          // 提取时的 handle.version
    pub offset: usize,         // 提取时的查看位置（0=实时屏）
    pub rows: Vec<Vec<CellRun>>, // 恰好 rows 行；行内尾随空白已剪除
    pub cursor: Option<(usize, usize)>, // (row, col)，仅 offset==0 且显示光标时 Some
}
```

### 5.2 网格提取（核心函数，放本文件）

```rust
/// 锁定 parser，把当前查看位置的可见网格提取为 CellRun 快照。
/// 必须在持锁期间一次性完成（含 set_scrollback 切换），调用方不要在循环外反复加锁。
pub fn snapshot_grid(handle: &TermHandle, want_offset: usize) -> GridSnapshot {
    let mut p = handle.parser.lock().unwrap();
    // 1. 探测回滚总行数（clamp 技巧）
    p.screen_mut().set_scrollback(usize::MAX);
    let total_scrollback = p.screen().scrollback();
    // 2. 定位查看位置：want_offset 由调用方预先锚定/clamp 到 0..=total_scrollback
    let offset = want_offset.min(total_scrollback);
    p.screen_mut().set_scrollback(offset);
    // 3. 逐 cell 提取
    let (rows_n, cols_n) = p.screen().size();
    let cursor = p.screen().cursor_position();
    let mut out = Vec::with_capacity(rows_n as usize);
    for r in 0..rows_n {
        let mut runs: Vec<CellRun> = Vec::new();
        let mut cur: Option<CellRun> = None;
        for c in 0..cols_n {
            let Some(cell) = p.screen().cell(r, c) else { continue };
            // 光标反显：仅实时视图；hide_cursor 时跳过
            let on_cursor = offset == 0
                && !p.screen().hide_cursor()
                && (r, c) == cursor;
            let ch = cell.contents();
            let blank = !cell.has_contents();
            // 同样式合并：blank 视作延续上一 run 的普通空格（用于剪除前的对齐判断）
            let same = cur.as_mut().is_some_and(|run| {
                run.bold == cell.bold() && run.underline == cell.underline()
                    && run.fg == map_color(cell.fgcolor(), true)
                    && run.bg == map_bg(cell.bgcolor())
            });
            if blank && runs.last().map_or(cur.is_none(), |_| false) { continue }
            // 具体规则：
            //   - cell 无内容且当前行尚未产生内容 → 跳过（行首空白剪除由最终 trim 完成）
            //   - 有内容：same 时 append 文本；否则开新 run
            //   - on_cursor 时对该字符所在 run 标记 inverse = true（整字符反转）
            //   - run.text.push_str(ch)；宽字符(is_wide)照常 push，宽度误差可接受
            ...
        }
        // 行尾剪除：pop 末尾纯空白 run
        out.push(runs);
    }
    // 4. 还原实时视图（重要：否则下次提取位置错乱）
    p.screen_mut().set_scrollback(0);
    GridSnapshot { version: handle.version.load(Ordering::Acquire), offset, rows: out,
                   cursor: (offset == 0).then(|| (cursor.0 as usize, cursor.1 as usize)) }
}
```

实现说明（实施者按此落地，上面注释中的 `...` 处展开）：

- 合并判定只比较 bold/underline/fg/bg 四项（italic/dim 忽略）；
- `inverse` 单独处理：光标所在字符无论原样式如何，渲染时前景取背景、背景取前景；
  简化实现——提取时若 `on_cursor`，把该字符拆成独立 run 并置 `inverse: true`；
- 行尾剪除：从行末 pop 掉 `text.chars().all(|ch| ch == ' ')` 的 run；
- `map_color(Color::Idx(n), is_fg)`：见 §5.3。

### 5.3 颜色映射

```rust
fn xterm256(idx: u8) -> iced::Color {
    match idx {
        0..=15 => theme::TERM_ANSI[idx as usize],           // 已有 16 色表
        16..=231 => {                                        // 6×6×6 色立方
            let i = idx - 16;
            let comp = |v: u16| if v == 0 { 0u8 } else { (55 + 40 * v) as u8 };
            let (r, g, b) = ((i / 36) as u16, ((i % 36) / 6) as u16, (i % 6) as u16);
            iced::Color::from_rgb8(comp(r), comp(g), comp(b))
        }
        _ => {                                               // 24 级灰度
            let g = (8 + (idx - 232) as u16 * 10).clamp(0, 255) as u8;
            iced::Color::from_rgb8(g, g, g)
        }
    }
}
fn map_color(c: vt100::Color, _fg: bool) -> iced::Color {
    match c {
        vt100::Color::Default => theme::TERM_DEFAULT,
        vt100::Color::Idx(n) => xterm256(n),
        vt100::Color::Rgb(r, g, b) => iced::Color::from_rgb8(r, g, b),
    }
}
fn map_bg(c: vt100::Color) -> Option<iced::Color> {
    match c {
        vt100::Color::Default => None,                       // 透明 → TERM_BG
        other => Some(map_color(other, false)),
    }
}
```

### 5.4 view 结构

```
TabBar（现状不动：3 页签 + 状态圆点 + 清空按钮）
终端体（container，TERM_BG 背景，圆角边框，padding 12 —— 现状样式不动）：
    if 服务从未有输出（grids[idx] 为 None 且 terminals[idx] 为 None）:
        "等待输出…" 占位（现状文案）
    else:
        column: 错误通知行（见 §5.6，若有）
              + rows 行 rich_text：
                  每个 run → span(run.text)
                      .color(if inverse { bg.unwrap_or(TERM_BG) } else { run.fg })
                      （背景色：iced 0.14 span 无背景填充能力 → 反显用前景互换模拟，
                        普通 bg 仅当 Some 且非深色时忽略——接受此简化，注明）
                      .font(Font::MONOSPACE)
                  bold → 无法单独加粗（rich_text 无 weight 支持）→ 忽略，注明
                  underline → span 暂无下划线支持 → 忽略，注明
                  .size(13.0).font(Font::MONOSPACE)
        整体不包 scrollable（固定 rows 行恰好占满容器高度）
```

**明确接受的渲染简化**（写进代码注释与"已知限制"）：背景色仅通过反显光标体现；
bold/underline/italic/dim 属性丢弃；中文双宽字符按 1 格渲染可能轻微错位。
理由：三个服务（mysqld/auth/world）的控制台输出以单色文本为主，以上简化不影响使用。

### 5.5 滚动模型（替代 scrollable）

- `State.scroll_offset: [usize; 4]`，0 = 实时屏，>0 = 回看 N 行；
- `State.prev_scrollback_total: [usize; 4]`，上次探测到的回滚总数（锚定用）；
- **新数据到达（TerminalData）且用户正在回看**：`offset += (新 total − 旧 total)`
  保持视觉锚定（vt100 的位置语义是"距最新内容的偏移"，新行追加会把历史推远）；
  offset==0 则不动；
- 滚轮向上（`TermScroll(+n)`）：`offset = min(offset + n, total)`；向下减到 0 为止；
- **任何键盘输入发送成功后**：`offset = 0`（回到实时屏，标准终端行为）；
- Shift+PgUp / Shift+PgDn：±半屏（rows/2）；普通 PgUp/PgDn 发送给远端程序；
- offset 变化和 version 变化都会使缓存失效 → 见 §6 缓存刷新时机。

### 5.6 错误通知（替代被删除的 push_log）

`ServiceStartFailed` 与一键启动失败的 `[一键启动] …` 消息改存
`state.errors: [Vec<String>; 4]`（上限 20 条，超出丢最旧）。终端页在该服务
网格上方渲染红色小字（TERM_RED，12px）；服务成功启动时清空对应 errors。

## 6. app.rs 改动明细

### 6.1 Message 变更

```diff
- ServiceOutput(ServiceKind, String),
+ TerminalData(ServiceKind),                          // 读线程心跳+脏标记
+ ServiceTerminalReady(ServiceKind, TermHandle),
+ TermKeyEvent(iced::keyboard::Event),               // 全局转发，update 内过滤
+ TermScroll(i32),                                   // 滚轮：正=向历史，负=向实时
+ TermPageScroll(i32),                               // Shift+PgUp/PgDn：±rows/2
+ WindowResized(iced::Size),
+ TermResizeTick,                                    // 防抖到期
+ SendTermBytes(Vec<u8>),                            // update 内转 Task 写入
- ClearLog(ServiceKind) → ClearTerminal(ServiceKind) // 语义变化见 §7
```

### 6.2 State 变更

```diff
- logs: [Vec<String>; 4],
+ terminals: [Option<TermHandle>; 4],
+ grids: [Option<GridSnapshot>; 4],       // 每服务一份快照缓存
+ scroll_offset: [usize; 4],
+ prev_scrollback_total: [usize; 4],
+ errors: [Vec<String>; 4],
+ applied_grid: [(u16, u16); 4],          // 各服务 PTY 当前生效尺寸（初始为默认值）
+ pending_resize: Option<(u16, u16)>,
+ last_window_size: (f32, f32),           // 每次窗口事件都更新（不限页签）
```

删除 `State::log/push_log/MAX_LOG_LINES`。

### 6.3 update() 分支行为表（逐条实现）

| 消息 | 行为 |
|------|------|
| `ServiceTerminalReady(kind, h)` | `terminals[idx]=Some(h)`；`applied_grid[idx]=(rows,cols)`（h.parser 里查）；若 `terminals[idx]` 原本已有句柄（重启场景）→ 向**新** parser `process(b"\x1b[33m--- 已重启 ---\x1b[0m\r\n")` 并 version+1；`grids[idx]=None`（强制重提取）；`scroll_offset[idx]=0`；`errors[idx].clear()` |
| `TerminalData(kind)` | 仅当 `kind==active_terminal && active_nav==Terminal`：若 `terminals[idx]` 版本号 ≠ 快照版本或 offset 变化 → 锚定调整（§5.5）→ `grids[idx]=Some(snapshot_grid(h, offset))`。非活动页签不提取（切页签时 `NavSelected` 补提取） |
| `NavSelected(kind)`（终端页签） | 现状 + 强制刷新该页签快照（同上提取逻辑，封装成 `refresh_grid(state, kind) -> ()` 私有函数复用） |
| `TermKeyEvent(e)` | 守卫链：`close_pending` → 忽略；`active_nav!=Terminal` → 忽略；`status[idx]!=Running` 或 `terminals[idx]` 为 None → 忽略。解构 `KeyPressed{key, text, modifiers, ..}`：`key==Named(PageDown/Up) && modifiers.shift()` → `TermPageScroll(∓rows/2)`（本地滚动，不发给远端）；其余调 `encode_key(key, text.as_deref(), modifiers, app_cursor)`，得 `Some(bytes)` → `scroll_offset=0` + 返回 `Task::perform` 异步 `send`（见下） |
| `SendTermBytes(bytes)` | `Task::perform(async move { let _ = handle.cmds.send(TermCmd::Input(bytes)); })`——`SyncSender::send` 满时阻塞属预期背压；注意把 `TermHandle` clone 进 async 块。**实现提示**：`TermKeyEvent` 分支里直接构造该 Task 返回，无需中间消息；仅当闭包捕获困难时才用 `SendTermBytes` 中转，二选一，优先前者 |
| `TermScroll(delta)` / `TermPageScroll(n)` | 守卫同上（nav/页签）；调整 `scroll_offset` → 刷新快照 |
| `WindowResized(size)` | `last_window_size=size`；若 `active_nav!=Terminal` → 忽略；否则算出 `(rows,cols)=grid_size_for_window(...)`，存 `pending_resize=Some(...)` 并返回防抖任务 `Task::perform(sleep(200ms), |_| TermResizeTick)`（每次 Resized 都覆盖 pending 并重发任务；旧任务到期时发现 pending 已被取走/不同则无害） |
| `TermResizeTick` | 取走 `pending_resize`；与各服务 `applied_grid[idx]` 比较，不同的才 `handle.cmds.send(TermCmd::Resize(PtySize{rows,cols,pixel_width:0,pixel_height:0}))`（同步 send 即可，通道满概率极低；为稳妥用 `try_send`，满则丢弃本次 resize）并更新 `applied_grid[idx]` |
| `ClearTerminal(kind)` | 见 §7 |
| `ServiceStopped / ProcessKilled` | 现状逻辑之外：`terminals[idx]` **保留**（写线程随后自然退出，句柄留着无害）？——**否**：置 `terminals[idx]=None` 使输入立即失效（状态≠Running 也已拦截输入，此处兜底）；`grids[idx]` **保留**（停服后仍可回看最后一屏） |
| `ServiceStartFailed` | 现状 + `errors[idx].push(format!("[启动失败] {e}"))`（替代 push_log） |
| `MysqlReadyFailed` | 现状 + `errors[Mysql].push(format!("[一键启动] {e}"))` |

`encode_key` 中需要的 `app_cursor`（DECCKM）获取方式：
`handle.parser.lock().unwrap().screen().application_cursor()`。

### 6.4 subscription()

现有 `listen_with` 闭包扩展（仍是 fn 指针，无状态，全部转发由 update 过滤）：

```rust
iced::event::listen_with(|event, status, window| {
    // 只在事件未被任何 widget 捕获时转发，避免抢配置页输入框的键
    if matches!(status, iced::event::Status::Captured) { return None; }
    match event {
        iced::Event::Window(iced::window::Event::CloseRequested) =>
            Some(Message::CloseRequested(window)),
        iced::Event::Window(iced::window::Event::Resized(size)) =>
            Some(Message::WindowResized(size)),
        iced::Event::Keyboard(ke @ iced::keyboard::Event::KeyPressed { .. }) =>
            Some(Message::TermKeyEvent(ke)),
        iced::Event::Mouse(iced::mouse::Event::WheelScrolled { delta }) => {
            // Lines.y 正值=向上滚（翻历史）；Pixels 按 /20 折算成行数
            let lines = match delta {
                iced::mouse::ScrollDelta::Lines { y, .. } => y * 3.0,
                iced::mouse::ScrollDelta::Pixels { y, .. } => y / 20.0,
            };
            (lines.round() as i32 != 0).then(|| Message::TermScroll(lines.round() as i32))
        }
        _ => None,
    }
})
```

### 6.5 键盘编码表（`process.rs` 内新建 `mod input` 或独立文件，纯函数便于测试）

```rust
/// 把一次按键编码为发往 PTY 的字节序列
/// app_cursor: 远端是否处于 DECCKM 应用光标模式（影响方向键用 SS3 还是 CSI）
pub fn encode_key(key: &Key, text: Option<&str>,
                  mods: iced::keyboard::Modifiers, app_cursor: bool) -> Option<Vec<u8>>
```

规则按优先级依次匹配（命中即返回）：

1. **Ctrl+字母**（mods.control 且 key 为 Character 且小写后 a-z）：
   `\x01..=\x1a`（字母 ASCII & 0x1f）。特例：Ctrl+C=`0x03`、Ctrl+D=`0x04`、
   Ctrl+Z=`0x1a` 一律照发【Win：ConPTY 将 0x03 翻译为 CTRL_C_EVENT】。
   Ctrl+@/Space=`0x00`、Ctrl+[=`0x1b`、Ctrl+\=`0x1c`、Ctrl+]=`0x1d`、
   Ctrl+^=`0x1e`、Ctrl+_=`0x1f`、Ctrl+/=`0x1f`。
2. **命名键表**（key==Named(n)，修饰参数化见第 3 条）：

   | 键 | 无修饰 | 说明 |
   |----|--------|------|
   | Enter | `\r` | 【Win】ConPTY 输入管道 `\r` 才是 Enter |
   | Backspace | `\x7f` | |
   | Tab | `\t` | |
   | Escape | `\x1b` | |
   | ArrowUp/Down/Left/Right | app_cursor ? `ESC OA/OB/OC/OD`(SS3) : `ESC[A/B/C/D` | |
   | Home / End | `ESC[H` / `ESC[F` | app_cursor 时同理用 SS3 OH/OF |
   | Delete | `ESC[3~` | |
   | Insert | `ESC[2~` | |
   | PageUp / PageDown | `ESC[5~` / `ESC[6~` | **Shift+ 时被 §6.3 截走做本地滚动，不会到这里** |
   | F1–F4 | SS3 `P Q R S` | |
   | F5–F12 | `ESC[15~ 17~ 18~ 19~ 20~ 21~ 23~ 24~` | 依次对应 F5..F12 |
   | Space | 走第 4 条（text 路径，`" "`） | Named(Space) 的 text 通常为 Some(" ") |

3. **修饰键参数化**：上述 CSI 类键（方向/Home/End/Delete/Insert/PgUp/PgDn/F5+）
   当 `shift||alt||control` 任一按下时，改为 `ESC[1;<m><终字节>` 或
   `ESC[<n>;<m>~`，`m = 1 + shift(1) + alt(2) + ctrl(4)`。例：Ctrl+Up=`ESC[1;5A`，
   Shift+F5=`ESC[15;2~`。SS3 键带修饰时统一降级为 CSI 形式 `ESC[1;<m>A` 等。
4. **可打印字符**：`text` 为 `Some(s)` 且非 control → 返回 `s.as_bytes()`；
   `mods.alt()` 按下时前置 `0x1b`（Alt+X → `ESC x`）。
5. 其余（含 KeyReleased 之外的 Unidentified、logo 修饰等）→ `None`。

补充单元测试用例（`#[cfg(test)]`，Linux 可跑）：Enter→`\r`、Ctrl+C→`\x03`、
上方向键 CSI/SS3 两态、Ctrl+Up→`ESC[1;5A`、F8→`ESC[19~`、Shift+PgUp 不进入编码
（由 update 截走）、普通字符透传 UTF-8。

## 7. 清空语义（B1 修复，务必按此实现）

```rust
Message::ClearTerminal(kind) => {
    let idx = kind.index();
    match &state.terminals[idx] {
        Some(h) => {
            // 服务仍在运行：原地清屏 + 清回滚，绝不替换 Arc
            let v = h.version.clone();
            let mut p = h.parser.lock().unwrap();
            p.process(b"\x1b[2J\x1b[3J\x1b[H"); // 清可视区 + 清 scrollback + 光标归位
            drop(p);
            v.fetch_add(1, Ordering::Release);   // 触发 UI 重提取
        }
        None => {}                               // 已停止：无可清
    }
    state.grids[idx] = None;
    state.scroll_offset[idx] = 0;
    state.prev_scrollback_total[idx] = 0;
}
```

## 8. 生命周期矩阵

| 时机 | terminals[idx] | grids[idx] | scroll_offset | 其他 |
|------|----------------|------------|---------------|------|
| ServiceTerminalReady | 存入新句柄 | None（强制重提取） | 0 | 重启场景插入黄色 `--- 已重启 ---` 标记行；errors 清空 |
| ServiceStopped / ProcessKilled / StartFailed | **None**（输入失效） | **保留**（可回看最后一屏） | 保留 | |
| 重启（SetDesired true → 新 Ready） | 新句柄覆盖 | None | 0 | 旧 Parser 连同旧画面一起释放 |
| ClearTerminal | 不变 | None | 0 | 运行中注入清屏序列 |
| 切换终端页签 | 不变 | 懒提取 | 不变 | |

## 9. 删除清单

- `ui/terminal.rs`：`parse_ansi`、`parse_sgr`、`classify_color`、`render_line`、
  `auto_scroll` 相关（约 130 行）；
- `app.rs`：`logs` 字段、`push_log`、`log()`、`MAX_LOG_LINES`、
  `Message::ServiceOutput` 分支；
- `theme.rs` 的 `TERM_ANSI` 保留（Idx 0..=15 映射仍使用）。

## 10. 已知限制（发布说明/代码注释引用）

1. 不支持 IME 中文输入（iced KeyPressed 组合期 text 为 None）；
2. xterm 鼠标报告序列不实现（三服务控制台用不到）；滚轮固定用于本地回滚；
3. resize 行列估算为近似值（±1~2 列），ConPTY 下无实质影响；
4. Ctrl+C 会关闭 mysqld（数据库停止）——符合终端语义，卡片按钮仍是一键停止的正途；
5. 背景色/bold/underline/italic/dim 渲染简化（§5.4）；中文双宽字符按 1 格计；
6. 客户端 wow.exe 不接入终端。

## 11. 实施顺序（每步独立可验证）

1. **依赖 + 通道骨架**：Cargo.toml 加 `vt100 = "0.16"`；TermCmd/TermHandle/
   grid_size_for_window；build_service_stream 建通道+写线程+发
   ServiceTerminalReady（读线程暂维持旧行为）；State 双字段 + update 存储。
   ✅ 验证：`cargo build` 通过；启动任一服务，终端显示与改造前一致（旧管线仍在）。
2. **读线程 vt100 化 + 网格渲染**：读线程改字节块喂 parser + TerminalData 心跳；
   terminal.rs 全量重写（snapshot_grid/颜色映射/view/CellRun）；删除旧 ANSI 解析。
   ✅ 验证：`cargo build`；启动服务看到彩色网格输出；滚轮可回看 2000 行；
   清空按钮生效且后续输出继续显示。
3. **键盘输入**：input 编码模块 + 单元测试；TermKeyEvent 转发链；输入通路。
   ✅ 验证【Win】：worldserver 控制台敲命令回显正常；退格/方向键/Ctrl+C 生效；
   输入后自动跳回底部。
4. **尺寸自适应**：Resized 监听 + 防抖 + Resize 命令；首次启动按窗口估初值。
   ✅ 验证【Win】：拖拽窗口后 `mode con` 类输出列数变化；全屏程序（如 mysql
   客户端表格）不串行。
5. **收尾**：清空语义、重启标记行、errors 通知行、光标反显、已知限制注释。
   ✅ 验证【Win】：重启 Auth 后顶部出现标记；停服后画面可回看；一键启动失败
   信息出现在终端页。

涉及文件：`Cargo.toml`、`src/process.rs`（大改）、`src/ui/terminal.rs`（重写）、
`src/app.rs`（消息/状态/订阅）。`main.rs`、`config.rs`、`service.rs` 不动。

**Windows 真机回归清单**（全部完成后过一遍）：MySQL.bat 树杀无残留孤儿；
一键启动/停止/单独重启全流程；关窗确认弹窗；作业对象兜底（任务管理器强杀
启动器，服务应随之退出）；终端三项服务的输入输出与滚动；crt-static 构建
（`cargo build --release`）产物在无 VC redist 机器可直接运行。
