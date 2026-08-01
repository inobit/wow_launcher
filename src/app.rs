use iced::window;
use iced::{Subscription, Task};

use crate::config::{self, Config};
use crate::process;
use crate::service::{ServiceKind, Status};
use crate::ui;

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
    ServiceOutput(ServiceKind, String),
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

    ClearLog(ServiceKind),
}

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
    pub logs: [Vec<String>; 4],
    pub sequence_active: bool,
    pub sequence_message: String,
    pub close_pending: bool,
    pub close_after_stop: bool,
    pub close_window: Option<window::Id>,
}

impl State {
    const MAX_LOG_LINES: usize = 1500;

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
            logs: Default::default(),
            sequence_active: false,
            sequence_message: String::new(),
            close_pending: false,
            close_after_stop: false,
            close_window: None,
        };
        (state, Task::none())
    }

    pub fn log(&self, kind: ServiceKind) -> &Vec<String> {
        &self.logs[kind.index()]
    }

    fn push_log(&mut self, kind: ServiceKind, line: String) {
        let store = &mut self.logs[kind.index()];
        store.push(line);
        if store.len() > Self::MAX_LOG_LINES {
            let drop = store.len() - Self::MAX_LOG_LINES;
            store.drain(0..drop);
        }
    }
}

pub fn update(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::NavSelected(tab) => {
            state.active_nav = tab;
        }
        Message::ServiceSelected(kind) => {
            if matches!(kind, ServiceKind::Mysql | ServiceKind::Auth | ServiceKind::World) {
                state.active_terminal = kind;
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
            state.desired[idx] = true;
            state.status[idx] = Status::Starting;
        }
        Message::SetDesired(_, _) => {}

        Message::ServiceStarted(kind, pid) => {
            let idx = kind.index();
            if !state.desired[idx] {
                // 启动完成后用户已点停止, 进程由 kill_on_drop 处理, 忽略本次事件
                return Task::none();
            }
            state.pids[idx] = Some(pid);
            state.status[idx] = Status::Running;
        }
        Message::ServiceStopped(kind) => {
            let idx = kind.index();
            state.pids[idx] = None;
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
            state.restart_pending[idx] = false;
            state.status[idx] = Status::Error;
            state.push_log(kind, format!("[启动失败] {e}"));
        }
        Message::ServiceOutput(kind, line) => {
            if matches!(kind, ServiceKind::Mysql | ServiceKind::Auth | ServiceKind::World) {
                state.push_log(kind, line);
            }
        }
        Message::ClientLaunched(res) => match res {
            Ok(()) => state.sequence_message = "客户端已启动".into(),
            Err(e) => state.sequence_message = format!("客户端启动失败: {e}"),
        },

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
            state.push_log(mysql, format!("[一键启动] {e}"));
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
                    state.config = state.config_draft.clone();
                    let set = ServiceKind::ALL
                        .iter()
                        .filter(|k| state.config.path_set(**k))
                        .count();
                    state.config_message =
                        Some(format!("已保存配置(已设置 {}/4)", set));
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

        Message::ClearLog(kind) => {
            state.logs[kind.index()].clear();
        }

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
            return stop_all(state);
        }
        Message::ConfirmClose(false) => {
            state.close_pending = false;
            state.close_window = None;
            state.close_after_stop = false;
        }
    }

    Task::none()
}

/// 停止全部后端服务(一键停止 / 关闭确认共用), 返回 kill 任务批
fn stop_all(state: &mut State) -> Task<Message> {
    state.sequence_active = false;
    state.sequence_message = String::new();
    state.restart_pending = [false; 4];
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
    // 拦截窗口关闭请求: 有服务运行时弹出确认框, 由 ConfirmClose 决定是否关闭
    subs.push(iced::event::listen_with(|event, _status, window| {
        match event {
            iced::Event::Window(iced::window::Event::CloseRequested) => {
                Some(Message::CloseRequested(window))
            }
            _ => None,
        }
    }));
    Subscription::batch(subs)
}

pub fn view(state: &State) -> iced::Element<'_, Message> {
    ui::view(state)
}