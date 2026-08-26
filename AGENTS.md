# AGENTS.md — WoW Launcher

> 本文档面向 AI 编码代理与新加入的开发者，按 **目标 → 架构 → 常用命令 → 设计约束** 组织。
> 阅读目标：快速进入开发状态，且**不违反第 4 节任何一条设计约束**（多数约束背后是一次真实事故）。

## 1. 项目目标

Windows 桌面启动器（Rust + iced），一键拉起/停止 WoW 服务端三件套与客户端：

| 应用 | 角色 | 终端捕获 | 单独停止/重启 |
|------|------|----------|----------------|
| MySQL (`mysqld.exe` 或包装 .bat) | 后端服务 | ✅ 有页签 | ✅ 有 |
| Auth Server | 后端服务 | ✅ 有页签 | ✅ 有 |
| World Server | 后端服务 | ✅ 有页签 | ✅ 有 |
| 客户端 (`wow.exe`) | 独立程序 | ❌ 压制不显示 | ❌ 仅启动 |

核心用户价值：① 一键按序启动（MySQL 就绪后再起 Auth/World）；② 内置可视化终端（实时彩色输出 + 键盘交互 + 回看）；③ 无论正常退出还是被强杀都不残留孤儿进程。适用范围与使用方法见 `README.md`。

## 2. 架构

### 2.1 总体形态

Elm 架构单体 GUI：`app.rs` 是唯一状态机（State + Message + update/subscription/view），UI 全部是 State 的纯函数投影。子进程生命周期由 **声明式订阅** 管理：`desired[kind] && path_set(kind)` → 存活一个 `Subscription::run_with(ServiceRecipe{kind, path})`；desired=false 订阅即 drop。

### 2.2 终端数据流（核心链路）

```
spawn(ConPTY) ──► 读线程 read(8KB) ──► vt100::Parser.process ──► version+1
                     │                                            │
                     └── push_msg(Message::TerminalData) ──► update: refresh_grid
                                                                  │ 活动页签才提取
                                                                  ▼
                                    snapshot_grid(parser 持锁一次完成: 定位 scrollback
                                    → 逐 cell 提取颜色/反显/光标 → 还原 offset=0)
                                                                  ▼
                                          grids[idx] 缓存(version+offset 失效)
                                                                  ▼
                                     terminal.rs view → render_row(rich_text spans)

键盘: KeyPressed(listen_with 全局转发) → encode_key(CSI/SS3/C0/UTF-8) 
      → TermCmd::Input → 写线程 → PTY writer → 子进程回显 → 同上链路回到屏幕
```

原始字节只进 vt100，`TerminalData` 消息仅作心跳/脏标记。设计细节见 `docs/terminal-input-vt100-emulation.md`。

### 2.3 模块地图

```
src/
├── main.rs              # 入口; 窗口设置(exit_on_close_request:false 必须为 false)/图标/字体
├── app.rs               # Elm 状态机; 一键启动序列; 滚动/resize 防抖; 关闭确认流程
├── config.rs            # wow_launcher.json 读写(exe 同目录)
├── service.rs           # ServiceKind(MySQL/Auth/World/Client) + Status 枚举
├── process.rs           # ConPTY 订阅流(三线程)+作业对象+kill_tree+wait_mysql_ready
│                        # +grid_size_for_window(网格几何常量)+input::encode_key(键盘编码,14 单测)
├── theme.rs             # 配色常量(Tokyo Night TERM_* + TERM_ANSI 16色表) + 按钮风格
└── ui/
    ├── mod.rs           # 左导航(208px)+内容区; 关闭确认弹窗(stack 覆盖层); 图标静态缓存
    ├── home.rs          # 一键按钮 + 4 服务卡片(状态灯)
    ├── settings.rs      # 路径配置(config_draft 草稿→保存落盘)
    └── terminal.rs      # 3 页签+状态圆点+GridSnapshot 提取/渲染+错误覆盖层
```

### 2.4 关键机制

- **进程树管理**：spawn 后立即挂作业对象（`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`，崩溃/强杀兜底）；主动停止走 `taskkill /PID x /T /F` 树杀（MySQL.bat 是 cmd→mysqld 两级）。订阅 drop 的通道断连只杀直接子进程——所以**一切主动停止必须走 kill_tree**。
- **一键启动**：MySQL `Starting` + Auth/World `Waiting` → `wait_mysql_ready()` 每 250ms 连接 `127.0.0.1:3306`（400ms 超时 ×120 次=30s）→ `MysqlReady` 后 Waiting 服务并发转 Starting。
- **重启**：`restart_pending=true` + 树杀 → `ProcessKilled` → 延时 800ms → `SetDesired(true)`（同时置 `restart_marker=true`）→ 新会话就绪后注入 `--- 已重启 ---` 标记行。期间点停止会清标记使延时任务失效。
- **退出保护**：`exit_on_close_request:false`（否则 CloseRequested 不转发）→ `listen_with` 拦截 → 有运行中服务则弹确认框 → `ConfirmClose(true)` → stop_all + 无 kill 任务时立即检查 `maybe_close_after_stop`（防止 Starting 未回报 pid 时窗口卡死）。
- **滚动回看**：固定 rows×cols 网格快照，**不用 scrollable**；vt100 内部 2000 行 scrollback；offset=0 实时屏；回看时按 scrollback 增量补偿锚定；像素滚轮残量跨事件累积（`wheel_remainder`）。

## 3. 常用命令

```powershell
cargo build                 # 快速类型检查
cargo test                  # 16 项测试: 键盘编码 14 + ConPTY 端到端 + 终端快照回归 ×2
cargo run                   # 调试运行 GUI
cargo build --release       # 产物 target\release\wow_launcher.exe (LTO, 约 2-4 分钟)

# 部署(先结束正在运行的启动器; wow_launcher.json 与 exe 同目录, 保留勿删):
Copy-Item target\release\wow_launcher.exe D:\apps\wow_80_tianlan\azbotcore\ -Force

# 终端输入链路诊断日志(WOW_LAUNCHER_DEBUG=1 时写 exe 同目录 wow_launcher_debug.log):
$env:WOW_LAUNCHER_DEBUG="1"; cargo run

# 重新生成窗口图标:
magick icon.jpg -fuzz 20% -transparent white -trim assets/icon.png
```

Toolchain 必须 `stable-x86_64-pc-windows-msvc`（-gnu 产物缺 MinGW DLL 报 error 126）。

## 4. 设计约束（改动前必读）

### 4.1 终端几何与渲染

1. **CELL_H 必须等于真实行高 `13.0×1.3=16.9`**。iced 0.14 默认 `LineHeight::Relative(1.3)`，`render_row` 必须显式 `.line_height(1.3)` 与 process.rs 的 CELL_H 对齐。曾因按 1.2 倍估算导致底部行溢出被裁——症状就是"光标/输入回显消失"。宁可少算行列，不可溢出。
2. **CHROME_W/CHROME_H 必须与 ui 布局联动**（侧边栏 208 + 内容容器 padding 48 + 页签行 ~35 + 间距 10 + 终端容器 padding 24 + 边框 2 + 余量）。改 `ui/mod.rs`/`terminal.rs` 布局后必须复核这两组常量。
3. **rich_text 必须 `.wrapping(Wrapping::None)`**：默认 Word 换行会让超宽行折行，纵向网格整体错位。
4. **光标只在实时屏（offset==0）渲染**，历史视图无光标（标准终端语义）；光标用 **`█` 实心块 + 该格前景色**，不得依赖 span 背景高亮（空白格背景盒部分后端不渲染）。
5. **错误通知必须是叠加在网格上的覆盖层**，不得占网格行高（否则无错误时白白损失终端行数）。
6. ANSI 黑作前景时映射 `TERM_FG_BLACK`（注释灰），纯黑在 `TERM_BG` 深底上不可见；背景路径仍用真黑。

### 4.2 进程与订阅生命周期

7. **PTY 尺寸绝不进 ServiceRecipe**——recipe 是订阅身份，含尺寸会让每次 resize 触发订阅重建 = 进程重启。
8. **ClearTerminal 注入 `ESC[2J ESC[3J ESC[H` 就地清屏，绝不替换 parser Arc**——替换后读线程仍写旧 Parser，清空一次即永久失显。
9. 一切主动停止/重启/保存配置变更路径都必须走 `kill_tree` 树杀；订阅 drop 只杀直接子进程，.bat 包装场景会留孤儿抢端口。
10. 进程退出后先 drain 再关 HPCON（监控线程等 version 连续 200ms 无变化，上限 1s）——ConPTY 异步渲染的尾部输出（往往是崩溃信息）否则丢失。
11. `push_msg` 重试必须退避且**全程不持锁**，仅在确认断连时瞬时取锁 kill 子进程。
12. resize 链路：任意页签都要应用 resize；`try_send(Resize)` 失败要保留 pending 重试（否则 applied_grid 去重会永久吞掉该尺寸）；resize 成功后写线程发 TerminalData 心跳触发重提取。

### 4.3 构建与环境

13. `.cargo/config.toml` 的 `[http] multiplexing=false` 必须保留（本机网络下 HTTP/2 多路复用会使 cargo 下载卡死）；`crt-static` 保证产物自包含。
14. `main.rs` 窗口设置 `exit_on_close_request: false` 必须保留（见 §2.4 退出保护）。
15. `main.rs` 中 `WGPU_BACKEND=dx12,vulkan` 防 AMD 残缺驱动 GL 探测失败。

### 4.4 代码约定

16. 代码注释中文、文档注释 Google 风格中文；日志消息英文。
17. 新功能优先补测试；bug 修复带回归测试（参考 `terminal.rs` 的滚动语义测试、`process.rs` 的 encode_key 测试组）。
18. 改终端相关代码前先读 `docs/terminal-input-vt100-emulation.md` 与本文档第 2.2/4.1 节。

## 5. 排查速查

| 症状 | 先查 |
|---|---|
| 光标/输入回显不可见 | §4.1-1/2 行高与 CHROME 常量；开 WOW_LAUNCHER_DEBUG 看 [ready]/[key]/[enc]/[pty] |
| 某 tab 无法滚动 | 输出是否不足一屏（scrollback 为空属正常）；vt100 set_size 是否扰动过 scrollback |
| 启动报 error 126 | toolchain 是否 msvc、crt-static 是否生效 |
| 关窗无确认弹窗 | `exit_on_close_request: false` 是否还在 |
| 一键启动按钮灰色 | 三条后端路径是否都已保存 |
| cargo clean os error 32 | 任务管理器结束残留 wow_launcher.exe |
| Auth 终端完全无输出 | 必须经 ConPTY；普通管道会被 CRT 4KB 缓冲吞掉（历史根因） |
