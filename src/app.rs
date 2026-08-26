use std::sync::atomic::Ordering;
use std::time::Duration;

use iced::keyboard::key::Named;
use iced::window;
use iced::{keyboard, Subscription, Task};
use portable_pty::PtySize;

use crate::config::{self, Config};
use crate::process::{self, TermCmd, TermHandle};
use crate::service::{ServiceKind, Status};
use crate::ui;
use crate::ui::terminal::{self, GridSnapshot};

/// 主窗口初始尺寸(与 main.rs 窗口设置保持一致), 用于估算 ConPTY 初始行列数
pub const WINDOW_W: f32 = 1100.0;
pub const WINDOW_H: f32 = 720.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavTab {
    Home,
    Settings,
    Terminal,
}

#[derive(Debug, Clone)]
pub enum Message {
    NavSelected(NavTab),
    ServiceSelected(ServiceKind),

    StartService(ServiceKind),
    StopService(ServiceKind),
    RestartService(ServiceKind),
    SetDesired(ServiceKind, bool),

    ServiceStarted(ServiceKind, u32),
    ServiceStopped(ServiceKind),
    ServiceStartFailed(ServiceKind, String),
    ProcessKilled(ServiceKind),

    /// 读线程心跳 + 脏标记: 有新输出进入 vt100 解析器
    TerminalData(ServiceKind),
    /// 服务进程的终端句柄就绪(订阅侧建好通道/解析器后发出)
    ServiceTerminalReady(ServiceKind, TermHandle),

    ClientLaunched(Result<(), String>),

    StartAll,
    StopAll,
    MysqlReady,
    MysqlReadyFailed(String),

    SaveConfig,
    ResetConfigDraft,
    PathEdited(ServiceKind, String),
    BrowsePath(ServiceKind),
    PathBrowsed(ServiceKind, Option<String>),

    CloseRequested(window::Id),
    ConfirmClose(bool),

    /// 清空当前终端(运行中注入清屏序列, 非替换 Arc)
    ClearTerminal(ServiceKind),
    /// 全局转发的键盘事件(update 内按页签/状态过滤)
    TermKeyEvent(keyboard::Event),
    /// 滚轮(行, 可为小数——像素滚动折算后的残量由 update 跨事件累积): 正=向历史回看, 负=向实时屏
    TermScroll(f32),
    /// Shift+PgUp/PgDn 本地翻页: ±rows/2
    TermPageScroll(i32),
    /// 窗口尺寸变化
    WindowResized(iced::Size),
    /// resize 防抖到期
    TermResizeTick,
}

/// 每服务错误通知条数上限
const MAX_ERRORS: usize = 20;

pub struct State {
    pub config: Config,
    pub config_draft: Config,
    pub config_message: Option<String>,
    pub active_nav: NavTab,
    pub active_terminal: ServiceKind,
    pub status: [Status; 4],
    pub desired: [bool; 4],
    pub pids: [Option<u32>; 4],
    pub restart_pending: [bool; 4],
    /// 重启标记: 延时重启重新拉起后, 在新会话顶部注入 "--- 已重启 ---" 标记行
    pub restart_marker: [bool; 4],

    /// 各服务的终端句柄(None = 未启动或已停止)
    pub terminals: [Option<TermHandle>; 4],
    /// 可见网格快照缓存(按 version/offset 失效)
    pub grids: [Option<GridSnapshot>; 4],
    /// 回看偏移: 0=实时屏, >0=距最新内容 N 行
    pub scroll_offset: [usize; 4],
    /// 上次探测到的回滚总行数(锚定用)
    pub prev_scrollback_total: [usize; 4],
    /// 每服务错误通知(渲染在终端网格上方, 上限 MAX_ERRORS 条)
    pub errors: [Vec<String>; 4],
    /// 各服务 PTY 当前生效尺寸(初始为 (0,0) 表示未应用)
    pub applied_grid: [(u16, u16); 4],
    /// 待应用的网格尺寸(resize 防抖窗口内被覆盖)
    pub pending_resize: Option<(u16, u16)>,
    /// 最近一次窗口尺寸(每次窗口事件都更新, 不限页签)
    pub last_window_size: (f32, f32),
    /// 滚轮残量(行): 精确触控板每次增量不足 1 行, 跨事件累积攒满 1 行才滚动
    pub wheel_remainder: f32,

    pub sequence_active: bool,
    pub sequence_message: String,
    pub close_pending: bool,
    pub close_after_stop: bool,
    pub close_window: Option<window::Id>,
}

impl State {
    pub fn new() -> (State, Task<Message>) {
        let config = config::load();
        let state = State {
            config: config.clone(),
            config_draft: config,
            config_message: None,
            active_nav: NavTab::Home,
            active_terminal: ServiceKind::Mysql,
            status: [Status::Stopped; 4],
            desired: [false; 4],
            pids: [None; 4],
            restart_pending: [false; 4],
            restart_marker: [false; 4],
            terminals: [None, None, None, None],
            grids: [None, None, None, None],
            scroll_offset: [0; 4],
            prev_scrollback_total: [0; 4],
            errors: Default::default(),
            applied_grid: [(0, 0); 4],
            pending_resize: None,
            last_window_size: (WINDOW_W, WINDOW_H),
            wheel_remainder: 0.0,
            sequence_active: false,
            sequence_message: String::new(),
            close_pending: false,
            close_after_stop: false,
            close_window: None,
        };
        (state, Task::none())
    }

    /// 记录一条错误通知(超出上限丢最旧), 替代旧 push_log
    fn push_error(&mut self, kind: ServiceKind, msg: String) {
        let store = &mut self.errors[kind.index()];
        store.push(msg);
        if store.len() > MAX_ERRORS {
            let drop = store.len() - MAX_ERRORS;
            store.drain(0..drop);
        }
    }
}

pub fn update(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::NavSelected(tab) => {
            state.active_nav = tab;
            state.wheel_remainder = 0.0;
            if tab == NavTab::Terminal {
                // 进入终端页/切页签时补提取快照(非活动页签平时不提取)
                let kind = state.active_terminal;
                refresh_grid(state, kind);
            }
        }
        Message::ServiceSelected(kind) => {
            if matches!(kind, ServiceKind::Mysql | ServiceKind::Auth | ServiceKind::World) {
                state.active_terminal = kind;
                state.wheel_remainder = 0.0;
                refresh_grid(state, kind);
            }
        }

        Message::StartService(kind) => {
            let path = state.config.path_for(kind).to_string();
            if path.is_empty() {
                state.config_message = Some(format!("{} 路径为空,请先配置。", kind.label()));
                return Task::none();
            }
            match kind {
                ServiceKind::Client => {
                    state.sequence_message = "正在启动客户端…".into();
                    return process::launch_client(path);
                }
                _ => {
                    state.desired[kind.index()] = true;
                    state.status[kind.index()] = Status::Starting;
                    state.config_message = None;
                }
            }
        }
        Message::StopService(kind) => {
            if matches!(kind, ServiceKind::Mysql | ServiceKind::Auth | ServiceKind::World) {
                state.restart_pending[kind.index()] = false;
                state.restart_marker[kind.index()] = false;
                state.desired[kind.index()] = false;
                if state.status[kind.index()] == Status::Waiting {
                    // 一键启动排队中, 直接取消等待
                    state.status[kind.index()] = Status::Stopped;
                } else if let Some(pid) = state.pids[kind.index()] {
                    state.status[kind.index()] = Status::Stopping;
                    return process::kill_tree(kind, pid);
                } else {
                    state.status[kind.index()] = Status::Stopped;
                }
            }
        }
        Message::RestartService(kind) => {
            if matches!(kind, ServiceKind::Mysql | ServiceKind::Auth | ServiceKind::World) {
                state.restart_pending[kind.index()] = true;
                state.desired[kind.index()] = false;
                state.status[kind.index()] = Status::Stopping;
                return match state.pids[kind.index()] {
                    Some(pid) => process::kill_tree(kind, pid),
                    None => process::delay_restart(kind),
                };
            }
        }
        Message::SetDesired(kind, true) => {
            let idx = kind.index();
            // 仅"重启"流程的延时任务会发此消息; 若用户已取消(停止/一键停止), 忽略
            if !state.restart_pending[idx] {
                return Task::none();
            }
            state.restart_pending[idx] = false;
            state.restart_marker[idx] = true;
            state.desired[idx] = true;
            state.status[idx] = Status::Starting;
        }
        Message::SetDesired(_, _) => {}

        Message::ServiceStarted(kind, pid) => {
            let idx = kind.index();
            if !state.desired[idx] {
                // 启动完成后用户已点停止: 忽略本次事件; 订阅已被 drop,
                // 读线程 push_msg 断连检测会杀掉子进程
                return Task::none();
            }
            state.pids[idx] = Some(pid);
            state.status[idx] = Status::Running;
        }
        Message::ServiceStopped(kind) => {
            let idx = kind.index();
            state.pids[idx] = None;
            // 定格最后一屏: 回看偏移归零并做最终提取(之后句柄失效, 不可再滚动)
            state.scroll_offset[idx] = 0;
            refresh_grid(state, kind);
            // 输入立即失效; 网格快照保留供停服后查看最后一屏
            state.terminals[idx] = None;
            if state.restart_pending[idx] {
                return process::delay_restart(kind);
            }
            state.desired[idx] = false;
            state.status[idx] = Status::Stopped;
            return maybe_close_after_stop(state);
        }
        Message::ProcessKilled(kind) => {
            let idx = kind.index();
            if state.desired[idx] {
                // 杀进程期间用户又手动启动了新进程, 忽略旧进程的结束事件
                return Task::none();
            }
            state.pids[idx] = None;
            state.scroll_offset[idx] = 0;
            refresh_grid(state, kind);
            state.terminals[idx] = None;
            if state.restart_pending[idx] {
                return process::delay_restart(kind);
            }
            state.status[idx] = Status::Stopped;
            return maybe_close_after_stop(state);
        }
        Message::ServiceStartFailed(kind, e) => {
            let idx = kind.index();
            state.desired[idx] = false;
            state.pids[idx] = None;
            state.terminals[idx] = None;
            state.restart_pending[idx] = false;
            state.restart_marker[idx] = false;
            state.status[idx] = Status::Error;
            state.push_error(kind, format!("[启动失败] {e}"));
        }
        Message::ClientLaunched(res) => match res {
            Ok(()) => state.sequence_message = "客户端已启动".into(),
            Err(e) => state.sequence_message = format!("客户端启动失败: {e}"),
        },

        Message::ServiceTerminalReady(kind, h) => {
            let idx = kind.index();
            if !state.desired[idx] {
                // 句柄送达前用户已停止, 忽略(订阅即将被 drop 清理)
                return Task::none();
            }
            let (size, cur, hidden) = {
                let p = h.parser.lock().unwrap();
                (p.screen().size(), p.screen().cursor_position(), p.screen().hide_cursor())
            };
            process::dbg_log(&format!(
                "[ready] {kind:?} grid={size:?} cursor={cur:?} hidden={hidden} restart={}",
                state.restart_marker[idx]
            ));
            // 重启场景: 新会话顶部插入黄色标记行(普通 str 字面量, 字节串不支持非 ASCII)。
            // 注意必须用 restart_marker 判定——正常重启流程中旧句柄已被 ProcessKilled
            // 清空, 若用 terminals[idx].is_some() 判定, 标记永远不会注入
            if state.restart_marker[idx] {
                state.restart_marker[idx] = false;
                let v = h.version.clone();
                let mut p = h.parser.lock().unwrap();
                p.process("\x1b[33m--- 已重启 ---\x1b[0m\r\n".as_bytes());
                drop(p);
                v.fetch_add(1, Ordering::Release);
            }
            state.applied_grid[idx] = size;
            state.terminals[idx] = Some(h);
            state.grids[idx] = None; // 强制重提取
            state.scroll_offset[idx] = 0;
            state.prev_scrollback_total[idx] = 0;
            state.errors[idx].clear();
        }
        Message::TerminalData(kind) => {
            // 仅活动页签提取快照; 其余页签在切换时补提取
            if state.active_nav != NavTab::Terminal || state.active_terminal != kind {
                return Task::none();
            }
            refresh_grid(state, kind);
        }

        Message::TermKeyEvent(e) => {
            // 守卫链: 弹窗中 / 非终端页 / 服务未运行或无句柄 → 忽略
            {
                let idx0 = state.active_terminal.index();
                process::dbg_log(&format!(
                    "[key] nav={:?} term={:?} status={:?} handle={} close={}",
                    state.active_nav,
                    state.active_terminal,
                    state.status[idx0],
                    state.terminals[idx0].is_some(),
                    state.close_pending
                ));
            }
            if state.close_pending || state.active_nav != NavTab::Terminal {
                return Task::none();
            }
            let kind = state.active_terminal;
            let idx = kind.index();
            if state.status[idx] != Status::Running || state.terminals[idx].is_none() {
                return Task::none();
            }
            let keyboard::Event::KeyPressed { key, text, modifiers, .. } = e else {
                return Task::none();
            };

            // Shift+PgUp/PgDn: 本地翻页滚动, 不发给远端程序
            if modifiers.shift() {
                let dir: i32 = match key {
                    keyboard::Key::Named(Named::PageUp) => 1,
                    keyboard::Key::Named(Named::PageDown) => -1,
                    _ => 0,
                };
                if dir != 0 {
                    let Some(handle) = state.terminals[idx].clone() else {
                        return Task::none();
                    };
                    let (rows, _) = { handle.parser.lock().unwrap().screen().size() };
                    let n = ((rows as i32 / 2).max(1)) * dir;
                    return Task::done(Message::TermPageScroll(n));
                }
            }

            let Some(handle) = state.terminals[idx].clone() else {
                return Task::none();
            };
            let app_cursor = { handle.parser.lock().unwrap().screen().application_cursor() };
            let encoded = process::input::encode_key(&key, text.as_deref(), modifiers, app_cursor);
            process::dbg_log(&format!(
                "[enc] {key:?} text={text:?} -> {}",
                encoded.as_deref().map(|b| format!("{b:?}")).unwrap_or_else(|| "None".into())
            ));
            if let Some(bytes) = encoded {
                // 发送输入后回到实时屏(标准终端行为); 完成消息复用 TerminalData 触发刷新
                state.scroll_offset[idx] = 0;
                return Task::perform(
                    async move { let _ = handle.cmds.send(TermCmd::Input(bytes)); },
                    move |_| Message::TerminalData(kind),
                );
            }
            return Task::none();
        }
        Message::TermScroll(delta) => {
            process::dbg_log(&format!("[wheel] delta={delta}"));
            if state.close_pending || state.active_nav != NavTab::Terminal {
                state.wheel_remainder = 0.0;
                return Task::none();
            }
            // 精确触控板每次增量不足 1 行: 跨事件累积残量, 攒满 1 行才滚动
            let total = state.wheel_remainder + delta;
            let n = total.trunc() as i32;
            state.wheel_remainder = total - n as f32;
            if n == 0 {
                return Task::none();
            }
            return apply_scroll(state, state.active_terminal, i64::from(n));
        }
        Message::TermPageScroll(n) => {
            if state.close_pending || state.active_nav != NavTab::Terminal || n == 0 {
                return Task::none();
            }
            return apply_scroll(state, state.active_terminal, n as i64);
        }
        Message::WindowResized(size) => {
            state.last_window_size = (size.width, size.height);
            // 任何页签下都应用 resize: 否则在终端页外缩放窗口后 PTY 尺寸滞留旧值
            let (rows, cols) = process::grid_size_for_window(size.width, size.height);
            // 每次都覆盖 pending 并重发防抖任务; 旧任务到期发现 pending 已取走则无害
            state.pending_resize = Some((rows, cols));
            return Task::perform(
                tokio::time::sleep(Duration::from_millis(200)),
                |_| Message::TermResizeTick,
            );
        }
        Message::TermResizeTick => {
            let Some((rows, cols)) = state.pending_resize.take() else {
                return Task::none();
            };
            let mut all_sent = true;
            for kind in ServiceKind::SERVICES {
                let idx = kind.index();
                if state.applied_grid[idx] == (rows, cols) {
                    continue;
                }
                if let Some(h) = &state.terminals[idx] {
                    // 仅发送成功才记录 applied_grid; 通道满时保留 pending 下个 tick 重试,
                    // 否则该尺寸会被 applied_grid 的去重判断永久吞掉
                    match h.cmds.try_send(TermCmd::Resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    })) {
                        Ok(()) => state.applied_grid[idx] = (rows, cols),
                        Err(_) => all_sent = false,
                    }
                } else {
                    state.applied_grid[idx] = (rows, cols);
                }
            }
            if !all_sent {
                state.pending_resize = Some((rows, cols));
                return Task::perform(
                    tokio::time::sleep(Duration::from_millis(200)),
                    |_| Message::TermResizeTick,
                );
            }
        }

        Message::ClearTerminal(kind) => {
            let idx = kind.index();
            match &state.terminals[idx] {
                Some(h) => {
                    // 服务仍在运行: 原地清屏 + 清回滚 + 光标归位, 绝不替换 Arc
                    // (UI 替换 Arc 后读线程仍写旧 Parser, 清空一次即永久失显)
                    let v = h.version.clone();
                    let mut p = h.parser.lock().unwrap();
                    p.process(b"\x1b[2J\x1b[3J\x1b[H");
                    drop(p);
                    v.fetch_add(1, Ordering::Release);
                }
                None => {} // 已停止: 无可清
            }
            state.grids[idx] = None;
            state.scroll_offset[idx] = 0;
            state.prev_scrollback_total[idx] = 0;
        }

        Message::StartAll => {
            let missing: Vec<ServiceKind> = ServiceKind::SERVICES
                .iter()
                .copied()
                .filter(|k| !state.config.path_set(*k))
                .collect();
            if !missing.is_empty() {
                state.sequence_active = false;
                state.sequence_message =
                    format!("一键启动需要先配置 {}", missing[0].label());
                return Task::none();
            }
            let mysql = ServiceKind::Mysql;
            // 已在运行/启动中的 MySQL 保持现状, 避免状态被覆盖
            if state.status[mysql.index()] != Status::Running && !state.desired[mysql.index()] {
                state.desired[mysql.index()] = true;
                state.status[mysql.index()] = Status::Starting;
            }
            state.sequence_active = true;
            state.sequence_message = "正在启动 MySQL…".into();
            // Auth/World 先进入"等待 MySQL"状态, MySQL 就绪后由内部继续启动
            for kind in [ServiceKind::Auth, ServiceKind::World] {
                if matches!(state.status[kind.index()], Status::Stopped | Status::Error) {
                    state.status[kind.index()] = Status::Waiting;
                }
            }
            return process::wait_mysql_ready();
        }
        Message::StopAll => {
            return stop_all(state);
        }
        Message::MysqlReady => {
            if !state.sequence_active {
                return Task::none();
            }
            let mysql = ServiceKind::Mysql;
            if state.status[mysql.index()] == Status::Running
                || state.desired[mysql.index()]
            {
                for kind in [ServiceKind::Auth, ServiceKind::World] {
                    if state.status[kind.index()] == Status::Waiting {
                        state.desired[kind.index()] = true;
                        state.status[kind.index()] = Status::Starting;
                    }
                }
                state.sequence_active = false;
                state.sequence_message = "MySQL 就绪,已启动 Auth/World".into();
            } else {
                state.sequence_active = false;
                state.sequence_message = "MySQL 未在运行,已中止一键启动".into();
            }
        }
        Message::MysqlReadyFailed(e) => {
            let mysql = ServiceKind::Mysql;
            state.desired[mysql.index()] = false;
            state.pids[mysql.index()] = None;
            state.status[mysql.index()] = Status::Error;
            state.sequence_active = false;
            state.sequence_message = e.clone();
            state.push_error(mysql, format!("[一键启动] {e}"));
            for kind in [ServiceKind::Auth, ServiceKind::World] {
                if state.status[kind.index()] == Status::Waiting {
                    state.status[kind.index()] = Status::Stopped;
                }
            }
        }

        Message::SaveConfig => {
            state.config_message = None;
            match config::save(&state.config_draft) {
                Ok(()) => {
                    // 路径变更且服务仍在运行/排队: 先树杀停止再应用新配置。
                    // 否则订阅按新路径重建时只会 kill 直接子进程,
                    // .bat 包装的服务(cmd→mysqld)会残留孤儿进程抢占端口
                    let mut stopped: Vec<&str> = Vec::new();
                    let mut kill_tasks = Vec::new();
                    for kind in ServiceKind::SERVICES {
                        let idx = kind.index();
                        let path_changed =
                            state.config.path_for(kind) != state.config_draft.path_for(kind);
                        let active = state.desired[idx]
                            || state.pids[idx].is_some()
                            || matches!(
                                state.status[idx],
                                Status::Starting
                                    | Status::Running
                                    | Status::Stopping
                                    | Status::Waiting
                            );
                        if path_changed && active {
                            state.restart_pending[idx] = false;
                            state.restart_marker[idx] = false;
                            state.desired[idx] = false;
                            match state.pids[idx] {
                                Some(pid) => {
                                    state.status[idx] = Status::Stopping;
                                    kill_tasks.push(process::kill_tree(kind, pid));
                                }
                                None => state.status[idx] = Status::Stopped,
                            }
                            stopped.push(kind.label());
                        }
                    }
                    state.config = state.config_draft.clone();
                    let set = ServiceKind::ALL
                        .iter()
                        .filter(|k| state.config.path_set(**k))
                        .count();
                    let mut msg = format!("已保存配置(已设置 {set}/4)");
                    if !stopped.is_empty() {
                        msg.push_str(&format!(
                            "; {} 路径已变更, 已先停止旧进程, 请重新启动",
                            stopped.join("/")
                        ));
                    }
                    state.config_message = Some(msg);
                    return Task::batch(kill_tasks);
                }
                Err(e) => state.config_message = Some(e),
            }
        }
        Message::ResetConfigDraft => {
            state.config_draft = state.config.clone();
            state.config_message = Some("已还原未保存的修改".into());
        }
        Message::PathEdited(kind, s) => {
            state.config_draft.set_path(kind, s);
            state.config_message = None;
        }
        Message::BrowsePath(kind) => {
            return process::browse_path(kind);
        }
        Message::PathBrowsed(kind, Some(p)) => {
            state.config_draft.set_path(kind, p);
            state.config_message = None;
        }
        Message::PathBrowsed(_, None) => {}

        Message::CloseRequested(id) => {
            state.close_window = Some(id);
            let any_active = ServiceKind::SERVICES.iter().any(|k| {
                let idx = k.index();
                matches!(
                    state.status[idx],
                    Status::Starting | Status::Running | Status::Stopping
                ) || state.pids[idx].is_some()
            });
            if any_active {
                // 有服务在运行: 弹确认框, 不直接关闭
                state.close_pending = true;
            } else {
                return window::close(id);
            }
        }
        Message::ConfirmClose(true) => {
            state.close_pending = false;
            state.close_after_stop = true;
            let stop = stop_all(state);
            // 无进程可杀时(如服务处于 Starting 尚未回报 pid)不会有 ProcessKilled
            // 回调, 立即检查一次关闭条件, 否则窗口永远等不到触发点
            let close = maybe_close_after_stop(state);
            return Task::batch([stop, close]);
        }
        Message::ConfirmClose(false) => {
            state.close_pending = false;
            state.close_window = None;
            state.close_after_stop = false;
        }
    }

    Task::none()
}

// ===== 终端网格快照 =====

/// 探测某服务当前可用的回滚总行数(set_scrollback clamp 技巧)
fn scrollback_total(handle: &TermHandle) -> usize {
    let mut p = handle.parser.lock().unwrap();
    p.screen_mut().set_scrollback(usize::MAX);
    p.screen().scrollback()
}

/// 提取活动页签的可见网格快照; 版本号与查看位置均未变化时跳过。
/// 回看期间新行追加会把历史推远(vt100 偏移语义是"距最新内容的距离"),
/// 用前后两次回滚总数之差补偿 offset, 保持画面视觉锚定。
fn refresh_grid(state: &mut State, kind: ServiceKind) {
    let idx = kind.index();
    let Some(handle) = state.terminals[idx].clone() else {
        return;
    };

    let total = scrollback_total(&handle);
    if state.scroll_offset[idx] > 0 {
        let delta = total.saturating_sub(state.prev_scrollback_total[idx]);
        state.scroll_offset[idx] = (state.scroll_offset[idx] + delta).min(total);
    }
    state.prev_scrollback_total[idx] = total;

    let version = handle.version.load(Ordering::Acquire);
    if let Some(g) = &state.grids[idx] {
        if g.version == version && g.offset == state.scroll_offset[idx] {
            return;
        }
    }
    let snap = terminal::snapshot_grid(&handle, state.scroll_offset[idx]);
    state.scroll_offset[idx] = snap.offset; // 写回 clamp 后的值
    state.grids[idx] = Some(snap);
}

/// 调整回看偏移并刷新快照(delta>0 向历史, <0 向实时)
fn apply_scroll(state: &mut State, kind: ServiceKind, delta: i64) -> Task<Message> {
    let idx = kind.index();
    let Some(handle) = state.terminals[idx].clone() else {
        return Task::none(); // 已停止: 无句柄不可再提取
    };
    let total = scrollback_total(&handle);
    if delta >= 0 {
        state.scroll_offset[idx] = (state.scroll_offset[idx] + delta as usize).min(total);
    } else {
        state.scroll_offset[idx] = state.scroll_offset[idx].saturating_sub((-delta) as usize);
    }
    refresh_grid(state, kind);
    Task::none()
}

/// 停止全部后端服务(一键停止 / 关闭确认共用), 返回 kill 任务批
fn stop_all(state: &mut State) -> Task<Message> {
    state.sequence_active = false;
    state.sequence_message = String::new();
    state.restart_pending = [false; 4];
    state.restart_marker = [false; 4];
    let mut tasks = Vec::new();
    for kind in ServiceKind::SERVICES {
        let idx = kind.index();
        state.desired[idx] = false;
        match state.status[idx] {
            Status::Waiting | Status::Stopped | Status::Error => {
                state.status[idx] = Status::Stopped;
            }
            _ => match state.pids[idx] {
                Some(pid) => {
                    state.status[idx] = Status::Stopping;
                    tasks.push(process::kill_tree(kind, pid));
                }
                None => state.status[idx] = Status::Stopped,
            },
        }
    }
    Task::batch(tasks)
}

/// 服务全部停止后, 若正等待关闭则关闭窗口
fn maybe_close_after_stop(state: &mut State) -> Task<Message> {
    if state.close_after_stop && state.pids.iter().all(|p| p.is_none()) {
        state.close_after_stop = false;
        let id = state.close_window.take().unwrap_or_else(window::Id::unique);
        return window::close(id);
    }
    Task::none()
}

pub fn subscription(state: &State) -> Subscription<Message> {
    let mut subs = Vec::new();
    for kind in ServiceKind::SERVICES {
        let idx = kind.index();
        if state.desired[idx] && state.config.path_set(kind) {
            subs.push(process::service_subscription(
                kind,
                state.config.path_for(kind).to_string(),
            ));
        }
    }
    // 全局事件转发(fn 指针, 无状态): 只转发未被 widget 捕获的事件,
    // 键盘/滚轮由 update 按 页签+状态 过滤, 避免抢配置页输入框的键
    subs.push(iced::event::listen_with(|event, status, window| {
        if matches!(status, iced::event::Status::Captured) {
            return None;
        }
        match event {
            iced::Event::Window(iced::window::Event::CloseRequested) => {
                Some(Message::CloseRequested(window))
            }
            iced::Event::Window(iced::window::Event::Resized(size)) => {
                Some(Message::WindowResized(size))
            }
            iced::Event::Keyboard(ke @ keyboard::Event::KeyPressed { .. }) => {
                if let keyboard::Event::KeyPressed { key, modifiers, .. } = &ke {
                    process::dbg_log(&format!("[fwd] key={key:?} mods={modifiers:?}"));
                }
                Some(Message::TermKeyEvent(ke))
            }
            iced::Event::Mouse(iced::mouse::Event::WheelScrolled { delta }) => {
                // Lines.y 正值=向上滚(翻历史), 每格折 3 行; Pixels 按 20px 折 1 行。
                // 像素增量通常不足 1 行, 保留小数交给 update 跨事件累积
                let lines = match delta {
                    iced::mouse::ScrollDelta::Lines { y, .. } => y * 3.0,
                    iced::mouse::ScrollDelta::Pixels { y, .. } => y / 20.0,
                };
                (lines != 0.0).then_some(Message::TermScroll(lines))
            }
            _ => None,
        }
    }));
    Subscription::batch(subs)
}

pub fn view(state: &State) -> iced::Element<'_, Message> {
    ui::view(state)
}
