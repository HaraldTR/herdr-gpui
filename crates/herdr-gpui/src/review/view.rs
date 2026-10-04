//! The review dialog: the focused checkout's changes, a note composer for
//! the line the user picked, and the queued notes with Send. Git runs on the
//! background executor; nothing reaches an agent until the user presses Send.
use super::{
    diff::{Kind, Loaded},
    notes::{self, MAX_NOTES, Note},
};
use crate::{
    HerdrWindow, fonts::StyledFont, menu::Page, pull_request::Input, search_input::SearchInput,
    window::Flash,
};
use gpui::{prelude::*, *};
use herdr_client::protocol::ClientShellSnapshot;
use std::{collections::HashMap, sync::Arc};

/// The notes column beside the diff.
const NOTES_WIDTH: f32 = 300.;

/// The agent the notes go back to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Agent {
    pub pane_id: String,
    pub label: String,
}

/// The agent whose changes these are: the one in the focused pane, or else
/// the first in the focused workspace.
pub(crate) fn pick_agent(snapshot: &ClientShellSnapshot) -> Option<Agent> {
    let workspace = snapshot.focused_workspace_id.as_deref().or_else(|| {
        snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.focused)
            .map(|workspace| workspace.workspace_id.as_str())
    })?;
    let agent = snapshot
        .focused_pane_id
        .as_deref()
        .and_then(|pane| crate::agent_notes::agent(snapshot, pane))
        .filter(|agent| agent.workspace_id == workspace)
        .or_else(|| {
            snapshot
                .agents
                .iter()
                .find(|agent| agent.workspace_id == workspace)
        })?;
    let label = [&agent.display_agent, &agent.agent, &agent.name]
        .into_iter()
        .flatten()
        .map(|label| crate::notifications::safe_text(label, 80))
        .find(|label| !label.trim().is_empty())
        .unwrap_or_else(|| "the agent".into());
    Some(Agent {
        pane_id: agent.pane_id.clone(),
        label,
    })
}

enum State {
    Loading,
    Loaded(Arc<Loaded>),
    Failed(String),
}

pub(crate) struct Review {
    /// The checkout under review; notes belong to it.
    checkout: Input,
    agent: Option<Agent>,
    /// The endpoint the agent's daemon was on when the review opened.
    endpoint: usize,
    state: State,
    /// The row a note is being written for.
    draft: Option<usize>,
    notes: Vec<Note>,
    /// Each noted row and its note's number, recomputed when either changes.
    marks: HashMap<usize, usize>,
    input: Entity<SearchInput>,
    scroll: UniformListScrollHandle,
    /// Numbers loads, so only the latest one lands.
    request: u64,
}

impl Review {
    fn loaded(&self) -> Option<&Arc<Loaded>> {
        match &self.state {
            State::Loaded(loaded) => Some(loaded),
            _ => None,
        }
    }

    fn refresh_marks(&mut self) {
        self.marks.clear();
        let Some(loaded) = self.loaded().cloned() else {
            return;
        };
        for (index, note) in self.notes.iter().enumerate() {
            if let Some(row) = loaded.diff.row_of(&note.anchor) {
                self.marks.entry(row).or_insert(index + 1);
            }
        }
    }
}

impl HerdrWindow {
    /// Opens the review of the focused checkout's uncommitted changes. Notes
    /// already queued for the same checkout stay; its diff is read again.
    pub(crate) fn open_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(checkout) = self.git.tracked().cloned() else {
            self.show_flash(Flash::warning("No local checkout to review"), cx);
            return;
        };
        if !self.open_menu(window, cx) {
            return;
        }
        let agent = self.live.snapshot.as_deref().and_then(pick_agent);
        let endpoint = self.selected_endpoint;
        let review = match self.menu.review.take() {
            Some(review) if review.checkout == checkout => review,
            _ => {
                let input = cx.new(|cx| {
                    let mut input = SearchInput::new(cx);
                    input.set_placeholder("Describe the change\u{2026}", cx);
                    input
                });
                Review {
                    checkout: checkout.clone(),
                    agent: None,
                    endpoint,
                    state: State::Loading,
                    draft: None,
                    notes: Vec::new(),
                    marks: HashMap::new(),
                    input,
                    scroll: UniformListScrollHandle::new(),
                    request: 0,
                }
            }
        };
        let mut review = Review {
            agent,
            endpoint,
            state: State::Loading,
            draft: None,
            request: review.request + 1,
            ..review
        };
        review.refresh_marks();
        let (ui, theme) = (self.config.ui.clone(), self.theme.clone());
        review
            .input
            .update(cx, |input, cx| input.set_appearance(ui, theme, cx));
        let request = review.request;
        self.menu.review = Some(review);
        self.menu.page = Some(Page::Review);
        let loading = cx
            .background_executor()
            .spawn(async move { super::diff::load(&checkout) });
        cx.spawn(async move |this, cx| {
            let result = loading.await;
            this.update(cx, |this, cx| {
                this.review_loaded(request, result);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Opens the dialog on `loaded` as if Git had just read it.
    #[cfg(test)]
    fn seed_review(&mut self, loaded: Loaded, cx: &mut Context<Self>) {
        let input = cx.new(SearchInput::new);
        let checkout = Input {
            checkout: Some("/work/repo".into()),
            repo_key: "/work/repo/.git".into(),
            branch: "feature".into(),
        };
        let agent = self.live.snapshot.as_deref().and_then(pick_agent);
        let mut review = Review {
            checkout,
            agent,
            endpoint: self.selected_endpoint,
            state: State::Loading,
            draft: None,
            notes: Vec::new(),
            marks: HashMap::new(),
            input,
            scroll: UniformListScrollHandle::new(),
            request: 1,
        };
        review.state = State::Loaded(Arc::new(loaded));
        self.menu.review = Some(review);
        self.menu.page = Some(Page::Review);
    }

    fn review_loaded(&mut self, request: u64, result: crate::Result<Loaded>) {
        let Some(review) = self
            .menu
            .review
            .as_mut()
            .filter(|review| review.request == request)
        else {
            return;
        };
        review.state = match result {
            Ok(loaded) => State::Loaded(Arc::new(loaded)),
            Err(error) => {
                tracing::warn!(%error, "Could not read the changes to review");
                State::Failed(error.to_string())
            }
        };
        review.refresh_marks();
    }

    /// Starts a note on `row`, typed in the composer.
    pub(crate) fn begin_review_note(
        &mut self,
        row: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(review) = self.menu.review.as_mut() else {
            return;
        };
        if review.notes.len() >= MAX_NOTES {
            self.show_flash(
                Flash::warning("Send or remove notes before adding more"),
                cx,
            );
            return;
        }
        if review
            .loaded()
            .is_none_or(|loaded| loaded.diff.anchor(row).is_none())
        {
            return;
        }
        review.draft = Some(row);
        let input = review.input.clone();
        input.update(cx, |input, cx| input.clear(cx));
        let focus = input.read(cx).focus.clone();
        window.focus(&focus, cx);
        cx.notify();
    }

    pub(crate) fn add_review_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(review) = self.menu.review.as_mut() else {
            return;
        };
        let text = review.input.read(cx).text().to_owned();
        let Some(anchor) = review
            .draft
            .zip(review.loaded())
            .and_then(|(row, loaded)| loaded.diff.anchor(row))
        else {
            return;
        };
        let Some(note) = Note::new(anchor, &text) else {
            self.show_flash(Flash::warning("Write what should change first"), cx);
            return;
        };
        review.notes.push(note);
        review.draft = None;
        review.refresh_marks();
        review.input.update(cx, |input, cx| input.clear(cx));
        window.focus(&self.menu.focus, cx);
        cx.notify();
    }

    fn cancel_review_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(review) = self.menu.review.as_mut() {
            review.draft = None;
        }
        window.focus(&self.menu.focus, cx);
        cx.notify();
    }

    fn remove_review_note(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(review) = self.menu.review.as_mut()
            && index < review.notes.len()
        {
            review.notes.remove(index);
            review.refresh_marks();
        }
        cx.notify();
    }

    /// The queued notes as the agent's prompt, with where they go.
    fn review_prompt(&self) -> Option<(String, Option<String>, bool)> {
        let review = self.menu.review.as_ref()?;
        let loaded = review.loaded()?;
        if review.notes.is_empty() {
            return None;
        }
        let text = notes::prompt(&loaded.checkout, &review.notes);
        let pane = review.agent.as_ref().map(|agent| agent.pane_id.clone());
        Some((text, pane, review.endpoint == self.selected_endpoint))
    }

    /// Sends the notes to the agent and closes the review; the queue is
    /// cleared at once so a second press cannot repeat it.
    pub(crate) fn send_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((text, pane, here)) = self.review_prompt() else {
            return;
        };
        if let Some(review) = self.menu.review.as_mut() {
            review.notes.clear();
            review.refresh_marks();
        }
        self.dismiss_menu(window, cx);
        self.deliver_notes(pane, here, text, cx);
    }

    fn copy_review(&mut self, cx: &mut Context<Self>) {
        let Some((text, _, _)) = self.review_prompt() else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.show_flash(Flash::success("Notes copied"), cx);
    }

    /// Keys the dialog itself takes; the composer handles its own.
    pub(crate) fn review_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key.as_str() == "escape" {
            cx.stop_propagation();
            if self
                .menu
                .review
                .as_ref()
                .is_some_and(|review| review.draft.is_some())
            {
                self.cancel_review_note(window, cx);
            } else {
                self.dismiss_menu(window, cx);
            }
        }
    }

    pub(crate) fn render_review(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme.clone();
        let Some(review) = self.menu.review.as_ref() else {
            return div().into_any_element();
        };
        let line_height = self.config.terminal.line_height().max(14.);
        let destination = match &review.agent {
            Some(agent) => format!("Notes go to {}", agent.label),
            None => "No agent in this workspace; notes can be copied".into(),
        };
        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(rgb(theme.active))
            .child(
                div()
                    .flex_none()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Review changes"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(rgb(theme.muted))
                    .child(review.checkout.branch.clone()),
            )
            .child(
                div()
                    .debug_selector(|| "review-destination".into())
                    .flex_none()
                    .text_color(rgb(theme.muted))
                    .child(destination),
            );
        let body = match &review.state {
            State::Loading => div()
                .p_3()
                .text_color(rgb(theme.muted))
                .child("Reading changes\u{2026}")
                .into_any_element(),
            State::Failed(error) => div()
                .debug_selector(|| "review-error".into())
                .p_3()
                .text_color(crate::menu::danger(&theme))
                .child(error.clone())
                .into_any_element(),
            State::Loaded(loaded) if loaded.diff.rows.is_empty() => div()
                .p_3()
                .text_color(rgb(theme.muted))
                .child("No uncommitted changes")
                .into_any_element(),
            State::Loaded(loaded) => {
                let count = loaded.diff.rows.len();
                let truncated = loaded.diff.truncated;
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .text_font(&self.config.terminal)
                    .text_size(px(self.config.terminal.size))
                    .child(
                        uniform_list(
                            "review-diff",
                            count,
                            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                                this.review_rows(range, line_height, cx)
                            }),
                        )
                        .track_scroll(&review.scroll)
                        .flex_1()
                        .min_h_0(),
                    )
                    .when(truncated, |list| {
                        list.child(
                            div()
                                .px_3()
                                .py_1()
                                .text_color(rgb(theme.muted))
                                .child("The change is too large to show in full"),
                        )
                    })
                    .into_any_element()
            }
        };
        div()
            .id("review")
            .debug_selector(|| "review".into())
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(div().flex_1().min_w_0().flex().flex_col().child(body))
                    .child(self.render_review_notes(review, cx)),
            )
            .into_any_element()
    }

    fn review_rows(
        &mut self,
        range: std::ops::Range<usize>,
        line_height: f32,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = &self.theme;
        let Some(review) = self.menu.review.as_ref() else {
            return Vec::new();
        };
        let Some(loaded) = review.loaded().cloned() else {
            return Vec::new();
        };
        let number = |value: Option<u32>| {
            div()
                .flex_none()
                .w(px(44.))
                .pr_1()
                .flex()
                .justify_end()
                .text_color(rgb(theme.muted))
                .child(value.map(|value| value.to_string()).unwrap_or_default())
        };
        let tint = |color: u32| rgba((color << 8) | 0x2c);
        range
            .filter_map(|index| {
                let row = loaded.diff.rows.get(index)?;
                let noteable = matches!(
                    row.kind,
                    Kind::File | Kind::Added | Kind::Removed | Kind::Context
                );
                let mark = review.marks.get(&index).copied();
                let drafting = review.draft == Some(index);
                let (sign, background, text) = match row.kind {
                    Kind::Added => ("+", Some(tint(theme.palette[2])), theme.foreground),
                    Kind::Removed => ("-", Some(tint(theme.palette[1])), theme.foreground),
                    Kind::Context => (" ", None, theme.foreground),
                    Kind::Hunk | Kind::Meta => ("", None, theme.muted),
                    Kind::File => ("", Some(rgb(theme.active)), theme.foreground),
                };
                let content = if row.kind == Kind::File {
                    let name = loaded.diff.files.get(row.file).cloned().unwrap_or_default();
                    if row.text.is_empty() {
                        name
                    } else {
                        format!("{name} ({})", row.text)
                    }
                } else {
                    row.text.clone()
                };
                Some(
                    div()
                        .id(("review-row", index))
                        .debug_selector(move || format!("review-row-{index}"))
                        .h(px(line_height))
                        .flex()
                        .items_center()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .when_some(background, |row, background| row.bg(background))
                        .when(drafting, |row| row.bg(rgb(theme.active)))
                        .text_color(rgb(text))
                        .when(noteable, |row| {
                            row.cursor_pointer()
                                .hover(|row| row.bg(rgb(theme.active)))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.begin_review_note(index, window, cx);
                                }))
                        })
                        .child(
                            div()
                                .flex_none()
                                .w(px(20.))
                                .flex()
                                .justify_center()
                                .when_some(mark, |slot, mark| {
                                    slot.child(
                                        div()
                                            .size(px(16.))
                                            .rounded_full()
                                            .bg(rgb(theme.palette[3]))
                                            .text_color(rgb(theme.text_on(theme.palette[3])))
                                            .text_size(px(10.))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(mark.to_string()),
                                    )
                                }),
                        )
                        .when(row.kind != Kind::File, |line| {
                            line.child(number(row.old))
                                .child(number(row.new))
                                .child(div().flex_none().w(px(16.)).child(sign))
                        })
                        .when(row.kind == Kind::File, |line| {
                            line.font_weight(FontWeight::SEMIBOLD).pl_1()
                        })
                        .child(div().min_w_0().child(content))
                        .into_any_element(),
                )
            })
            .collect()
    }

    fn render_review_notes(&self, review: &Review, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = &self.theme;
        let button = |id: &'static str, label: &'static str, primary: bool| {
            let background = if primary {
                theme.primary()
            } else {
                theme.active
            };
            div()
                .id(id)
                .debug_selector(move || id.into())
                .px_2()
                .py_1()
                .rounded(px(crate::config::corners::CONTROL))
                .cursor_pointer()
                .bg(rgb(background))
                .text_color(rgb(theme.text_on(background)))
                .child(label)
        };
        let drafting = review
            .draft
            .zip(review.loaded())
            .and_then(|(row, loaded)| loaded.diff.anchor(row))
            .and_then(|anchor| Note::new(anchor, "x"))
            .map(|note| note.place());
        let composer = drafting.map(|place| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .border_b_1()
                .border_color(rgb(theme.active))
                .child(div().text_color(rgb(theme.muted)).truncate().child(place))
                .child(
                    div()
                        .id("review-input")
                        .debug_selector(|| "review-input".into())
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            let composing = this
                                .menu
                                .review
                                .as_ref()
                                .is_some_and(|review| review.input.read(cx).is_composing());
                            if composing {
                                return;
                            }
                            match event.keystroke.key.as_str() {
                                "enter" => this.add_review_note(window, cx),
                                "escape" => this.cancel_review_note(window, cx),
                                _ => return,
                            }
                            cx.stop_propagation();
                        }))
                        .child(review.input.clone()),
                )
                .child(div().flex().child(
                    button("review-add", "Add note", true).on_click(
                        cx.listener(|this, _, window, cx| this.add_review_note(window, cx)),
                    ),
                ))
        });
        let rows = review.notes.iter().enumerate().map(|(index, note)| {
            div()
                .id(("review-note", index))
                .flex()
                .gap_2()
                .p_2()
                .border_b_1()
                .border_color(rgb(theme.active))
                .child(
                    div()
                        .flex_none()
                        .size(px(18.))
                        .rounded_full()
                        .bg(rgb(theme.palette[3]))
                        .text_color(rgb(theme.text_on(theme.palette[3])))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child((index + 1).to_string()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_color(rgb(theme.muted))
                                .truncate()
                                .child(note.place()),
                        )
                        .child(div().child(note.comment.clone())),
                )
                .child(
                    div()
                        .id(("review-remove", index))
                        .flex_none()
                        .size(px(18.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .rounded(px(crate::config::corners::CONTROL))
                        .hover(|s| s.bg(rgb(theme.active)))
                        .child(
                            svg()
                                .path("icons/close.svg")
                                .size(px(12.))
                                .text_color(rgb(theme.muted)),
                        )
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.remove_review_note(index, cx)),
                        ),
                )
        });
        let has_notes = !review.notes.is_empty();
        let has_agent = review.agent.is_some();
        div()
            .id("review-notes")
            .debug_selector(|| "review-notes".into())
            .flex_none()
            .w(px(NOTES_WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(rgb(theme.active))
            .children(composer)
            .child(
                div()
                    .id("review-note-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(rows)
                    .when(!has_notes && review.draft.is_none(), |list| {
                        list.child(
                            div()
                                .p_2()
                                .text_color(rgb(theme.muted))
                                .child("Click a line or a file name to note what should change."),
                        )
                    }),
            )
            .when(has_notes, |panel| {
                panel.child(
                    div()
                        .flex()
                        .gap_1()
                        .p_2()
                        .border_t_1()
                        .border_color(rgb(theme.active))
                        .when(has_agent, |row| {
                            row.child(button("review-send", "Send to agent", true).on_click(
                                cx.listener(|this, _, window, cx| this.send_review(window, cx)),
                            ))
                        })
                        .child(
                            button("review-copy", "Copy", !has_agent)
                                .on_click(cx.listener(|this, _, _, cx| this.copy_review(cx))),
                        ),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    // Not `super::*`: it brings in `gpui::test`, which `#[test]` would then name.
    use super::{Agent, HerdrWindow, Loaded, State, pick_agent};
    use gpui::Entity;
    use herdr_client::protocol::ClientShellSnapshot;
    use std::{collections::HashMap, sync::Arc};

    fn snapshot(value: serde_json::Value) -> ClientShellSnapshot {
        serde_json::from_value(value).unwrap()
    }

    fn agent(pane: &str, workspace: &str, label: Option<&str>) -> serde_json::Value {
        serde_json::json!({
            "pane_id": pane, "workspace_id": workspace, "tab_id": "t0", "name": null,
            "display_agent": label, "agent": null, "title": null,
            "terminal_title": null, "terminal_title_stripped": null,
            "agent_status": "idle", "state_change_seq": 0, "state_labels": [],
            "tokens": [], "focused": false
        })
    }

    fn base(focused_pane: Option<&str>, agents: Vec<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({
            "boot_id": "b", "revision": 1, "config_diagnostic": null,
            "product_announcement": null, "update_available": null,
            "update_install_command": "", "server_keybindings_toml": null,
            "latest_release_notes_available": false, "integration_updates_available": false,
            "worktree_directory": "", "release_notes": null,
            "focused_workspace_id": "w1", "focused_tab_id": null,
            "focused_pane_id": focused_pane, "tab_bar_right": [],
            "tab_bar_right_separator": "", "agent_view_label": null, "agent_order": [],
            "workspaces": [], "tabs": [], "panes": [], "agents": agents, "commands": []
        })
    }

    /// The fixture window, its workspace `w0` focused, with an agent in pane
    /// `w0:p1` when `status` is given.
    fn window<'a>(
        cx: &'a mut gpui::TestAppContext,
        status: Option<&str>,
    ) -> (Entity<HerdrWindow>, &'a mut gpui::VisualTestContext) {
        cx.add_window_view(|window, cx| {
            let mut view = crate::sidebar::layout_tests::fixture_window(window, cx);
            let mut shown =
                serde_json::to_value(crate::sidebar::layout_tests::snapshot(2)).unwrap();
            shown["focused_workspace_id"] = "w0".into();
            shown["focused_pane_id"] = "w0:p1".into();
            shown["panes"] = serde_json::json!([{
                "pane_id": "w0:p1", "workspace_id": "w0", "tab_id": "t0", "label": null,
                "cwd": null, "foreground_cwd": null, "focused": true,
                "right_click_passthrough": false
            }]);
            shown["agents"] = match status {
                Some(status) => {
                    let mut found = agent("w0:p1", "w0", Some("Claude Code"));
                    found["agent_status"] = status.into();
                    serde_json::json!([found])
                }
                None => serde_json::json!([]),
            };
            view.live.snapshot = Some(Arc::new(snapshot(shown)));
            view
        })
    }

    fn changes() -> Loaded {
        Loaded {
            checkout: "/work/repo".into(),
            diff: super::super::diff::Diff::parse(
                "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,2 @@\n fn a() {}\n-fn b() {}\n+fn b() { todo!() }\n",
            ),
        }
    }

    /// Writes `comment` on diff row `row`, the way a click and typing would.
    fn note(
        view: &Entity<HerdrWindow>,
        cx: &mut gpui::VisualTestContext,
        row: usize,
        comment: &str,
    ) {
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.begin_review_note(row, window, cx);
                let input = view.menu.review.as_ref().unwrap().input.clone();
                input.update(cx, |input, cx| input.set_text_selected(comment, cx));
                view.add_review_note(window, cx);
            });
        });
    }

    fn kept(cx: &mut gpui::VisualTestContext) -> Option<String> {
        cx.update(|_, cx| {
            cx.default_global::<crate::browser::Feedback>()
                .take("w0:p1")
        })
    }

    #[gpui::test]
    fn notes_on_lines_reach_the_agent_that_made_the_changes(cx: &mut gpui::TestAppContext) {
        let (view, cx) = window(cx, Some("working"));
        cx.update(|_, cx| view.update(cx, |view, cx| view.seed_review(changes(), cx)));
        cx.update(|window, cx| crate::sidebar::layout_tests::full_draw(window, cx).clear(cx));
        // Hunk headers take no notes; an empty note is refused.
        note(&view, cx, 1, "ignored");
        note(&view, cx, 4, "   ");
        note(&view, cx, 4, "Implement this");
        note(&view, cx, 0, "Add a test");
        view.read_with(cx, |view, _| {
            let review = view.menu.review.as_ref().unwrap();
            assert_eq!(review.notes.len(), 2);
            assert_eq!(review.marks, HashMap::from([(4, 1), (0, 2)]));
            assert!(review.draft.is_none());
        });
        cx.update(|window, cx| crate::sidebar::layout_tests::full_draw(window, cx).clear(cx));

        // The agent is working: the notes wait for it, and the queue is
        // emptied at once so Send cannot repeat them.
        cx.update(|window, cx| view.update(cx, |view, cx| view.send_review(window, cx)));
        view.read_with(cx, |view, _| {
            assert_eq!(view.deliveries.len(), 1);
            assert!(view.menu.page.is_none());
            assert!(view.menu.review.as_ref().unwrap().notes.is_empty());
        });
        cx.update(|_, cx| view.update(cx, |view, cx| view.poll_deliveries(cx)));
        assert_eq!(view.read_with(cx, |view, _| view.deliveries.len()), 1);

        // This fixture has no connection, so the paste fails once the agent
        // waits and the notes are kept for `browser feedback` instead.
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                let mut shown = (*view.live.snapshot.clone().unwrap()).clone();
                shown.agents[0].agent_status = herdr_client::protocol::AgentStatus::Idle;
                view.live.snapshot = Some(Arc::new(shown));
                view.poll_deliveries(cx);
            });
        });
        let text = kept(cx).unwrap();
        assert!(text.starts_with("Review notes on the uncommitted changes in /work/repo"));
        assert!(text.contains(
            "\n1. On `src/lib.rs:2` (added line)\n   Code: `fn b() { todo!() }`\n   Note: Implement this\n"
        ), "{text}");
        assert!(
            text.contains("\n2. On `src/lib.rs` as a whole\n   Note: Add a test\n"),
            "{text}"
        );
    }

    #[gpui::test]
    fn without_an_agent_the_notes_are_copied(cx: &mut gpui::TestAppContext) {
        let (view, cx) = window(cx, None);
        cx.update(|_, cx| view.update(cx, |view, cx| view.seed_review(changes(), cx)));
        note(&view, cx, 3, "Why remove this?");
        cx.update(|window, cx| view.update(cx, |view, cx| view.send_review(window, cx)));
        let copied = cx
            .update(|_, cx| cx.read_from_clipboard())
            .and_then(|item| item.text());
        assert!(copied.is_some_and(|text| text.contains("`src/lib.rs:2` (removed line")));
        assert!(kept(cx).is_none());
    }

    #[gpui::test]
    fn a_stale_load_never_replaces_a_newer_one(cx: &mut gpui::TestAppContext) {
        let (view, cx) = window(cx, None);
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.seed_review(changes(), cx);
                view.review_loaded(0, Err(crate::Error::GitWorker));
                let review = view.menu.review.as_ref().unwrap();
                assert!(review.loaded().is_some());
                view.review_loaded(1, Err(crate::Error::GitWorker));
                let review = view.menu.review.as_ref().unwrap();
                assert!(matches!(review.state, State::Failed(_)));
            });
        });
    }

    #[test]
    fn notes_go_to_the_focused_agent_or_the_workspace_s_first() {
        let agents = vec![
            agent("w0:p1", "w0", Some("Codex")),
            agent("w1:p1", "w1", Some("Claude Code")),
            agent("w1:p2", "w1", Some("Pi")),
        ];
        let focused = pick_agent(&snapshot(base(Some("w1:p2"), agents.clone()))).unwrap();
        assert_eq!(
            focused,
            Agent {
                pane_id: "w1:p2".into(),
                label: "Pi".into()
            }
        );
        // A focused shell falls back to the workspace's agent, never
        // another workspace's.
        let fallback = pick_agent(&snapshot(base(Some("w1:p9"), agents))).unwrap();
        assert_eq!(fallback.pane_id, "w1:p1");
        let unnamed = pick_agent(&snapshot(base(None, vec![agent("w1:p3", "w1", None)])));
        assert_eq!(unnamed.unwrap().label, "the agent");
        assert!(pick_agent(&snapshot(base(None, vec![agent("w0:p1", "w0", None)]))).is_none());
    }
}
