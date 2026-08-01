use iced::widget::{button, column, container, row, space, text};
use iced::{Alignment, Element, Length};

use crate::app::{Message, State};
use crate::service::{ServiceKind, Status};
use crate::theme;

pub fn view<'a>(state: &'a State) -> Element<'a, Message> {
    let running_count = state
        .status
        .iter()
        .filter(|s| **s == Status::Running)
        .count();

    let primary = row![
        one_click_start(state),
        one_click_stop(state),
        running_text(running_count),
        space().width(Length::Fill),
    ]
    .spacing(12)
    .align_y(Alignment::Center);

    let mut cards = column![primary, space().height(20)];

    for kind in ServiceKind::ALL {
        cards = cards.push(service_card(kind, state));
        cards = cards.push(space().height(14));
    }

    column![cards.spacing(0)]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn running_text(running: usize) -> Element<'static, Message> {
    container(text(format!("运行中服务: {}/3", running)).size(13).color(theme::TEXT_MUTED))
        .padding([4, 8])
        .into()
}

fn one_click_start<'a>(state: &'a State) -> Element<'a, Message> {
    let busy = state.sequence_active;
    let ready = state.config.path_set(ServiceKind::Mysql)
        && state.config.path_set(ServiceKind::Auth)
        && state.config.path_set(ServiceKind::World);

    container(
        button(text("一键启动").size(14))
            .style(theme::accent_button_style())
            .padding([10, 22])
            .on_press_maybe(if busy || !ready { None } else { Some(Message::StartAll) }),
    )
    .into()
}

fn one_click_stop<'a>(state: &'a State) -> Element<'a, Message> {
    let any_running = state
        .status
        .iter()
        .any(|s| matches!(s, Status::Starting | Status::Running | Status::Stopping));
    let busy = state.sequence_active;

    container(
        button(text("一键停止").size(14))
            .style(theme::danger_button_style())
            .padding([10, 22])
            .on_press_maybe(if busy || !any_running { None } else { Some(Message::StopAll) }),
    )
    .into()
}

fn service_card<'a>(kind: ServiceKind, state: &'a State) -> Element<'a, Message> {
    let status = state.status[kind.index()];
    let path = state.config.path_for(kind);

    let dot_color = match status {
        Status::Running => theme::SUCCESS,
        Status::Starting | Status::Stopping | Status::Waiting => theme::WARNING,
        Status::Error => theme::DANGER,
        Status::Stopped => theme::TEXT_MUTED,
    };

    let status_dot = container(text("\u{25CF}").size(16).color(dot_color))
        .width(Length::Shrink)
        .height(Length::Shrink);

    let mut info = column![
        row![status_dot, text(kind.label()).size(15).color(theme::TEXT)]
            .spacing(10)
            .align_y(Alignment::Center),
        text(if path.is_empty() {
            "未配置路径"
        } else {
            path
        })
        .size(11)
        .color(theme::TEXT_MUTED),
    ];
    // 客户端是独立进程, 不追踪状态, 不显示状态行
    if kind != ServiceKind::Client {
        info = info.push(
            row![
                text(status.text()).size(12).color(dot_color),
                space().width(Length::Fill),
            ],
        );
    }
    let info = info.spacing(4).width(Length::Fill);

    let mut actions = row![].spacing(8);

    if kind == ServiceKind::Client {
        actions = actions.push(
            button(text("启动").size(13))
                .style(theme::accent_button_style())
                .padding([8, 16])
                .on_press_maybe(
                    if path.is_empty() { None } else { Some(Message::StartService(kind)) },
                ),
        );
    } else {
        match status {
            Status::Stopped | Status::Error => {
                actions = actions.push(
                    button(text("启动").size(13))
                        .style(theme::accent_button_style())
                        .padding([8, 16])
                        .on_press_maybe(
                            if path.is_empty() { None } else { Some(Message::StartService(kind)) },
                        ),
                );
            }
            // Starting/Running/Stopping 统一渲染"停止/重启", 避免停止中按钮消失造成闪烁
            Status::Running | Status::Starting | Status::Stopping => {
                actions = actions.push(
                    button(text("停止").size(13))
                        .style(theme::danger_button_style())
                        .padding([8, 16])
                        .on_press(Message::StopService(kind)),
                );
                actions = actions.push(
                    button(text("重启").size(13))
                        .style(theme::ghost_button_style())
                        .padding([8, 16])
                        .on_press(Message::RestartService(kind)),
                );
            }
            // 一键启动排队中, 提供"停止"取消排队
            Status::Waiting => {
                actions = actions.push(
                    button(text("停止").size(13))
                        .style(theme::danger_button_style())
                        .padding([8, 16])
                        .on_press(Message::StopService(kind)),
                );
            }
        }
    }

    let body = row![info, actions.align_y(Alignment::Center)]
        .spacing(16)
        .align_y(Alignment::Center)
        .width(Length::Fill);

    container(body)
        .padding([16, 18])
        .width(Length::Fill)
        .style(|_| theme::card_style())
        .into()
}