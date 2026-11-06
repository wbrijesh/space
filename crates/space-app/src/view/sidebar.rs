//! Thread list (cmux style): title, 2-line preview, branch • path meta, unread badge.

use gpui::component::spinner::Spinner;
use gpui::component::{Icon, IconName, Sizable as _};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use space_core::PluginStatus;

use super::{icon_button, status_dot, tip, tip_action, title_bar_sidebar};
use crate::app::{SpaceApp, Thread, ToggleLogs};
use crate::format::{self, Preview};
use crate::theme;
use gpui::assets::IconName as Lucide;

pub fn render(app: &SpaceApp, _window: &mut Window, cx: &mut Context<SpaceApp>) -> impl IntoElement {
    let mut list = div().flex().flex_col().gap_2().px_2p5().pt_1().pb_4();
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

fn thread_row(app: &SpaceApp, t: &Thread, cx: &mut Context<SpaceApp>) -> impl IntoElement {
    let selected = app.selected == Some(t.key);
    let key = t.key;
    let (fg, fg2, fg3) = if selected {
        (theme::on_accent(), theme::on_accent_2(), theme::on_accent_3())
    } else {
        (theme::text(), theme::text_2(), theme::text_3())
    };

    let preview = match format::thread_preview(&t.state) {
        Preview::Empty if t.error.is_some() => {
            (format!("Couldn't start a session: {}", t.error.as_deref().unwrap_or("")), Some(theme::red()))
        }
        Preview::Empty if t.queued.is_some() || t.creating => ("Starting…".to_string(), None),
        Preview::Empty => ("No messages yet".to_string(), None),
        Preview::Text(s) => (s, None),
        Preview::Working(Some(tool)) => (format!("Working… {tool}"), None),
        Preview::Working(None) => ("Working…".to_string(), None),
        Preview::NeedsInput => ("Waiting for your input".to_string(), Some(theme::yellow())),
        Preview::Error(e) => (e, Some(theme::red())),
    };
    let (preview_text, preview_color) = preview;
    let preview_color = if selected { fg2 } else { preview_color.unwrap_or(fg2) };

    let mut meta = div()
        .flex()
        .items_center()
        .gap_1()
        .min_w_0()
        .font_family(theme::MONO_FONT)
        .text_size(theme::TEXT_XS)
        .text_color(fg3);
    if let Some(branch) = &t.branch {
        meta = meta
            .child(Icon::new(Lucide::GitBranch).size(px(11.)).flex_none())
            .child(div().flex_none().max_w(px(110.)).truncate().child(branch.clone()))
            .child(div().flex_none().child("•"));
    }
    meta = meta.child(div().min_w_0().truncate().child(format::abbreviate_home(&t.cwd)));

    let running = t.state.running || (t.queued.is_some() && t.error.is_none());
    let mut title_row = div().flex().items_center().gap_2().child(
        div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_size(theme::TEXT_MD)
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(fg)
            .child(t.title()),
    );
    if running {
        title_row = title_row.child(Spinner::new().xsmall().color(if selected { fg2 } else { theme::text_3() }));
    }
    if t.unread > 0 && !selected {
        title_row = title_row.child(
            div()
                .flex_none()
                .min_w(px(18.))
                .h(px(18.))
                .px_1()
                .rounded_full()
                .bg(theme::accent())
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(10.5))
                .font_weight(FontWeight::BOLD)
                .text_color(theme::on_accent())
                .child(t.unread.to_string()),
        );
    }

    div()
        .id(("thread", key as usize))
        .flex()
        .flex_col()
        .gap_1()
        .px_3()
        .py_2p5()
        .rounded(px(10.))
        .cursor_pointer()
        .when(selected, |d| d.bg(theme::accent()))
        .when(!selected, |d| d.hover(|s| s.bg(theme::hover())).active(|s| s.bg(theme::pressed())))
        .child(title_row)
        .child(div().text_size(theme::TEXT_SM).line_height(relative(1.4)).text_color(preview_color).line_clamp(2).child(preview_text))
        .child(meta)
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
