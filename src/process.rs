use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use futures::channel::mpsc::Sender;
use futures::SinkExt;
use iced::stream;
use iced::{Subscription, Task};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

use crate::app::Message;
use crate::service::ServiceKind;

#[cfg(windows)]
mod job {
    use std::ffi::c_void;
    use std::sync::OnceLock;

    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    /// 裸 HANDLE 指针不满足 Send/Sync, 包一层以便放进 static
    struct SharedHandle(*mut c_void);
    unsafe impl Send for SharedHandle {}
    unsafe impl Sync for SharedHandle {}

    /// 把子进程挂入带 KILL_ON_JOB_CLOSE 的作业对象:
    /// 启动器进程无论崩溃还是被强杀, 关闭作业句柄后该进程树会被系统强制终止,
    /// 避免服务进程变孤儿。尽力而为——进程已被其他作业占用时静默跳过。
    pub fn assign(pid: u32) {
        static JOB: OnceLock<SharedHandle> = OnceLock::new();
        let job = JOB
            .get_or_init(|| unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if !job.is_null() {
                    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                    SetInformationJobObject(
                        job,
                        JobObjectExtendedLimitInformation,
                        &mut info as *mut _ as *const c_void,
                        std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                    );
                }
                SharedHandle(job)
            })
            .0;
        if job.is_null() {
            return;
        }
        unsafe {
            let proc = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if !proc.is_null() {
                AssignProcessToJobObject(job, proc);
                CloseHandle(proc);
            }
        }
    }
}

#[derive(Debug, Clone, Hash)]
pub struct ServiceRecipe {
    pub kind: ServiceKind,
    pub path: String,
}

pub fn service_subscription(kind: ServiceKind, path: String) -> Subscription<Message> {
    Subscription::run_with(
        ServiceRecipe { kind, path },
        build_service_stream,
    )
}

/// 返回可执行文件/脚本所在目录, 作为子进程工作目录, 保证相对路径(配置/数据)解析正确
fn parent_dir(path: &str) -> Option<PathBuf> {
    Path::new(path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
}

fn build_service_stream(recipe: &ServiceRecipe) -> impl futures::Stream<Item = Message> {
    let kind = recipe.kind;
    let path = recipe.path.clone();

    stream::channel(256, async move |mut sender| {
        // ConPTY 伪终端: 子进程 stdout 是真实控制台句柄, 输出逐行实时到达,
        // 不会被 CRT 4KB 块缓冲吞掉(修复 Auth/World 终端无输出的问题)
        let pty_system = native_pty_system();
        let pair = match pty_system.openpty(PtySize {
            rows: 50,
            cols: 160,
            ..Default::default()
        }) {
            Ok(pair) => pair,
            Err(e) => {
                let _ = sender
                    .send(Message::ServiceStartFailed(kind, format!("创建伪终端失败: {e}")))
                    .await;
                return;
            }
        };

        let mut cmd = CommandBuilder::new(&path);
        if let Some(dir) = parent_dir(&path) {
            cmd.cwd(dir);
        }
        let mut child = match pair.slave.spawn_command(cmd) {
            Ok(child) => child,
            Err(e) => {
                let _ = sender
                    .send(Message::ServiceStartFailed(kind, e.to_string()))
                    .await;
                return;
            }
        };
        // 释放 slave, 子进程退出后其句柄关闭, 伪终端即可销毁
        drop(pair.slave);

        let pid = child.process_id().unwrap_or(0);
        // 挂入作业对象: 启动器意外退出时, 系统自动杀死整个进程树, 防止孤儿
        #[cfg(windows)]
        if pid != 0 {
            job::assign(pid);
        }
        let _ = sender.send(Message::ServiceStarted(kind, pid)).await;

        let mut reader = match pair.master.try_clone_reader() {
            Ok(reader) => reader,
            Err(e) => {
                let _ = child.kill();
                let _ = sender
                    .send(Message::ServiceStartFailed(kind, format!("读取伪终端失败: {e}")))
                    .await;
                return;
            }
        };
        // 持有输入端句柄, 避免子进程 stdin 提前 EOF
        let _writer = pair.master.take_writer();

        // 子进程与伪终端句柄在线程间共享: 读线程可 kill, 监控线程负责关闭伪终端
        let child_holder: Arc<Mutex<Option<Box<dyn Child + Send + Sync>>>> =
            Arc::new(Mutex::new(Some(child)));
        let master_holder: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>> =
            Arc::new(Mutex::new(Some(pair.master)));

        // 监控线程: 子进程退出后关闭伪终端(HPCON), 读端随即收到 EOF, 唤醒读线程
        {
            let child_holder = child_holder.clone();
            let master_holder = master_holder.clone();
            std::thread::spawn(move || {
                loop {
                    let exited = child_holder
                        .lock()
                        .unwrap()
                        .as_mut()
                        .map(|c| c.try_wait().ok().flatten().is_some())
                        .unwrap_or(true);
                    if exited {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                *master_holder.lock().unwrap() = None;
            });
        }

        // 读线程: 伪终端是同步 Read, 阻塞读取并转发到 futures 通道
        let mut out_sender = sender.clone();
        let reader_kind = kind;
        let thread = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            let mut pending: Vec<u8> = Vec::new();
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        pending.extend_from_slice(&buf[..n]);
                        while let Some(pos) = pending.iter().position(|&b| b == b'\n') {
                            let raw: Vec<u8> = pending.drain(..=pos).collect();
                            let line = String::from_utf8_lossy(&raw);
                            let trimmed = line.trim_end_matches(['\r', '\n']).to_string();
                            if !trimmed.is_empty() {
                                let msg = Message::ServiceOutput(reader_kind, trimmed);
                                let mut guard = child_holder.lock().unwrap();
                                if !push_msg(&mut out_sender, msg, guard.as_deref_mut().unwrap()) {
                                    return; // UI 端已断开(停止/关闭)
                                }
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            let mut guard = child_holder.lock().unwrap();
            let _ = push_msg(
                &mut out_sender,
                Message::ServiceStopped(reader_kind),
                guard.as_deref_mut().unwrap(),
            );
        });

        // 保持 sender/PTY 存活直到读线程结束, 流才会关闭
        while !thread.is_finished() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
}

/// 向 UI 通道发送消息; 通道满时短暂重试, 通道关闭(UI 断开)时杀掉子进程并返回 false
fn push_msg(
    sender: &mut Sender<Message>,
    msg: Message,
    child: &mut dyn Child,
) -> bool {
    let mut msg = msg;
    loop {
        match sender.try_send(msg) {
            Ok(()) => return true,
            Err(e) if e.is_full() => {
                msg = e.into_inner();
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                // 通道已关闭(UI 断开): 杀掉子进程
                let _ = child.kill();
                let _ = e;
                return false;
            }
        }
    }
}

/// 通过 taskkill 杀死进程树(如 MySQL.bat 包装的 cmd -> mysqld), 而不是只杀直接子进程
pub fn kill_tree(kind: ServiceKind, pid: u32) -> Task<Message> {
    Task::perform(
        async move {
            // CREATE_NO_WINDOW: 防止控制台程序 taskkill 弹出短暂的黑窗口
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .creation_flags(0x08000000)
                .status();
        },
        move |_| Message::ProcessKilled(kind),
    )
}

pub fn wait_mysql_ready() -> Task<Message> {
    Task::perform(
        async {
            let addr: std::net::SocketAddr = "127.0.0.1:3306".parse().unwrap();
            for _ in 0..120 {
                match tokio::time::timeout(
                    Duration::from_millis(400),
                    tokio::net::TcpStream::connect(addr),
                )
                .await
                {
                    Ok(Ok(_stream)) => return Ok(()),
                    _ => {}
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err("MySQL 在 30 秒内未能监听 3306 端口，已放弃等待。".to_string())
        },
        |res| match res {
            Ok(()) => Message::MysqlReady,
            Err(e) => Message::MysqlReadyFailed(e),
        },
    )
}

pub fn delay_restart(kind: ServiceKind) -> Task<Message> {
    Task::perform(
        async {
            tokio::time::sleep(Duration::from_millis(800)).await;
        },
        move |_| Message::SetDesired(kind, true),
    )
}

pub fn launch_client(path: String) -> Task<Message> {
    Task::perform(
        async move {
            let mut cmd = std::process::Command::new(&path);
            // 工作目录切到脚本所在目录, 否则 .bat 内相对路径(wow.exe 等)会解析失败
            if let Some(dir) = parent_dir(&path) {
                cmd.current_dir(dir);
            }
            match cmd
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .creation_flags(0x08000000) // CREATE_NO_WINDOW: 禁止子进程创建控制台窗口
                .spawn()
            {
                Ok(_child) => {
                    std::mem::forget(_child);
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            }
        },
        Message::ClientLaunched,
    )
}

pub fn browse_path(kind: ServiceKind) -> Task<Message> {
    Task::perform(
        async move {
            rfd::AsyncFileDialog::new()
                .add_filter("可执行程序", &["exe", "bat", "cmd"])
                .pick_file()
                .await
                .map(|h| h.path().to_string_lossy().to_string())
        },
        move |picked| Message::PathBrowsed(kind, picked),
    )
}
