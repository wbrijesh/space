//! Thread list (cmux style): title, 2-line preview, branch • path meta, unread badge.

use gpui::component::spinner::Spinner;
use gpui::component::{IconName, Sizable as _};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use space_core::PluginStatus;

use super::{icon_button, status_dot, tip, tip_action, title_bar_sidebar};
use crate::app::{SpaceApp, Thread, ToggleLogs};
use crate::format::{self, Preview};
use crate::theme;
use gpui::assets::IconName as Lucide;

pub fn render(app: &SpaceApp, _window: &mut Window, cx: &mut Context<SpaceApp>) -> impl IntoElement {
    let mut list = div().flex().flex_col().gap_0p5().px_2().pt_1().pb_4();
    // Newest first.
    for thread in app.threads.iter().rev() {
        list = list.child(thread_row(app, thread, cx));
    }

    div()
        .flex()
        .flex_col()
        .w(theme::SIDEBAR_W)
        .flex_none()
        .h_full()
        .bg(theme::bg_sidebar())
        .border_r_1()
        .border_color(theme::border())
        .child(title_bar_sidebar(app, cx))
        .child(div().id("thread-list").flex_1().min_h_0().overflow_y_scroll().child(list))
        .child(footer(app, cx))
}

/// One line per thread: its title, plus a spinner while it works and a dot when it has
/// news you haven't seen (yellow if it is waiting for your input).
fn thread_row(app: &SpaceApp, t: &Thread, cx: &mut Context<SpaceApp>) -> impl IntoElement {
    let selected = app.selected == Some(t.key);
    let key = t.key;
    let running = t.state.running || (t.queued.is_some() && t.error.is_none());
    let needs_input = t.state.pending_permission().is_some();
    let failed = matches!(format::thread_preview(&t.state), Preview::Error(_)) || t.error.is_some();

    let indicator: Option<AnyElement> = if needs_input && !selected {
        Some(status_dot(theme::yellow()).into_any_element())
    } else if running {
        Some(Spinner::new().xsmall().color(theme::text_3()).into_any_element())
    } else if t.unread > 0 && !selected {
        Some(status_dot(if failed { theme::red() } else { theme::accent() }).into_any_element())
    } else {
        None
    };

    div()
        .id(("thread", key as usize))
        .flex()
        .items_center()
        .gap_2()
        .h(px(32.))
        .px_2p5()
        .rounded(px(7.))
        .cursor_pointer()
        .text_size(theme::TEXT_MD)
        .map(|d| {
            if selected {
                d.bg(theme::pressed()).text_color(theme::text())
            } else {
                d.text_color(theme::text_2())
                    // Quieter than the selected fill, so hover never reads as a second selection.
                    .hover(|s| s.bg(theme::surface()).text_color(theme::text()))
                    .active(|s| s.bg(theme::hover()))
            }
        })
        .child(div().flex_1().min_w_0().truncate().child(t.title()))
        .children(indicator)
        .on_click(cx.listener(move |this, _, window, cx| this.select_thread(key, window, cx)))
}

fn footer(app: &SpaceApp, cx: &mut Context<SpaceApp>) -> impl IntoElement {
    let (color, label) = match &app.agent_status {
        PluginStatus::Running => (theme::green(), "Running".to_string()),
        PluginStatus::Starting => (theme::yellow(), "Starting…".to_string()),
        PluginStatus::Stopped => (theme::text_3(), "Stopped".to_string()),
        PluginStatus::Crashed(_) => (theme::red(), "Exited".to_string()),
    };
    let detail = match &app.agent_status {
        PluginStatus::Crashed(msg) => msg.clone(),
        _ => format!("{} · {}", app.agent_title, label.to_lowercase()),
    };
    let stopped = matches!(app.agent_status, PluginStatus::Crashed(_) | PluginStatus::Stopped);
    div()
        .flex_none()
        .h(px(44.))
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .border_t_1()
        .border_color(theme::border())
        .child(
            div()
                .id("agent-status")
                .flex()
                .items_center()
                .gap_2()
                .flex_1()
                .min_w_0()
                .child(status_dot(color))
                .child(div().min_w_0().truncate().text_size(theme::TEXT_SM).text_color(theme::text_2()).child(app.agent_title.clone()))
                .tooltip(tip(detail)),
        )
        .when(stopped && app.agent_id.is_some(), |d| {
            d.child(
                icon_button("restart-agent", IconName::RefreshCw, true, false)
                    .tooltip(tip("Restart agent"))
                    .on_click(cx.listener(|this, _, _, cx| this.restart_agent(cx))),
            )
        })
        .child(
            icon_button("toggle-logs", Lucide::Terminal, true, app.show_logs)
                .tooltip(tip_action("Plugin logs", &ToggleLogs))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_logs(cx))),
        )
}
