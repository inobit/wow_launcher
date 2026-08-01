// 声明为 Windows GUI 程序，避免启动时弹出控制台窗口
#![windows_subsystem = "windows"]

use iced::font::Family;
use iced::window;
use iced::{Font, Size};

mod app;
mod config;
mod process;
mod service;
mod theme;
mod ui;

fn load_icon() -> Option<window::Icon> {
    let bytes = include_bytes!("../assets/icon.png");
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = (img.width(), img.height());
    window::icon::from_rgba(img.into_raw(), w, h).ok()
}

pub fn main() -> iced::Result {
    // 限制 wgpu 只使用原生图形后端（DX12/Vulkan），避免初始化 OpenGL 后端。
    // 部分 AMD 驱动安装不完整时（System32 缺少 atiglpxx.dll），GL 探测会触发
    // "LoadLibrary failed with error 126" 弹窗并阻塞窗口创建。
    std::env::set_var("WGPU_BACKEND", "dx12,vulkan");

    let window_settings = window::Settings {
        size: Size::new(1100.0, 720.0),
        min_size: Some(Size::new(900.0, 600.0)),
        resizable: true,
        icon: load_icon(),
        // 关闭请求先转发给应用层(确认服务停止后才真正关闭)
        exit_on_close_request: false,
        ..Default::default()
    };

    iced::application(app::State::new, app::update, app::view)
        .title(|_state: &app::State| "WoW 启动器".to_string())
        .theme(|_state: &app::State| theme::theme())
        .window(window_settings)
        .default_font(Font {
            family: Family::Name("Microsoft YaHei".into()),
            ..Font::DEFAULT
        })
        .subscription(app::subscription)
        .run()
}