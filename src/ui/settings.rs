use iced::widget::{button, column, container, row, space, text, text_input};
use iced::{Alignment, Element, Length};

use crate::app::{Message, State};
use crate::service::ServiceKind;
use crate::theme;

pub fn view<'a>(state: &'a State) -> Element<'a, Message> {
    let heading = text("配置应用路径").size(20).color(theme::TEXT);

    let mut rows = column![heading, space().height(4)];

    for kind in ServiceKind::ALL {
        rows = rows.push(path_row(kind, state));
        rows = rows.push(space().height(14));
    }

    let save = button(text("保存配置").size(14))
        .style(theme::accent_button_style())
        .padding([10, 24])
        .on_press(Message::SaveConfig);

    let cancel = button(text("还原").size(14))
        .style(theme::ghost_button_style())
        .padding([10, 24])
        .on_press(Message::ResetConfigDraft);

    let actions = row![cancel, save]
        .spacing(12)
        .align_y(Alignment::Center);

    let result_row: Option<iced::Element<'_, Message>> = if let Some(msg) = &state.config_message {
        Some(
            row![text(msg).size(12).color(theme::TEXT_MUTED)]
                .align_y(Alignment::Center)
                .into(),
        )
    } else {
        None
    };

    let mut footer = column![actions];
    if let Some(r) = result_row {
        footer = footer.push(r);
    }

    column![rows, space().height(16), footer]
        .spacing(0)
        .align_x(Alignment::Start)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn label<'a>(kind: ServiceKind) -> Element<'a, Message> {
    text(kind.label()).size(13).color(theme::TEXT_MUTED).into()
}

fn path_row<'a>(kind: ServiceKind, state: &'a State) -> Element<'a, Message> {
    let current = state.config_draft.path_for(kind);

    let input = text_input(kind.placeholder(), current)
        .on_input(move |s| Message::PathEdited(kind, s))
        .size(13)
        .padding(10);

    let browse = button(text("浏览…").size(13))
        .style(theme::ghost_button_style())
        .padding([10, 14])
        .on_press(Message::BrowsePath(kind));

    let body = row![
        container(label(kind)).width(Length::Fixed(140.0)),
        input,
        browse,
    ]
    .spacing(10)
    .align_y(Alignment::Center);

    let set = state.config_draft.path_set(kind);
    let badge = if set {
        text("已设置").size(11).color(theme::SUCCESS)
    } else {
        text("未设置").size(11).color(theme::WARNING)
    };

    column![
        body,
        row![space().width(Length::Fixed(150.0)), badge]
            .width(Length::Fill),
    ]
    .spacing(6)
    .width(Length::Fill)
    .into()
}