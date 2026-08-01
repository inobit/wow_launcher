# AGENTS.md — WoW Launcher

Rust + iced 桌面启动器，用于一键拉起 WoW 服务端三件套（MySQL / auth_server / world_server）与客户端。

## 构建命令

```powershell
cargo build --release          # 产物: target\release\wow_launcher.exe
cargo run                       # 调试运行
cargo build                     # 快速类型检查
```

- **默认 toolchain 必须为 `stable-x86_64-pc-windows-msvc`**（`rustup default stable-x86_64-pc-windows-msvc`）。需 VS Build Tools 2022+ 的 C++ 桌面开发组件。
- 项目根 `.cargo\config.toml` 已开启 `target-feature=+crt-static`，CRT 静态链接进 exe，产物自包含、不依赖外部 VCRUNTIME redist。另有 `[http] multiplexing=false`（本机网络下 HTTP/2 多路复用会导致 cargo 下载卡死，必须保留）。
- LTO + 单 codegen-unit，release 首次构建约 2-4 分钟。
- **部署**：`cargo build --release` 后复制 `target\release\wow_launcher.exe` 到 `D:\apps\wow_80_tianlan\azbotcore\`（覆盖同目录旧版，需先结束正在运行的启动器）。`wow_launcher.json` 与 exe 同目录，部署时保留。

## 功能说明

启动器管理 4 个应用，按角色分为两类：

| 服务 | 角色 | 终端捕获 | 单独停止/重启 |
|------|------|----------|----------------|
| MySQL (`mysqld.exe`) | 后端服务 | ✅ 有页签 | ✅ 有 |
| Auth Server | 后端服务 | ✅ 有页签 | ✅ 有 |
| World Server | 后端服务 | ✅ 有页签 | ✅ 有 |
| 客户端 (`wow.exe`) | 独立程序 | ❌ 压制不显示 | ❌ 仅启动 |

核心特性：

- **配置持久化**：4 个应用路径保存到可执行文件同目录的 `wow_launcher.json`。首次运行无配置时使用空缺省值。
- **终端输出捕获**：MySQL/Auth/World 通过 **ConPTY 伪终端**（`portable-pty`）启动，子进程 stdout 是真实控制台句柄，输出逐行实时到达且带 ANSI 颜色码（直接跑管道会被 CRT 4KB 块缓冲吞掉，这是 Auth 终端无输出的根因）。按服务分别缓存到 `Vec<String>`，通过 iced Subscription 推送给 UI。客户端 `spawn` 后立即丢弃 handle，输出完全压制。
- **一键启动**：MySQL → Auth/World 先进入"等待 MySQL 启动…"状态 → 异步轮询 `127.0.0.1:3306`（每 250ms，最长 30s）→ 就绪后 Auth + World 并发启动。停止/失败会取消排队中的服务。
- **一键停止**：将三个服务 `desired` 置 false（订阅被 drop），同时 `taskkill /PID <pid> /T /F` 杀**进程树**——因为 MySQL 配置指向 `MySQL.bat`，真实进程是 cmd→mysqld 两级，只杀直接子进程会遗留 mysqld 孤儿进程。
- **单独控制**：每个后端服务卡片有独立停止/重启按钮。重启 = `restart_pending` 标记 + 杀进程树 → 延时 800ms 后自动重新启动，期间状态保持"停止中"不闪烁。
- **退出保护**：关窗时若有服务在运行，弹确认框（是=停止全部并退出 / 否=取消关闭）；所有服务进程挂入带 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 的作业对象（`process.rs::job`），启动器无论崩溃还是被强杀，进程树都会被系统强制终止，不残留孤儿。客户端进程不挂作业对象，保持独立。
- **滚动去重**：每服务独立行缓冲，新行单向 append（带 1500 行上限的 ring drain），`scrollable().auto_scroll(true)` 跟随底部，避免重渲染导致的文字重复。
- **字体渲染**：默认 `Microsoft YaHei`（中文），终端用 `Font::MONOSPACE`（Consolas/Cascadia）。
- **终端配色**：Tokyo Night 风格（背景 `#16161E`），解析子进程输出的 ANSI SGR 颜色码（16 色 + 38;5 256 色 + 38;2 真彩色），无颜色码时按关键字着色（error→红 / warning→黄 / note/info→蓝 / system→青 / ready→绿）。
- **窗口图标**：`assets/icon.png`（由 `icon.jpg` 用 ImageMagick 去白底生成）经 `include_bytes!` 嵌入，运行时解码为 RGBA。

## UI 布局

Win11 风格深色主题，主窗口 1100×720（最小 900×600，可缩放）。

```
┌────────────┬───────────────────────────────────────┐
│            │                                       │
│  [图标]    │                                       │
│  WoW Launcher│            内容区 (Fill)            │
│  启动管理器 │     (由左侧导航切换三种视图)          │
│            │                                       │
│  ⌂ 主页    │                                       │
│  ⚙ 配置    │                                       │
│  ▷ 终端    │                                       │
│            │                                       │
│  [状态消息]│                                       │
└────────────┴───────────────────────────────────────┘
   208px                     Fill
```

### 左侧导航栏（`src/ui/mod.rs`）

- 固定宽 208px，深灰底 `#181818`
- 顶部：圆角图标容器（40px）+ 标题"WoW 启动器/启动管理器"
- 三个导航按钮：主页 / 配置 / 终端，选中态用 accent 软蓝边框高亮
- 底部：可选的序列状态消息（一键启动进度等）

### 主页（`src/ui/home.rs`，NavTab::Home）

```
┌──────────────────────────────────────────────────┐
│ [▶一键启动] [■一键停止]  运行中服务: N/3         │
├──────────────────────────────────────────────────┤
│ ● MySQL        已停止   [停止][重启]             │
│   D:\...\mysqld.exe                               │
├──────────────────────────────────────────────────┤
│ ● Auth Server  运行中    [停止][重启]            │
│   D:\...\auth_server.exe                          │
├──────────────────────────────────────────────────┤
│ ● World Server 已停止    [启动]                  │
│   D:\...\world_server.exe                         │
├──────────────────────────────────────────────────┤
│ ● 客户端                       [启动]            │
│   D:\...\wow.exe                                  │
└──────────────────────────────────────────────────┘
```

- 顶部主操作行：一键启动（accent 蓝）、一键停止（danger 橙）、运行中计数
- 4 个服务卡片：状态指示灯（● 绿=运行 / 黄=启动中·等待 / 红=错误 / 灰=停止）+ 名称 + 路径 + 状态文字 + 操作按钮
- 客户端卡片只有"启动"按钮，无停止/重启，且**不显示状态行**（独立进程不追踪）
- 一键启动需三个后端服务路径均已配置，否则按钮禁用并在状态栏提示

### 配置页（`src/ui/settings.rs`，NavTab::Settings）

```
┌──────────────────────────────────────────────────┐
│ 配置应用路径                                      │
│                                                  │
│ MySQL       [mysqld.exe 完整路径        ] [浏览…] │
│             已设置                                │
│ Auth Server [auth_server 可执行文件路径 ] [浏览…]│
│             未设置                                │
│ World Server[world_server 可执行文件路径] [浏览…]│
│             已设置                                │
│ 客户端      [wow.exe 客户端路径        ] [浏览…]  │
│             已设置                                │
│                                                  │
│ 客户端只会在主页点击启动后,以独立进程运行,输出不在此捕获。│
│ [还原]  [保存配置]                                │
│ [保存结果消息]                                   │
└──────────────────────────────────────────────────┘
```

- 编辑的是 `config_draft` 副本，保存才落盘并同步到 `config`
- "浏览…" 调 `rfd` 异步文件对话框（过滤 .exe）
- 每行右侧"已设置/未设置"徽标反映 **已保存** 配置（非草稿）状态

### 终端页（`src/ui/terminal.rs`，NavTab::Terminal）

```
┌──────────────────────────────────────────────────┐
│ [MySQL] [Auth Server] [World Server]     [清空]  │  ← TabBar(仅3个,无客户端)
├──────────────────────────────────────────────────┤
│ 2026-07-30 10:23:01 [Note] mysqld ready          │
│ 2026-07-30 10:23:02 [Note] X Plugin...           │  ← 等宽字体, auto_scroll
│ ...                                              │
└──────────────────────────────────────────────────┘
```

- 顶部自定义 tab 按钮行：MySQL / Auth / World 三个页签（带状态色圆点），无客户端页签
- 右侧"清空"按钮清空当前页签日志
- 终端区 Tokyo Night 深色底 `#16161E`，等宽字体 13px，自动滚动到底；逐行解析 ANSI SGR 颜色（16 色/256 色/真彩色），无颜色码时按关键字着色

## 配色常量（`src/theme.rs`）

| 常量 | 值 | 用途 |
|------|----|------|
| `BG` | `#1F1F1F` | 内容区背景 |
| `SIDEBAR` | `#181818` | 导航栏 |
| `CARD` | `#2B2B2B` | 服务卡片 |
| `CARD_BORDER` | `#3D3D3D` | 卡片/输入框边框 |
| `ACCENT` | `#60CDFF` | 主操作 + 选中高亮（Win11 蓝） |
| `SUCCESS` | `#6FCE9A` | 运行中 |
| `DANGER` | `#FF9974` | 停止/错误 |
| `WARNING` | `#FFC97A` | 启动中/未设置 |
| `TEXT` | `#F2F2F2` | 主文字 |
| `TEXT_MUTED` | `#B8B8B8` | 次要文字 |

终端另有 `TERM_*` 系列（Tokyo Night）：`TERM_BG #16161E` / `TERM_DEFAULT #C0CAF5` / `TERM_BLUE #7AA2F7` / `TERM_CYAN #7DCFFF` / `TERM_GREEN #9ECE6A` / `TERM_RED #F7768E` / `TERM_YELLOW #E0AF68` / `TERM_MUTED #9AA5CE`，以及 ANSI 16 色映射表 `TERM_ANSI`。

## 代码结构

```
src/
├── main.rs              # 入口；application builder；窗口/图标/字体/订阅；load_icon()
├── app.rs               # State、Message、update()、view()、subscription();核心 Elm 状态机
├── config.rs            # Config 结构 + JSON 读写(同目录 wow_launcher.json);path_for/set/path_set
├── service.rs           # ServiceKind 枚举(MySQL/Auth/World/Client)+ index/label/placeholder;Status 枚举
├── process.rs           # 进程生命周期:service_subscription(ConPTY 流)、job(作业对象)、kill_tree(taskkill 树杀)、wait_mysql_ready(TCP 轮询)、launch_client、delay_restart、browse_path
├── theme.rs             # 颜色常量 + 按钮风格函数(accent/danger/ghost/nav/tab)
└── ui/
    ├── mod.rs           # 总布局:左导航 + 右内容;build_sidebar();close_dialog(关闭确认弹窗);logo_handle(静态图标缓存)
    ├── home.rs          # 主页:一键按钮 + 4 服务卡片
    ├── settings.rs      # 配置页:4 路径行 + 保存/还原
    └── terminal.rs      # 终端页:3 页签 + ANSI 解析 + auto_scroll 日志
assets/icon.png          # 去白底后的透明 PNG 窗口图标(由 icon.jpg 生成)
.cargo/config.toml       # msvc crt-static 静态链接配置
wow_launcher.json        # 运行时生成；4 服务路径(运行后落盘)
```

## 关键实现细节

### 进程管理模型（ Elm + 声明式订阅）

`app::subscription(state)` 根据 `state.desired[kind] && path_set(kind)` 为每个后端服务返回一个 `Subscription::run_with`。desired=true 时订阅存活；desired=false 时订阅被 drop → UI 通道关闭 → 读线程 `push_msg` 失败 → kill 直接子进程，同时 `update()` 侧用 `taskkill /T /F` 杀**进程树**（`kill_tree`，防 cmd→mysqld 两级包装残留孤儿进程），完成后发 `ProcessKilled`。`pids[kind]` 记录各服务直接子进程 PID，`restart_pending[kind]` 标记重启流程（重启 = 树杀 → 延时 800ms → `SetDesired(true)` 重新 spawn；期间用户点"停止"会清标记使延时任务失效）。

### stdout 捕获（`process.rs::build_service_stream`）

`stream::channel` 内用 `portable-pty` 开 ConPTY 伪终端（50 行 × 160 列）spawn 子进程，`cwd` 设为可执行文件所在目录（保证 .bat/.conf 相对路径解析）。子进程退出由监控线程 `try_wait` 轮询（100ms），退出后关闭 HPCON → 读端 EOF；阻塞读线程把输出按 `\n` 分行（trim `\r`，跳过空行），经 futures mpsc 转发 `Message::ServiceOutput`，结束发 `ServiceStopped`。原始 ANSI 颜色码原样透传，由 UI 解析渲染。

### 一键启动序列（`app.rs::update`）

1. `StartAll` → 校验三路径 → MySQL `desired=true` + `Starting`（已在运行则跳过）→ Auth/World 进入 `Waiting`（"等待 MySQL 启动…"，卡片提供"停止"可取消排队）→ 返回 `wait_mysql_ready()` task
2. `MysqlReady`（且 `sequence_active` 仍为 true）→ 仍处 `Waiting` 的 Auth/World 同时 `desired=true` + `Starting` → 各自订阅自动 spawn
3. `MysqlReadyFailed(e)` → MySQL 置 Error + 清 pid，Auth/World 从 Waiting 回 Stopped

`wait_mysql_ready` 每 250ms 用 `tokio::net::TcpStream::connect("127.0.0.1:3306")`(400ms 超时)尝试连接，最多 120 次(30s)。

### 退出保护（`app.rs` + `process.rs::job`）

`main.rs` 窗口设置必须 `exit_on_close_request: false`——iced_winit 默认收到 `CloseRequested` 会**直接关窗**而不转发事件，拦截订阅将永远收不到。`subscription` 里 `iced::event::listen_with` 拦截 `WindowEvent::CloseRequested` → `Message::CloseRequested`：有服务在运行则 `close_pending=true` 弹确认框（`ui::close_dialog`，stack 覆盖层），否=取消关闭；是=`ConfirmClose(true)` → `stop_all()` 杀全部进程树 → 每个 `ProcessKilled` 后经 `maybe_close_after_stop` 检查，全部 PID 清空后 `iced::window::close(id)`。子进程 spawn 后立即 `job::assign(pid)` 挂入全局单例作业对象（`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`），启动器崩溃/被杀时系统自动终止整个进程树；进程已属其他作业时静默失败（仅优雅关闭兜底）。

## 常见问题排查

- **启动报 error 126 / 找不到模块**：确认用 msvc toolchain 且 `.cargo/config.toml` 的 crt-static 生效；-gnu toolchain 产物会因缺 MinGW DLL 触发 126。
- **关窗无确认提示**：确认 `main.rs` 窗口设置里 `exit_on_close_request: false`（iced_winit 默认 true 时 CloseRequested 直接关窗、不转发事件）。
- **图标不显示**：确认 `assets/icon.png` 存在且是有效 PNG（RGBA）。
- **一键启动按钮灰色**：MySQL/Auth/World 三个路径必须全部在配置页保存后才会启用。
- **修改源码后重新构建**：若 `cargo clean` 报 os error 32，先在任务管理器结束残留 `wow_launcher.exe`。
- **生成图标**：`magick icon.jpg -fuzz 20% -transparent white -trim assets/icon.png`