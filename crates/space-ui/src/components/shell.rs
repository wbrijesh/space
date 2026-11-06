//! Shell pieces: sidebar column and footer, logs panel, banners, empty-state heading.

use std::rc::Rc;

use gpui::assets::IconName as Lucide;
use gpui::component::{Icon, IconName};
use gpui::prelude::FluentBuilder as _;
use gpui::*;

use super::widgets::{icon_button, pill_button, status_dot, tip, tip_action};
use crate::ClickHandler;
use crate::actions::ToggleLogs;
use crate::theme::{color, radius, size, text};

/// Sidebar column: title bar segment, scrolling list, footer.
pub fn sidebar_column(title_bar: impl IntoElement, rows: Vec<AnyElement>, footer: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_col()
        .w(size::SIDEBAR_W)
        .flex_none()
        .h_full()
        .bg(color::bg_sidebar())
        .border_r_1()
        .border_color(color::border())
        .child(title_bar)
        .child(
            div()
                .id("thread-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(div().flex().flex_col().gap_0p5().px_2().pt_1().pb_4().children(rows)),
        )
        .child(footer)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentState {
    Starting,
    Running,
    Stopped,
    Exited(String),
}

/// Sidebar footer: agent status, restart (when stopped), logs toggle.
#[derive(IntoElement)]
pub struct AgentFooter {
    title: SharedString,
    state: AgentState,
    logs_open: bool,
    on_restart: Option<ClickHandler>,
    on_toggle_logs: ClickHandler,
}

impl AgentFooter {
    pub fn new(title: impl Into<SharedString>, state: AgentState) -> Self {
        Self { title: title.into(), state, logs_open: false, on_restart: None, on_toggle_logs: Rc::new(|_, _, _| {}) }
    }

    pub fn logs_open(mut self, open: bool) -> Self {
        self.logs_open = open;
        self
    }

    /// Shown only while the agent is stopped or exited.
    pub fn on_restart(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_restart = Some(Rc::new(f));
        self
    }

    pub fn on_toggle_logs(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_toggle_logs = Rc::new(f);
        self
    }
}

impl RenderOnce for AgentFooter {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let (dot, label) = match &self.state {
            AgentState::Running => (color::green(), "running"),
            AgentState::Starting => (color::yellow(), "starting…"),
            AgentState::Stopped => (color::text_3(), "stopped"),
            AgentState::Exited(_) => (color::red(), "exited"),
        };
        let detail = match &self.state {
            AgentState::Exited(msg) => msg.clone(),
            _ => format!("{} · {label}", self.title),
        };
        let stopped = matches!(self.state, AgentState::Stopped | AgentState::Exited(_));
        let toggle = self.on_toggle_logs;
        div()
            .flex_none()
            .h(size::SIDEBAR_FOOTER_H)
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .border_t_1()
            .border_color(color::border())
            .child(
                div()
                    .id("agent-status")
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_1()
                    .min_w_0()
                    .child(status_dot(dot))
                    .child(div().min_w_0().truncate().text_size(text::SM).text_color(color::text_2()).child(self.title))
                    .tooltip(tip(detail)),
            )
            .when_some(self.on_restart.filter(|_| stopped), |d, f| {
                d.child(
                    icon_button("restart-agent", IconName::RefreshCw, true, false)
                        .tooltip(tip("Restart agent"))
                        .on_click(move |e, w, cx| f(e, w, cx)),
                )
            })
            .child(
                icon_button("toggle-logs", Lucide::Terminal, true, self.logs_open)
                    .tooltip(tip_action("Plugin logs", &ToggleLogs))
                    .on_click(move |e, w, cx| toggle(e, w, cx)),
            )
    }
}

/// Bottom panel with the agent's stderr.
#[derive(IntoElement)]
pub struct LogsPanel {
    title: SharedString,
    lines: Vec<SharedString>,
    scroll: ScrollHandle,
    on_close: ClickHandler,
}

impl LogsPanel {
    pub fn new(title: impl Into<SharedString>, lines: Vec<SharedString>, scroll: &ScrollHandle) -> Self {
        Self { title: title.into(), lines, scroll: scroll.clone(), on_close: Rc::new(|_, _, _| {}) }
    }

    pub fn on_close(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_close = Rc::new(f);
        self
    }
}

impl RenderOnce for LogsPanel {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let mut col = div()
            .flex()
            .flex_col()
            .px_4()
            .py_2()
            .font_family(text::MONO_FONT)
            .text_size(text::MONO_SM)
            .line_height(relative(text::LEADING));
        if self.lines.is_empty() {
            col = col.child(div().text_color(color::text_3()).child("No output yet."));
        }
        for line in self.lines {
            col = col.child(div().text_color(color::text_2()).child(line));
        }
        let close = self.on_close;
        div()
            .flex_none()
            .flex()
            .flex_col()
            .h(size::LOGS_H)
            .border_t_1()
            .border_color(color::border())
            .bg(color::bg_sidebar())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(36.))
                    .pl_4()
                    .pr_2()
                    .border_b_1()
                    .border_color(color::border())
                    .text_size(text::SM)
                    .text_color(color::text_2())
                    .child(Icon::new(Lucide::Terminal).size(size::ICON).text_color(color::text_3()))
                    .child(self.title)
                    .child(div().flex_1())
                    .child(
                        icon_button("close-logs", IconName::Close, true, false)
                            .tooltip(tip("Close logs"))
                            .on_click(move |e, w, cx| close(e, w, cx)),
                    ),
            )
            .child(div().id("logs").flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.scroll).child(col))
    }
}

/// Quiet info banner with a dismiss button.
pub fn banner(message: impl Into<SharedString>, on_dismiss: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_2()
        .rounded(radius::XL)
        .bg(color::surface())
        .border_1()
        .border_color(color::border())
        .text_size(text::SM)
        .text_color(color::text_2())
        .child(Icon::new(IconName::Info).size(size::ICON).text_color(color::text_3()))
        .child(div().flex_1().min_w_0().child(message.into()))
        .child(icon_button("dismiss-banner", IconName::Close, true, false).on_click(on_dismiss))
}

/// Red card for an agent problem, with optional Restart / Logs actions.
pub fn problem_card(message: impl Into<SharedString>, actions: Option<(ClickHandler, ClickHandler)>) -> Div {
    div()
        .flex()
        .items_start()
        .gap_2p5()
        .p_3()
        .rounded(radius::XL)
        .bg(color::red_bg())
        .border_1()
        .border_color(color::red_border())
        .child(Icon::new(IconName::CircleAlert).size(size::ICON_MD).text_color(color::red()).mt(px(1.)))
        .child(div().flex_1().min_w_0().text_size(text::SM).line_clamp(4).child(message.into()))
        .when_some(actions, |d, (restart, logs)| {
            d.child(pill_button("problem-restart", "Restart", false).on_click(move |e, w, cx| restart(e, w, cx)))
                .child(pill_button("problem-logs", "Logs", false).on_click(move |e, w, cx| logs(e, w, cx)))
        })
}

/// Heading above the centered composer of an empty thread.
/// Fade over the bottom edge of a scroll area so content dims into the background before it
/// meets the composer. Place inside a `relative()` parent, after the scroll container. Has no
/// hitbox, so wheel and clicks pass through to the content below.
pub fn scroll_fade() -> Div {
    let bg = color::bg();
    div()
        .absolute()
        .left_0()
        .right_0()
        .bottom_0()
        .h(size::SCROLL_FADE_H)
        .bg(linear_gradient(180., linear_color_stop(bg.opacity(0.), 0.), linear_color_stop(bg, 1.)))
}

pub fn empty_heading() -> Div {
    div()
        .flex()
        .justify_center()
        .text_size(text::TITLE)
        .font_weight(FontWeight::SEMIBOLD)
        .child("What should we work on?")
}

#[cfg(feature = "dev")]
pub fn previews() -> Vec<crate::preview::ComponentPreview> {
    use crate::preview::{Area, ComponentPreview, Example};
    let noop: ClickHandler = Rc::new(|_, _, _| {});
    let footer = |name: &'static str, state: AgentState, logs: bool| {
        Example::new(name, move |_, _| {
            AgentFooter::new("Pi Agent", state.clone()).logs_open(logs).on_restart(|_, _, _| {}).into_any_element()
        })
        .width(size::SIDEBAR_W)
    };
    let actions = (noop.clone(), noop);
    vec![
        ComponentPreview::new(Area::Sidebar, "Agent footer", "Agent status and the logs toggle.").stacked().examples([
            footer("running", AgentState::Running, false),
            footer("starting", AgentState::Starting, false),
            footer("exited", AgentState::Exited("exited with code 3".into()), false),
            footer("logs open", AgentState::Running, true),
        ]),
        ComponentPreview::new(Area::Shell, "Banners", "Problems and notices above the composer.").stacked().examples([
            Example::new("agent problem", move |_, _| {
                problem_card("The agent exited: exited with code 3: model request failed", Some(actions.clone()))
                    .into_any_element()
            })
            .width(size::CONTENT_MAX_W),
            Example::new("notice", |_, _| {
                banner("Agent plugin `foo` not found; using `pi-agent`.", |_, _, _| {}).into_any_element()
            })
            .width(size::CONTENT_MAX_W),
        ]),
        ComponentPreview::new(Area::Shell, "Empty state", "New thread: heading over the centered composer.")
            .example(Example::new("heading", |_, _| empty_heading().into_any_element()).width(size::CONTENT_MAX_W)),
        ComponentPreview::new(Area::Shell, "Logs panel", "Agent stderr (⌘J).").stacked().example(
            Example::new("with output", |window, cx| {
                let scroll = window.use_keyed_state("preview-logs-scroll", cx, |_, _| ScrollHandle::new());
                let lines = [
                    "[pi-agent] v0.1.0 starting; model=claude-cli/claude-opus-5-5 thinking=medium",
                    "[pi-agent] initialize: client protocolVersion=1",
                    "[pi-agent] session/new 39d780ef cwd=/Users/me/projects/space",
                ]
                .map(SharedString::from)
                .to_vec();
                LogsPanel::new("pi-agent · stderr", lines, scroll.read(cx)).into_any_element()
            })
            .width(px(900.)),
        ),
    ]
}
