//! The review dialog: the focused checkout's changes, a note composer for
//! the line the user picked, and the queued notes with Send. Git runs on the
//! background executor; nothing reaches an agent until the user presses Send.
use super::{
    diff::{FileEntry, Loaded, RowIndex, Scope, SplitRow},
    notes::{self, MAX_NOTES, Note},
};
use crate::browser::TabId;
use crate::{
    HerdrWindow, fonts::StyledFont, pull_request::Input, search_input::SearchInput, window::Flash,
};
use gpui::{prelude::*, *};
use herdr_client::protocol::ClientShellSnapshot;
use std::{collections::HashMap, sync::Arc};

mod files;
mod panels;
mod rows;
mod scrollbar;
mod tab;

/// The share of the review's width the file list and the notes may each
/// take, so a review in a narrow group keeps most of its room for the diff.
const PANEL_SHARE: f32 = 0.3;

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
    Some(describe(agent))
}

/// The agent Herdr sees in `pane`, as a review names it.
pub(crate) fn agent_in(snapshot: &ClientShellSnapshot, pane: &str) -> Option<Agent> {
    crate::agent_notes::agent(snapshot, pane).map(describe)
}

fn describe(agent: &herdr_client::protocol::ClientShellAgent) -> Agent {
    let label = [&agent.display_agent, &agent.agent, &agent.name]
        .into_iter()
        .flatten()
        .map(|label| crate::notifications::safe_text(label, 80))
        .find(|label| !label.trim().is_empty())
        .unwrap_or_else(|| "the agent".into());
    Agent {
        pane_id: agent.pane_id.clone(),
        label,
    }
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
    /// The share of a side-by-side row the old side takes.
    split_ratio: f32,
    /// The changed files, and the file list's lines with their folders.
    files: Vec<FileEntry>,
    file_items: Vec<files::FileItem>,
    files_scroll: UniformListScrollHandle,
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
    /// Holds the keyboard while the tab is used.
    focus: FocusHandle,
    /// The file list and the notes: shown or hidden by the user, or `None`
    /// to follow the review's width.
    files_shown: Option<bool>,
    notes_shown: Option<bool>,
    /// The review's width at its last layout, which decides that.
    width: std::rc::Rc<std::cell::Cell<f32>>,
}

impl Review {
    fn set_loaded(&mut self, loaded: Loaded) {
        self.split = loaded.diff.split_rows();
        self.index = loaded.diff.index();
        self.files = loaded.diff.file_entries();
        self.file_items = files::file_items(&loaded.diff, &self.files);
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
    /// Reads the review's changes again in its scope, off the UI thread.
    /// Only the latest read lands.
    fn load_review(&mut self, id: TabId, cx: &mut Context<Self>) {
        // The pull request's base, when GitHub reported one for this branch.
        let base_hint = self.git_pull_request().map(|pr| pr.base_ref_name.clone());
        let Some(review) = self.reviews.get_mut(&id) else {
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
            let plain = result.as_ref().ok().map(|loaded| loaded.diff.clone());
            this.update(cx, |this, cx| {
                this.review_loaded(id, request, result);
                cx.notify();
            })
            .ok();
            // The plain diff shows at once; its colours follow.
            let Some(mut diff) = plain else {
                return;
            };
            let coloured = cx
                .background_executor()
                .spawn(async move {
                    super::highlight::colour(&mut diff);
                    diff
                })
                .await;
            this.update(cx, |this, cx| {
                this.review_coloured(id, request, coloured);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Shows uncommitted changes or the whole branch; queued notes stay.
    pub(crate) fn set_review_scope(&mut self, id: TabId, scope: Scope, cx: &mut Context<Self>) {
        let Some(review) = self.reviews.get_mut(&id) else {
            return;
        };
        if review.scope == scope {
            return;
        }
        review.scope = scope;
        self.load_review(id, cx);
    }

    /// Opens a review tab in workspace `w0` on `loaded`, as if Git had
    /// just read it, shown in the group in use. Its tab.
    #[cfg(test)]
    #[allow(clippy::expect_used)]
    pub(super) fn seed_review(
        &mut self,
        loaded: Loaded,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> TabId {
        let scope = crate::browser::scope(&self.endpoints[self.selected_endpoint]);
        let checkout = crate::browser::ReviewCheckout {
            repo_key: "/work/repo/.git".into(),
            branch: "feature".into(),
            checkout: Some("/work/repo".into()),
        };
        let agent = self.live.snapshot.as_deref().and_then(pick_agent);
        let origin = agent.as_ref().map(|agent| agent.pane_id.clone());
        let input_checkout = Input::from(&checkout);
        let id = crate::browser::Store::update(cx, |store| {
            store.open(
                scope,
                "w0",
                Some(crate::browser::Location::Review { checkout }),
                origin,
            )
        })
        .expect("a tab");
        let input = cx.new(SearchInput::new);
        let mut review = Review::new(
            input_checkout,
            agent,
            self.selected_endpoint,
            input,
            cx.focus_handle(),
        );
        review.scope = loaded.scope;
        review.request = 1;
        review.set_loaded(loaded);
        self.reviews.insert(id, review);
        self.show_browser_tab(id, window, cx);
        id
    }

    fn review_loaded(&mut self, id: TabId, request: u64, result: crate::Result<Loaded>) {
        let Some(review) = self
            .reviews
            .get_mut(&id)
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

    /// Swaps in the coloured rows of load `request`, if it is still the
    /// one shown. The rows are the same, so notes and markers stand.
    fn review_coloured(&mut self, id: TabId, request: u64, diff: super::diff::Diff) {
        let Some(review) = self
            .reviews
            .get_mut(&id)
            .filter(|review| review.request == request)
        else {
            return;
        };
        let Some(loaded) = review.loaded() else {
            return;
        };
        if loaded.diff.rows.len() != diff.rows.len() {
            return;
        }
        review.state = State::Loaded(Arc::new(Loaded {
            checkout: loaded.checkout.clone(),
            scope: loaded.scope,
            base: loaded.base.clone(),
            diff,
        }));
    }

    /// Starts a note on `row`, typed in the composer.
    pub(crate) fn begin_review_note(
        &mut self,
        id: TabId,
        row: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(review) = self.reviews.get_mut(&id) else {
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

    pub(crate) fn add_review_note(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(review) = self.reviews.get_mut(&id) else {
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
        let focus = review.focus.clone();
        window.focus(&focus, cx);
        cx.notify();
    }

    fn cancel_review_note(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(review) = self.reviews.get_mut(&id) {
            review.draft = None;
            let focus = review.focus.clone();
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    fn remove_review_note(&mut self, id: TabId, index: usize, cx: &mut Context<Self>) {
        if let Some(review) = self.reviews.get_mut(&id)
            && index < review.notes.len()
        {
            review.notes.remove(index);
            review.refresh_marks();
        }
        cx.notify();
    }

    /// The queued notes as the agent's prompt, with where they go.
    fn review_prompt(&self, id: TabId) -> Option<(String, Option<String>, bool)> {
        let review = self.reviews.get(&id)?;
        let loaded = review.loaded()?;
        if review.notes.is_empty() {
            return None;
        }
        let text = notes::prompt(&loaded.checkout, &review.notes);
        let pane = review.agent.as_ref().map(|agent| agent.pane_id.clone());
        Some((text, pane, review.endpoint == self.selected_endpoint))
    }

    /// Sends the notes to the agent; the queue is cleared at once so a
    /// second press cannot repeat it. The tab stays open for the next round.
    pub(crate) fn send_review(&mut self, id: TabId, cx: &mut Context<Self>) {
        let Some((text, pane, here)) = self.review_prompt(id) else {
            return;
        };
        if let Some(review) = self.reviews.get_mut(&id) {
            review.notes.clear();
            review.refresh_marks();
        }
        self.deliver_notes(pane, here, text, cx);
        cx.notify();
    }

    fn copy_review(&mut self, id: TabId, cx: &mut Context<Self>) {
        let Some((text, _, _)) = self.review_prompt(id) else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.show_flash(Flash::success("Notes copied"), cx);
    }

    pub(crate) fn render_review(&self, id: TabId, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme.clone();
        let Some(review) = self.reviews.get(&id) else {
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
            .child(self.render_review_panel_toggle(id, review, panels::Panel::Files, cx))
            .child(
                div()
                    .flex_none()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Review changes"),
            )
            .child(self.render_review_scope(id, review.scope, cx))
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
            .child(self.render_review_layout(id, review.layout, cx))
            .child(self.render_review_panel_toggle(id, review, panels::Panel::Notes, cx))
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
                            id,
                            uniform_list(
                                "review-diff",
                                count,
                                cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                                    this.review_rows(id, range, line_height, cx)
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
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .child(panels::measure(review.width.clone()))
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .when(
                        !review.files.is_empty() && review.shows(panels::Panel::Files),
                        |row| row.child(self.render_review_files(id, review, cx)),
                    )
                    .child(div().flex_1().min_w_0().flex().flex_col().child(body))
                    .when(review.shows(panels::Panel::Notes), |row| {
                        row.child(self.render_review_notes(id, review, cx))
                    }),
            )
            .into_any_element()
    }

    /// The switch between uncommitted changes and the whole branch.
    fn render_review_scope(&self, id: TabId, current: Scope, cx: &mut Context<Self>) -> Div {
        let theme = &self.theme;
        let segment = |name: &'static str, label: &'static str, scope: Scope| {
            let chosen = scope == current;
            div()
                .id(name)
                .debug_selector(move || name.into())
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
                    this.set_review_scope(id, scope, cx);
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

    fn render_review_notes(
        &self,
        id: TabId,
        review: &Review,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
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
                        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                            let composing = this
                                .reviews
                                .get(&id)
                                .is_some_and(|review| review.input.read(cx).is_composing());
                            if composing {
                                return;
                            }
                            match event.keystroke.key.as_str() {
                                "enter" => this.add_review_note(id, window, cx),
                                "escape" => this.cancel_review_note(id, window, cx),
                                _ => return,
                            }
                            cx.stop_propagation();
                        }))
                        .child(review.input.clone()),
                )
                .child(
                    div()
                        .flex()
                        .child(button("review-add", "Add note", true).on_click(cx.listener(
                            move |this, _, window, cx| this.add_review_note(id, window, cx),
                        ))),
                )
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
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.remove_review_note(id, index, cx)
                        })),
                )
        });
        let has_notes = !review.notes.is_empty();
        let has_agent = review.agent.is_some();
        let panel =
            div()
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
                                div().p_2().text_color(rgb(theme.muted)).child(
                                    "Click a line or a file name to note what should change.",
                                ),
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
                                    cx.listener(move |this, _, _, cx| this.send_review(id, cx)),
                                ))
                            })
                            .child(button("review-copy", "Copy", !has_agent).on_click(
                                cx.listener(move |this, _, _, cx| this.copy_review(id, cx)),
                            )),
                    )
                });
        self.resizable_panel(
            panel,
            "review-notes-resize",
            crate::panel_resize::PanelDrag::ReviewNotes,
            Some(PANEL_SHARE),
            cx,
        )
    }
}

#[cfg(test)]
mod tests;
