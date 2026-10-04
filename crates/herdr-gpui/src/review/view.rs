//! The review dialog: the focused checkout's changes, a note composer for
//! the line the user picked, and the queued notes with Send. Git runs on the
//! background executor; nothing reaches an agent until the user presses Send.
use super::{
    diff::{Loaded, RowIndex, Scope, SplitRow},
    notes::{self, MAX_NOTES, Note},
};
use crate::{
    HerdrWindow, fonts::StyledFont, menu::Page, pull_request::Input, search_input::SearchInput,
    window::Flash,
};
use gpui::{prelude::*, *};
use herdr_client::protocol::ClientShellSnapshot;
use std::{collections::HashMap, sync::Arc};

mod rows;
mod scrollbar;

/// How the diff is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Layout {
    /// One column, removed lines above the ones that replaced them.
    #[default]
    Unified,
    /// Before on the left, after on the right.
    Split,
}

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
    /// Which of its changes show; kept between looks.
    scope: Scope,
    /// How they are drawn; kept between looks.
    layout: Layout,
    /// The loaded rows paired for the side-by-side view.
    split: Vec<SplitRow>,
    /// Where the loaded rows are, to mark notes without a scan.
    index: RowIndex,
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
    /// Where on the scrollbar's thumb the pointer took hold of it.
    grab: f32,
    /// Numbers loads, so only the latest one lands.
    request: u64,
}

impl Review {
    fn set_loaded(&mut self, loaded: Loaded) {
        self.split = loaded.diff.split_rows();
        self.index = loaded.diff.index();
        self.state = State::Loaded(Arc::new(loaded));
    }

    /// How many rows the list draws in the current layout.
    fn row_count(&self) -> usize {
        match (self.loaded(), self.layout) {
            (None, _) => 0,
            (Some(loaded), Layout::Unified) => loaded.diff.rows.len(),
            (Some(_), Layout::Split) => self.split.len(),
        }
    }

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
            if let Some(row) = loaded.diff.row_of(&self.index, &note.anchor) {
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
                    scope: Scope::default(),
                    layout: Layout::default(),
                    split: Vec::new(),
                    index: RowIndex::default(),
                    agent: None,
                    endpoint,
                    state: State::Loading,
                    draft: None,
                    notes: Vec::new(),
                    marks: HashMap::new(),
                    input,
                    scroll: UniformListScrollHandle::new(),
                    grab: 0.,
                    request: 0,
                }
            }
        };
        let mut review = Review {
            agent,
            endpoint,
            ..review
        };
        review.refresh_marks();
        let (ui, theme) = (self.config.ui.clone(), self.theme.clone());
        review
            .input
            .update(cx, |input, cx| input.set_appearance(ui, theme, cx));
        self.menu.review = Some(review);
        self.menu.page = Some(Page::Review);
        self.load_review(cx);
    }

    /// Reads the review's changes again in its scope, off the UI thread.
    /// Only the latest read lands.
    fn load_review(&mut self, cx: &mut Context<Self>) {
        // The pull request's base, when GitHub reported one for this branch.
        let base_hint = self.git_pull_request().map(|pr| pr.base_ref_name.clone());
        let Some(review) = self.menu.review.as_mut() else {
            return;
        };
        review.request += 1;
        review.state = State::Loading;
        review.draft = None;
        let (request, checkout, scope) = (review.request, review.checkout.clone(), review.scope);
        let loading = cx
            .background_executor()
            .spawn(async move { super::diff::load(&checkout, scope, base_hint.as_deref()) });
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

    /// Shows uncommitted changes or the whole branch; queued notes stay.
    pub(crate) fn set_review_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        let Some(review) = self.menu.review.as_mut() else {
            return;
        };
        if review.scope == scope {
            return;
        }
        review.scope = scope;
        self.load_review(cx);
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
            scope: loaded.scope,
            layout: Layout::default(),
            split: Vec::new(),
            index: RowIndex::default(),
            agent,
            endpoint: self.selected_endpoint,
            state: State::Loading,
            draft: None,
            notes: Vec::new(),
            marks: HashMap::new(),
            input,
            scroll: UniformListScrollHandle::new(),
            grab: 0.,
            request: 1,
        };
        review.set_loaded(loaded);
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
        match result {
            Ok(loaded) => review.set_loaded(loaded),
            Err(error) => {
                tracing::warn!(%error, "Could not read the changes to review");
                review.state = State::Failed(error.to_string());
            }
        }
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
            .child(self.render_review_scope(review.scope, cx))
            .child(
                div()
                    .debug_selector(|| "review-against".into())
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(rgb(theme.muted))
                    .child(
                        match review.loaded().and_then(|loaded| loaded.base.as_deref()) {
                            Some(base) if review.scope == Scope::Branch => {
                                format!("{} against {base}", review.checkout.branch)
                            }
                            _ => review.checkout.branch.clone(),
                        },
                    ),
            )
            .child(self.render_review_layout(review.layout, cx))
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
                .child(match loaded.scope {
                    Scope::Uncommitted => "No uncommitted changes",
                    Scope::Branch => "No changes on this branch",
                })
                .into_any_element(),
            State::Loaded(loaded) => {
                let count = review.row_count();
                let truncated = loaded.diff.truncated;
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .text_font(&self.config.terminal)
                    .text_size(px(self.config.terminal.size))
                    .child(
                        self.review_scroll_area(
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
                            cx,
                        ),
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

    /// The switch between uncommitted changes and the whole branch.
    fn render_review_scope(&self, current: Scope, cx: &mut Context<Self>) -> Div {
        let theme = &self.theme;
        let segment = |id: &'static str, label: &'static str, scope: Scope| {
            let chosen = scope == current;
            div()
                .id(id)
                .debug_selector(move || id.into())
                .px_2()
                .rounded(px(crate::config::corners::CONTROL))
                .cursor_pointer()
                .when(chosen, |segment| {
                    segment
                        .bg(rgb(theme.active))
                        .text_color(rgb(theme.foreground))
                })
                .when(!chosen, |segment| {
                    segment
                        .text_color(rgb(theme.muted))
                        .hover(|segment| segment.text_color(rgb(theme.foreground)))
                })
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.set_review_scope(scope, cx);
                }))
        };
        div()
            .flex()
            .flex_none()
            .gap_1()
            .child(segment(
                "review-scope-uncommitted",
                "Uncommitted",
                Scope::Uncommitted,
            ))
            .child(segment("review-scope-branch", "Branch", Scope::Branch))
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
        let panel = div()
            .id("review-notes")
            .debug_selector(|| "review-notes".into())
            .flex_none()
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
            });
        self.resizable_notes(
            panel,
            "review-notes-resize",
            crate::panel_resize::PanelDrag::ReviewNotes,
            cx,
        )
    }
}

#[cfg(test)]
mod tests;
