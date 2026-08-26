use iced::widget::{button, column, container, image, row, space, text};
use iced::{Alignment, Element, Length};

use crate::app::{Message, NavTab, State};
use crate::theme;

mod home;
mod settings;
pub(crate) mod terminal;

const LOGO_BYTES: &[u8] = include_bytes!("../../assets/icon.png");

/// 静态缓存图标 Handle: image::Handle::from_bytes 每次调用生成唯一 Id,
/// 若每帧重建会导致纹理缓存失效、图标持续闪烁(终端有输出重绘时更明显)。
/// 预缩放到 64x64: iced 直接缩放 256 源到 40px 会因无 mipmap 而模糊
fn logo_handle() -> &'static image::Handle {
    static HANDLE: std::sync::OnceLock<image::Handle> = std::sync::OnceLock::new();
    HANDLE.get_or_init(|| {
        let resized = ::image::load_from_memory(LOGO_BYTES)
            .map(|img| img.resize(64, 64, ::image::imageops::FilterType::Lanczos3))
            .ok()
            .map(|img| {
                let rgba = img.to_rgba8();
                image::Handle::from_rgba(rgba.width(), rgba.height(), rgba.into_raw())
            });
        resized.unwrap_or_else(|| image::Handle::from_bytes(LOGO_BYTES))
    })
}

pub fn view<'a>(state: &'a State) -> Element<'a, Message> {
    let base = build_base(state);

    if state.close_pending {
        // 关闭确认弹窗: 覆盖在界面之上, 半透明遮罩
        iced::widget::stack([base, close_dialog()]).into()
    } else {
        base
    }
}

fn build_base<'a>(state: &'a State) -> Element<'a, Message> {
    let content = match state.active_nav {
        NavTab::Home => home::view(state),
        NavTab::Settings => settings::view(state),
        NavTab::Terminal => terminal::view(state),
    };

    let content = container(content)
        .padding(24)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(iced::Background::Color(theme::BG)),
            ..Default::default()
        });

    row![sidebar(build_sidebar(state)), content]
        .align_y(Alignment::Start)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn close_dialog<'a>() -> Element<'a, Message> {
    container(
        container(
            column![
                text("服务仍在运行").size(16).color(theme::TEXT),
                text("MySQL / Auth / World 正在运行。关闭启动器将无法再管理它们, 若直接退出可能残留进程。")
                    .size(12)
                    .color(theme::TEXT_MUTED),
                row![
                    button(text("是, 停止服务并退出").size(13))
                        .style(theme::accent_button_style())
                        .padding([8, 16])
                        .on_press(Message::ConfirmClose(true)),
                    button(text("否, 取消").size(13))
                        .style(theme::ghost_button_style())
                        .padding([8, 16])
                        .on_press(Message::ConfirmClose(false)),
                ]
                .spacing(12)
                .align_y(Alignment::Center),
            ]
            .spacing(14)
            .align_x(Alignment::Center),
        )
        .padding(28)
        .width(Length::Shrink)
        .style(|_| theme::card_style()),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .align_x(Alignment::Center)
    .align_y(Alignment::Center)
    .style(|_| container::Style {
        background: Some(iced::Background::Color(iced::Color::from_rgba(
            0.0, 0.0, 0.0, 0.55,
        ))),
        ..Default::default()
    })
    .into()
}

fn sidebar(inner: Element<Message>) -> Element<Message> {
    container(inner)
        .padding([20, 14])
        .width(Length::Fixed(208.0))
        .height(Length::Fill)
        .style(|_| theme::sidebar_style())
        .into()
}

fn build_sidebar<'a>(state: &'a State) -> Element<'a, Message> {
    let logo = image(logo_handle().clone()).width(40).height(40);

    let title = column![
        text("WoW Launcher").size(16),
        text("启动管理器").size(11).color(theme::TEXT_MUTED),
    ]
    .spacing(2);

    let header = row![logo, title].spacing(12).align_y(Alignment::Center);

    let mut nav = column![header, space().height(28)];

    for (tab, label) in [
        (NavTab::Home, "主页"),
        (NavTab::Terminal, "终端"),
        (NavTab::Settings, "配置"),
    ] {
        let active = state.active_nav == tab;
        let btn = button(
            text(label)
                .size(14)
                .color(if active { theme::TEXT } else { theme::TEXT_MUTED }),
        )
        .width(Length::Fill)
        .padding([10, 14])
        .style(theme::nav_button_style(active))
        .on_press(Message::NavSelected(tab));

        nav = nav.push(btn);
    }

    nav = nav.push(space().height(Length::Fill));

    if !state.sequence_message.is_empty() {
        nav = nav.push(
            container(text(&state.sequence_message).size(11).color(theme::TEXT_MUTED))
                .padding([8, 4]),
        );
    }

    nav.width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Start)
        .into()
}