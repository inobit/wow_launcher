use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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

/// 诊断日志(WOW_LAUNCHER_DEBUG=1 时启用): 追加写入 exe 同目录 wow_launcher_debug.log。
/// 用于排查终端输入链路问题; 默认关闭, 不产生任何 IO。
pub fn dbg_log(msg: &str) {
    use std::io::Write;

    if std::env::var("WOW_LAUNCHER_DEBUG").as_deref() != Ok("1") {
        return;
    }
    let Some(path) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("wow_launcher_debug.log")))
    else {
        return;
    };
    // 首次写入时记录会话头(含 exe 路径与修改时间, 用于识别是否运行了旧构建)
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
            let exe = std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "?".into());
            let mtime = std::env::current_exe()
                .ok()
                .and_then(|p| fs::metadata(p).ok())
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| format!("unix={}", d.as_secs()))
                .unwrap_or_else(|| "?".into());
            let _ = writeln!(f, "=== session start, exe={exe} mtime={mtime} ===");
        }
    });
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{msg}");
    }
}

pub fn service_subscription(kind: ServiceKind, path: String) -> Subscription<Message> {
    Subscription::run_with(
        ServiceRecipe { kind, path },
        build_service_stream,
    )
}

/// 终端控制命令(UI → 写线程)
#[derive(Debug)]
pub enum TermCmd {
    /// 键盘编码后的输入字节, 原样写入 PTY
    Input(Vec<u8>),
    /// 调整 PTY 与解析器尺寸
    Resize(PtySize),
}

/// UI 持有的终端句柄。整体手写 Debug(不对 vt100::Parser 的 Debug 实现作假设)
#[derive(Clone)]
pub struct TermHandle {
    /// 有界通道容量 TERM_CMD_CHANNEL_CAP, 满时 send 阻塞(天然背压)
    pub cmds: std::sync::mpsc::SyncSender<TermCmd>,
    /// 屏幕状态, 读线程写 / UI 读, 共用一把锁
    pub parser: Arc<Mutex<vt100::Parser>>,
    /// 数据版本号: 读线程每次 process 后 +1, Resize 时也 +1; UI 据此刷新快照缓存
    pub version: Arc<AtomicU64>,
}

impl std::fmt::Debug for TermHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TermHandle").finish_non_exhaustive()
    }
}

/// 回滚缓冲行数(vt100 Parser 内部 scrollback)
const SCROLLBACK_LINES: usize = 2000;
/// 终端命令通道容量(满时 UI 端 send/try_send 阻塞或丢弃)
const TERM_CMD_CHANNEL_CAP: usize = 512;

// ===== 网格尺寸估算 =====

/// 网格字体常量: MONOSPACE 13px 下 Consolas/Cascadia 的近似度量。
/// 行高必须用 iced 0.14 真实的默认行高(LineHeight::Relative(1.3)):
/// 13px × 1.3 = 16.9px; terminal.rs 渲染侧必须显式 .line_height(1.3) 保持一致,
/// 否则每行实际更高, 底部(光标/回显行)会被容器裁掉
const CELL_W: f32 = 7.3; // 字符 advance(偏保守, 宁可少算列不可右缘溢出)
const CELL_H: f32 = 13.0 * 1.3; // = 16.9, 与 rich_text 显式 line_height(1.3) 对齐

// 横向: 侧边栏 208 + 内容容器左右 padding 48(ui/mod.rs) + 终端容器左右 padding 24
//      + 终端容器边框 2 + 安全余量 8(字符宽度为近似值, 宁可少算不可溢出——
//      溢出会触发 rich_text 换行或右缘裁剪, 纵向网格随之错位)
const CHROME_W: f32 = 208.0 + 48.0 + 24.0 + 2.0 + 8.0;
// 纵向: 内容容器上下 padding 48(ui/mod.rs) + 页签行约 35(13px 行高 16.9 + 按钮 padding 16 + 边框 2)
//      + 页签下间距 10 + 终端容器上下 padding 24 + 边框 2 + 安全余量 5
//      (错误通知为叠加在网格上的覆盖层, 不占用网格行高)
const CHROME_H: f32 = 48.0 + 35.0 + 10.0 + 24.0 + 2.0 + 5.0;

/// 由窗口尺寸估算行列数(近似值即可, ±1~2 列对 ConPTY 无实质影响)
pub fn grid_size_for_window(w: f32, h: f32) -> (u16, u16) {
    let cols = ((w - CHROME_W) / CELL_W).floor().clamp(40.0, 240.0) as u16;
    let rows = ((h - CHROME_H) / CELL_H).floor().clamp(10.0, 60.0) as u16;
    (rows, cols)
}

/// 返回可执行文件/脚本所在目录, 作为子进程工作目录, 保证相对路径(配置/数据)解析正确
fn parent_dir(path: &str) -> Option<PathBuf> {
    Path::new(path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
}

// `+ use<>`: 流类型完全自持有(edition 2024 RPIT 默认捕获入参生命周期,
// 会破坏 run_with 要求的 for<'a> fn 指针形态)
fn build_service_stream(recipe: &ServiceRecipe) -> impl futures::Stream<Item = Message> + use<> {
    let kind = recipe.kind;
    let path = recipe.path.clone();

    stream::channel(256, async move |mut sender| {
        // 初始行列按主窗口尺寸估算(recipe 是订阅身份, 尺寸不能进 recipe,
        // 否则 resize 会触发订阅重建 → 进程重启)
        let (rows, cols) = grid_size_for_window(crate::app::WINDOW_W, crate::app::WINDOW_H);

        // ConPTY 伪终端: 子进程 stdout 是真实控制台句柄, 输出逐字节实时到达,
        // 不会被 CRT 4KB 块缓冲吞掉(修复 Auth/World 终端无输出的问题)
        let pty_system = native_pty_system();
        let pair = match pty_system.openpty(PtySize {
            rows,
            cols,
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
        // 输入端句柄交给写线程, 用于向 PTY 写键盘编码字节
        let mut writer = match pair.master.take_writer() {
            Ok(writer) => writer,
            Err(e) => {
                let _ = child.kill();
                let _ = sender
                    .send(Message::ServiceStartFailed(kind, format!("写入伪终端失败: {e}")))
                    .await;
                return;
            }
        };

        // 共享屏幕状态: 读线程喂原始字节, UI 线程按版本号提取快照
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, SCROLLBACK_LINES)));
        let version = Arc::new(AtomicU64::new(0));
        let (tx_cmd, rx_cmd) = std::sync::mpsc::sync_channel::<TermCmd>(TERM_CMD_CHANNEL_CAP);

        // 子进程与伪终端句柄在线程间共享: 读线程可 kill, 监控线程负责关闭伪终端
        let child_holder: Arc<Mutex<Option<Box<dyn Child + Send + Sync>>>> =
            Arc::new(Mutex::new(Some(child)));
        let master_holder: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>> =
            Arc::new(Mutex::new(Some(pair.master)));

        // 监控线程: 子进程退出后关闭伪终端(HPCON), 读端随即收到 EOF, 唤醒读线程
        {
            let child_holder = child_holder.clone();
            let master_holder = master_holder.clone();
            let version = version.clone();
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
                // ConPTY 把子进程输出异步渲染为 VT 序列, 退出瞬间管道里可能还有
                // 最后一小段(往往是崩溃信息)未读走: 等读线程抽干(版本号连续
                // 200ms 无变化即视为抽干, 上限 1s)再关 HPCON, 避免尾部输出丢失
                let mut last = version.load(Ordering::Relaxed);
                let mut stable = 0;
                for _ in 0..10 {
                    std::thread::sleep(Duration::from_millis(100));
                    let v = version.load(Ordering::Relaxed);
                    if v == last {
                        stable += 1;
                        if stable >= 2 {
                            break;
                        }
                    } else {
                        stable = 0;
                        last = v;
                    }
                }
                *master_holder.lock().unwrap() = None;
            });
        }

        // 写线程: 处理 UI 的输入/resize 命令; 通道关闭(订阅结束)或写失败则退出。
        // 锁序约定: master_holder 与 parser 只单独短暂持有, 不嵌套
        {
            let parser = parser.clone();
            let version = version.clone();
            let master_holder = master_holder.clone();
            let child_holder = child_holder.clone();
            let mut resize_sender = sender.clone();
            std::thread::spawn(move || loop {
                match rx_cmd.recv() {
                    Ok(TermCmd::Input(bytes)) => {
                        let ok = writer.write_all(&bytes).and_then(|_| writer.flush()).is_ok();
                        dbg_log(&format!("[pty] write {} bytes ok={ok}", bytes.len()));
                        if !ok {
                            break;
                        }
                    }
                    Ok(TermCmd::Resize(size)) => {
                        if let Some(m) = master_holder.lock().unwrap().as_ref() {
                            let _ = m.resize(size);
                        }
                        parser.lock().unwrap().screen_mut().set_size(size.rows, size.cols);
                        version.fetch_add(1, Ordering::Release);
                        // resize 本身不产生输出, 主动发心跳让 UI 重提取快照;
                        // 断连时 push_msg 会杀掉子进程, 写线程随即退出
                        if !push_msg(&mut resize_sender, Message::TerminalData(kind), &child_holder) {
                            break;
                        }
                    }
                    Err(_) => break, // 所有 SyncSender 已 drop(订阅结束)
                }
            });
        }

        // 终端句柄发给 UI(先建好全部句柄再发, 保证完整); 断连则退出
        {
            let handle = TermHandle {
                cmds: tx_cmd,
                parser: parser.clone(),
                version: version.clone(),
            };
            if !push_msg(&mut sender, Message::ServiceTerminalReady(kind, handle), &child_holder) {
                return;
            }
        }

        // 读线程: 原始字节块直接喂 vt100 解析器(ANSI 序列由仿真层消化),
        // 每块发一条 TerminalData 心跳——仍走 push_msg 保住断连检测(通道关 → child.kill())
        let mut out_sender = sender.clone();
        let reader_kind = kind;
        let thread_parser = parser.clone();
        let thread_version = version.clone();
        let thread = std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        thread_parser.lock().unwrap().process(&buf[..n]);
                        thread_version.fetch_add(1, Ordering::Release);
                        let msg = Message::TerminalData(reader_kind);
                        if !push_msg(&mut out_sender, msg, &child_holder) {
                            return; // UI 端已断开(停止/关闭)
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = push_msg(&mut out_sender, Message::ServiceStopped(reader_kind), &child_holder);
        });

        // 保持 sender/PTY 存活直到读线程结束, 流才会关闭
        while !thread.is_finished() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
}

/// 向 UI 通道发送消息; 通道满时退避重试, 通道关闭(UI 断开)时杀掉子进程并返回 false。
/// 退避期间不持有任何锁(旧实现持 child_holder 锁自旋, UI 卡顿时会连带阻塞监控线程)
fn push_msg(
    sender: &mut Sender<Message>,
    msg: Message,
    child_holder: &Arc<Mutex<Option<Box<dyn Child + Send + Sync>>>>,
) -> bool {
    let mut msg = msg;
    let mut backoff = Duration::from_millis(10);
    loop {
        match sender.try_send(msg) {
            Ok(()) => return true,
            Err(e) if e.is_full() => {
                msg = e.into_inner();
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_millis(50));
            }
            Err(_) => {
                // 通道已关闭(UI 断开): 杀掉子进程(瞬时取锁, 不阻塞其他线程)
                if let Some(child) = child_holder.lock().unwrap().as_deref_mut() {
                    let _ = child.kill();
                }
                return false;
            }
        }
    }
}

// ===== 键盘编码 =====

pub mod input {
    use iced::keyboard::{key::Named, Key, Modifiers};

    /// 把一次按键编码为发往 PTY 的字节序列。
    ///
    /// # 参数
    /// - `app_cursor`: 远端是否处于 DECCKM 应用光标模式(方向键用 SS3 还是 CSI)
    /// - `text`: 平台输入法路径给出的可打印文本(winit Windows 下来自 WM_CHAR)
    ///
    /// 返回 `None` 表示该按键无需发送(如纯修饰键、Unidentified 等)。
    pub fn encode_key(
        key: &Key,
        text: Option<&str>,
        mods: Modifiers,
        app_cursor: bool,
    ) -> Option<Vec<u8>> {
        // Shift+PgUp/PgDn 由上层截走做本地翻页滚动, 此处防御性兜底不进入编码
        if mods.shift() && matches!(key, Key::Named(Named::PageUp) | Key::Named(Named::PageDown)) {
            return None;
        }

        // 规则 1: Ctrl 组合键 → C0 控制字符(字母 & 0x1f 及特殊符号映射)
        if mods.control() {
            match key {
                Key::Named(Named::Space) => return Some(vec![0x00]),
                Key::Character(s) => {
                    let mut chars = s.chars();
                    if let (Some(c), None) = (chars.next(), chars.next()) {
                        let l = c.to_ascii_lowercase();
                        let ctrl: u8 = match l {
                            'a'..='z' => (l as u8) & 0x1f,
                            '@' => 0x00,
                            '[' => 0x1b,
                            '\\' => 0x1c,
                            ']' => 0x1d,
                            '^' => 0x1e,
                            '_' | '/' => 0x1f,
                            // Ctrl+其他可打印字符: 交给文本透传路径
                            _ => return encode_text(key, text, mods),
                        };
                        return Some(vec![ctrl]);
                    }
                }
                _ => {}
            }
        }

        // 规则 2/3: 命名键表 + 修饰键参数化 CSI
        if let Key::Named(n) = key {
            let m = modifier_param(mods);
            match n {
                Named::Enter => return Some(b"\r".to_vec()),
                Named::Backspace => return Some(b"\x7f".to_vec()),
                Named::Tab => return Some(b"\t".to_vec()),
                Named::Escape => return Some(b"\x1b".to_vec()),
                Named::ArrowUp => return Some(csi_or_ss3('A', app_cursor, m)),
                Named::ArrowDown => return Some(csi_or_ss3('B', app_cursor, m)),
                Named::ArrowRight => return Some(csi_or_ss3('C', app_cursor, m)),
                Named::ArrowLeft => return Some(csi_or_ss3('D', app_cursor, m)),
                Named::Home => return Some(csi_or_ss3('H', app_cursor, m)),
                Named::End => return Some(csi_or_ss3('F', app_cursor, m)),
                Named::Insert => return Some(tilde(2, m)),
                Named::Delete => return Some(tilde(3, m)),
                Named::PageUp => return Some(tilde(5, m)),
                Named::PageDown => return Some(tilde(6, m)),
                Named::F1 => return Some(fn_key('P', m)),
                Named::F2 => return Some(fn_key('Q', m)),
                Named::F3 => return Some(fn_key('R', m)),
                Named::F4 => return Some(fn_key('S', m)),
                Named::F5 => return Some(tilde(15, m)),
                Named::F6 => return Some(tilde(17, m)),
                Named::F7 => return Some(tilde(18, m)),
                Named::F8 => return Some(tilde(19, m)),
                Named::F9 => return Some(tilde(20, m)),
                Named::F10 => return Some(tilde(21, m)),
                Named::F11 => return Some(tilde(23, m)),
                Named::F12 => return Some(tilde(24, m)),
                _ => {}
            }
        }

        // 规则 4: 可打印字符透传
        encode_text(key, text, mods)
    }

    /// shift||alt||control 任一按下时的修饰参数 m = 1 + shift(1) + alt(2) + ctrl(4)
    fn modifier_param(mods: Modifiers) -> Option<u8> {
        let m = 1
            + u8::from(mods.shift())
            + u8::from(mods.alt()) * 2
            + u8::from(mods.control()) * 4;
        (m > 1).then_some(m)
    }

    /// 方向/Home/End 键: 无修饰时 CSI(或 DECCKM 下的 SS3), 带修饰统一降级为参数化 CSI
    fn csi_or_ss3(final_ch: char, app_cursor: bool, m: Option<u8>) -> Vec<u8> {
        match m {
            Some(m) => format!("\x1b[1;{m}{final_ch}").into_bytes(),
            None if app_cursor => format!("\x1bO{final_ch}").into_bytes(),
            None => format!("\x1b[{final_ch}").into_bytes(),
        }
    }

    /// 数字结尾的 CSI 序列: ESC[n~ 或带修饰 ESC[n;<m>~
    fn tilde(n: u8, m: Option<u8>) -> Vec<u8> {
        match m {
            Some(m) => format!("\x1b[{n};{m}~").into_bytes(),
            None => format!("\x1b[{n}~").into_bytes(),
        }
    }

    /// F1-F4 的 SS3 形式(带修饰降级为 ESC[1;<m><ch>)
    fn fn_key(final_ch: char, m: Option<u8>) -> Vec<u8> {
        match m {
            Some(m) => format!("\x1b[1;{m}{final_ch}").into_bytes(),
            None => format!("\x1bO{final_ch}").into_bytes(),
        }
    }

    /// 可打印字符透传 UTF-8; Alt 按下时前置 ESC(Alt+X → ESC x)
    fn encode_text(key: &Key, text: Option<&str>, mods: Modifiers) -> Option<Vec<u8>> {
        let s = match (text, key) {
            (Some(s), _) if !s.is_empty() && !s.chars().any(char::is_control) => s,
            (None, Key::Character(k)) if !k.is_empty() && !k.chars().any(char::is_control) => k.as_str(),
            (_, Key::Named(Named::Space)) => " ",
            _ => return None,
        };
        let mut bytes = Vec::with_capacity(s.len() + 1);
        if mods.alt() {
            bytes.push(0x1b);
        }
        bytes.extend_from_slice(s.as_bytes());
        Some(bytes)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn enc(key: Key, text: Option<&str>, mods: Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
            encode_key(&key, text, mods, app_cursor)
        }

        #[test]
        fn enter_sends_cr() {
            assert_eq!(enc(Key::Named(Named::Enter), Some("\r"), Modifiers::empty(), false), Some(b"\r".to_vec()));
        }

        #[test]
        fn ctrl_c_sends_etx() {
            assert_eq!(
                enc(Key::Character("c".into()), None, Modifiers::CTRL, false),
                Some(vec![0x03])
            );
        }

        #[test]
        fn ctrl_specials() {
            assert_eq!(
                enc(Key::Character("[".into()), Some("["), Modifiers::CTRL, false),
                Some(vec![0x1b])
            );
            assert_eq!(
                enc(Key::Character("_".into()), Some("_"), Modifiers::CTRL, false),
                Some(vec![0x1f])
            );
        }

        #[test]
        fn arrow_up_csi_mode() {
            assert_eq!(
                enc(Key::Named(Named::ArrowUp), None, Modifiers::empty(), false),
                Some(b"\x1b[A".to_vec())
            );
        }

        #[test]
        fn arrow_up_ss3_mode() {
            assert_eq!(
                enc(Key::Named(Named::ArrowDown), None, Modifiers::empty(), true),
                Some(b"\x1bOB".to_vec())
            );
        }

        #[test]
        fn ctrl_arrow_parameterized_csi() {
            assert_eq!(
                enc(Key::Named(Named::ArrowUp), None, Modifiers::CTRL, true),
                Some(b"\x1b[1;5A".to_vec())
            );
        }

        #[test]
        fn f8_sends_csi_19_tilde() {
            assert_eq!(
                enc(Key::Named(Named::F8), None, Modifiers::empty(), false),
                Some(b"\x1b[19~".to_vec())
            );
        }

        #[test]
        fn shift_f5_parameterized() {
            assert_eq!(
                enc(Key::Named(Named::F5), None, Modifiers::SHIFT, false),
                Some(b"\x1b[15;2~".to_vec())
            );
        }

        #[test]
        fn shift_pgup_never_encoded_locally_intercepted_by_update() {
            // Shift+PgUp 由 update 层截走做本地滚动, 编码层返回 None 兜底
            assert_eq!(
                enc(Key::Named(Named::PageUp), None, Modifiers::SHIFT, false),
                None
            );
        }

        #[test]
        fn plain_pgup_sent_to_remote() {
            assert_eq!(
                enc(Key::Named(Named::PageUp), None, Modifiers::empty(), false),
                Some(b"\x1b[5~".to_vec())
            );
        }

        #[test]
        fn utf8_text_passthrough() {
            assert_eq!(
                enc(Key::Character("é".into()), Some("é"), Modifiers::empty(), false),
                Some("é".as_bytes().to_vec())
            );
        }

        #[test]
        fn alt_prefixes_escape() {
            assert_eq!(
                enc(Key::Character("x".into()), Some("x"), Modifiers::ALT, false),
                Some(b"\x1bx".to_vec())
            );
        }

        #[test]
        fn backspace_and_tab() {
            assert_eq!(enc(Key::Named(Named::Backspace), None, Modifiers::empty(), false), Some(b"\x7f".to_vec()));
            assert_eq!(enc(Key::Named(Named::Tab), None, Modifiers::empty(), false), Some(b"\t".to_vec()));
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

#[cfg(all(test, windows))]
mod pty_input_tests {
    use super::*;

    /// 端到端验证 ConPTY 输入通路: 与 build_service_stream 相同的方式
    /// spawn cmd.exe, 向 master writer 写命令, 断言输出中出现回显/命令结果
    #[test]
    fn pty_input_reaches_child_and_echoes_back() {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 30,
                cols: 100,
                ..Default::default()
            })
            .expect("openpty");
        let cmd = CommandBuilder::new("cmd.exe");
        let mut child = pair.slave.spawn_command(cmd).expect("spawn cmd");
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().expect("clone reader");
        let mut writer = pair.master.take_writer().expect("take writer");

        writer.write_all(b"echo PTY_MARKER_42\r\n").expect("write input");
        writer.flush().expect("flush input");

        // 轮询读输出, 最多 5 秒内应看到标记
        let mut buf = [0u8; 4096];
        let mut seen = String::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline && !seen.contains("PTY_MARKER_42") {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => seen.push_str(&String::from_utf8_lossy(&buf[..n])),
                Err(_) => break,
            }
        }
        let _ = child.kill();
        assert!(
            seen.contains("PTY_MARKER_42"),
            "输入未到达子进程, 实际输出: {seen:?}"
        );
    }
}
