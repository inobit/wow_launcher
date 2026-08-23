# 终端交互输入 + 全屏仿真 实施方案

## 目标

终端页从只读日志升级为可交互终端：键盘直接输入到 ConPTY 子进程
（worldserver/authserver/mysql 控制台命令），输出用 vt100 全屏网格仿真渲染，
并支持 PTY 尺寸随窗口自适应。不采用 input 输入框方案。

## 技术选型

- **vt100 = "0.16"**（唯一新增依赖，内部基于 vte 0.15）：
  现成的"终端字节流解析 + 内存屏幕网格"，自带 scrollback、set_size 缩放、
  逐 cell 颜色/属性访问（fgcolor/bgcolor/bold/underline）。
  相比裸写 vte Perform 网格省 600~900 行；alacritty_terminal 过重不选。
- 键盘捕获：`iced::event::listen_with`（仅接受 fn 指针，无法捕获状态 →
  全局转发 KeyPressed，在 update() 中按条件过滤）。

## 数据流

```
输入：键盘事件 → listen_with 全局转发 → update 编码为终端字节
      → TermCmd 通道 → 写线程 → PTY master writer
输出：PTY reader 线程 → vt100::Parser.process() → Arc<Mutex<Parser>>
      → view 按 cell 渲染 rich_text 网格
```

## 改动明细

### A. 控制通道（process.rs）

- `enum TermCmd { Input(Vec<u8>), Resize(PtySize) }`
- build_service_stream 内建 mpsc channel；专用写线程循环：
  Input → writer.write_all；Resize → 锁 master_holder 调 master.resize()
  并对 parser set_size()（parser 与 reader 共用一把 Mutex）
- Sender 包 newtype（手写 impl Debug，Message 是 derive(Debug)）
- 新消息 ServiceTerminalReady(kind, TermHandle)：
  `TermHandle { cmds: Sender<TermCmd>, screen: Arc<Mutex<vt100::Parser>> }`

### B. 键盘捕获与编码（app.rs + process.rs/input.rs）

- 扩展现有 listen_with：KeyPressed{key,text,modifiers} → Message::TermKey(..)
- update() 过滤：active_nav==Terminal && 服务 Running && !close_pending
- encode_key(key, mods, text) -> Option<Vec<u8>> 标准编码表：
  Enter→\r、Backspace→\x7f、Tab→\t、Shift+Tab→ESC[Z、Esc→\x1b、
  方向键→ESC[A/B/C/D（Ctrl 加 ;5）、Home/End→ESC[H/F、Delete→ESC[3~、
  PgUp/PgDn→ESC[5~/6~、F1-F4→SS3 P/Q/R/S、F5-F12→ESC[n~、
  Ctrl+字母→控制字节(Ctrl+C=0x03)；可打印字符取 text UTF-8 字节
- Windows 按住自动重复天然支持（winit 重复 KeyPressed）

### C. 渲染模型重构（ui/terminal.rs，最大改动）

- State.logs: [Vec<String>;4] → screens: [Option<Arc<Mutex<Parser>>>;4]
  （scrollback 上限 2000 行，替代原 ring buffer）
- 渲染：遍历网格每行 → 相邻同样式 cell 合并为 span
  （Color::Idx(n) 走 TERM_ANSI 表，Rgb 直接映射，含 bold/underline）
  → 每行一个 rich_text → 原 scrollable + auto_scroll(true)
- 删除 parse_ansi / parse_sgr / classify_color（约 130 行）
- 光标 cell 反色渲染为块状光标（服务运行时显示）
- "清空" = 重建全新 Parser；保留"等待输出…"占位
- 性能：reader 每次 process() 后递增版本号，
  UI 缓存"版本号→已提取样式行数据"，避免每帧重复扫描全部 cell

### D. 尺寸自适应

- subscription 增加 WindowEvent::Resized(size) 监听
- update 估算行列：宽≈(窗口宽−208−44)/7.2px，高≈(窗口高−70)/15.6px，
  clamp 合理区间；与当前尺寸不同才发 TermCmd::Resize（天然去抖）
- 首次启动按窗口尺寸设初始 PtySize，替代硬编码 50×160

### E. 生命周期

| 时机                      | cmds[idx]            | screens[idx]                  |
|---------------------------|----------------------|-------------------------------|
| ServiceTerminalReady      | 存入                 | 存入                          |
| Stopped/Killed/StartFailed| 清空(→EOF)           | 保留(停服后仍可回看)          |
| 重启                      | 新句柄覆盖           | 新 Parser 覆盖 + 首行插入 [已重启] 标记 |

## 已知限制

1. 不支持 IME 中文输入
2. xterm 鼠标报告序列忽略（三个服务用不到）
3. resize 行列估算是近似值（±1~2 列对 ConPTY 无实质影响）
4. 客户端 wow.exe 不接入

## 实施顺序（每步可独立验证）

1. 加依赖 + TermCmd 通道 + ServiceTerminalReady + State 双字段
   （编译通过，行为不变）
2. 读线程接 vt100 Parser + terminal.rs 网格渲染替换旧管线
   （先只读验证显示正确）
3. 键盘捕获 + encode_key + Input 通路
   （在 worldserver 控制台敲命令验证回显/退格/Ctrl+C）
4. resize 自适应
5. 收尾：清空语义、重启 [已重启] 标记行、块状光标

涉及文件：process.rs（大改）、ui/terminal.rs（渲染重写）、app.rs（消息/状态）、
Cargo.toml（+vt100）。main.rs / config.rs / service.rs 不动。
