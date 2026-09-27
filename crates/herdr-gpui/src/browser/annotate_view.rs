//! Annotating a page in a window: the picker's reports, the notes panel
//! beside the page, and sending the notes to the agent that opened it. The
//! notes are written in the app, never in the page, and nothing reaches an
//! agent until the user presses Send.
use super::{
    Feedback, Tab, TabId,
    annotate::{self, Anchor, MAX_NOTES, Note, Report},
    feedback::Batch,
};
use crate::{
    HerdrWindow, connection::ConnectionBridge, search_input::SearchInput, terminal::InputTarget,
    window::Flash,
};
use gpui::{prelude::*, *};
use herdr_client::protocol::{
    AgentStatus, ClientKeyCode, ClientKeyKind, ClientPaneInputEvent, ClientShellSnapshot,
};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

/// How long a batch waits for a busy agent before it is pasted anyway, or,
/// when the agent is asking a question, kept for `browser feedback`.
const HOLD: Duration = Duration::from_secs(120);
/// Lets the agent's input take the paste before Enter submits it.
const SUBMIT_DELAY: Duration = Duration::from_millis(150);

#[derive(Default)]
struct TabNotes {
    /// Whether the picker is running in the page.
    armed: bool,
    /// What the next note will be about, picked but not yet written.
    pending: Option<Anchor>,
    notes: Vec<Note>,
}

/// Notes on the way to an agent's pane, waiting for it to be idle.
struct Delivery {
    pane_id: String,
    boot_id: String,
    text: String,
    until: Instant,
}

pub(crate) struct Annotations {
    tabs: HashMap<TabId, TabNotes>,
    pub(super) input: Entity<SearchInput>,
    deliveries: Vec<Delivery>,
}

impl Annotations {
    pub(crate) fn new(cx: &mut App) -> Self {
        let input = cx.new(|cx| {
            let mut input = SearchInput::new(cx);
            input.set_placeholder("Describe the change\u{2026}", cx);
            input
        });
        Self {
            tabs: HashMap::new(),
            input,
            deliveries: Vec::new(),
        }
    }

    pub(crate) fn armed(&self, id: TabId) -> bool {
        self.tabs.get(&id).is_some_and(|tab| tab.armed)
    }

    /// Whether the notes panel shows beside the page.
    pub(crate) fn open(&self, id: TabId) -> bool {
        self.tabs
            .get(&id)
            .is_some_and(|tab| tab.armed || tab.pending.is_some() || !tab.notes.is_empty())
    }

    pub(crate) fn ids(&self) -> impl Iterator<Item = TabId> + '_ {
        self.tabs.keys().copied()
    }

    #[cfg(test)]
    pub(super) fn queued(&self, id: TabId) -> usize {
        self.tabs.get(&id).map_or(0, |tab| tab.notes.len())
    }

    #[cfg(test)]
    pub(super) fn delivering(&self) -> usize {
        self.deliveries.len()
    }

    pub(crate) fn forget(&mut self, id: TabId) {
        self.tabs.remove(&id);
    }
}

/// The agent in `pane_id` and what it is doing, if Herdr sees one there.
fn agent<'a>(
    snapshot: &'a ClientShellSnapshot,
    pane_id: &str,
) -> Option<&'a herdr_client::protocol::ClientShellAgent> {
    snapshot
        .agents
        .iter()
        .find(|agent| agent.pane_id == pane_id)
}

fn enter() -> ClientPaneInputEvent {
    ClientPaneInputEvent::Key {
        code: ClientKeyCode::Enter,
        modifiers: 0,
        kind: ClientKeyKind::Press,
        repeat_count: 1,
        shifted_codepoint: None,
        generated_text: None,
        tracks_release: false,
        physical_key_id: None,
        windows_record: None,
    }
}

impl HerdrWindow {
    fn tab_notes(&mut self, id: TabId) -> &mut TabNotes {
        self.browser.annotations.tabs.entry(id).or_default()
    }

    /// Starts the picker in the page again, drawing the queued notes. A page
    /// loses it whenever it navigates.
    pub(crate) fn arm_page(&mut self, id: TabId, cx: &mut Context<Self>) {
        #[cfg(any(target_os = "macos", windows))]
        {
            let notes = &self.tab_notes(id).notes;
            let script = annotate::arm_script(notes);
            self.browser.pages.script(id, &script, cx);
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        let _ = (id, cx);
    }

    fn disarm_page(&mut self, id: TabId, cx: &mut Context<Self>) {
        #[cfg(any(target_os = "macos", windows))]
        self.browser.pages.script(id, annotate::disarm_script(), cx);
        #[cfg(not(any(target_os = "macos", windows)))]
        let _ = (id, cx);
    }

    pub(crate) fn toggle_annotating(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let notes = self.tab_notes(id);
        notes.armed = !notes.armed;
        if notes.armed {
            self.arm_page(id, cx);
            self.show_flash(
                Flash::success("Click an element or select text to note it"),
                cx,
            );
        } else {
            notes.pending = None;
            self.disarm_page(id, cx);
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    /// The picker lost its page to a navigation; put it back.
    pub(crate) fn page_loaded(&mut self, id: TabId, cx: &mut Context<Self>) {
        if self.browser.annotations.armed(id) {
            self.arm_page(id, cx);
        }
    }

    /// Applies what the picker posted. Only a tab being annotated listens,
    /// and a pick only fills the draft: the user still writes and sends.
    pub(crate) fn page_posted(
        &mut self,
        id: TabId,
        body: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.browser.annotations.armed(id) {
            return;
        }
        match Report::parse(body) {
            Some(Report::Picked(anchor)) => self.begin_note(id, anchor, window, cx),
            Some(Report::Cancelled) => {
                if self.tab_notes(id).pending.take().is_none() {
                    self.toggle_annotating(id, window, cx);
                }
                cx.notify();
            }
            None => tracing::debug!("Ignored a malformed annotation message"),
        }
    }

    fn begin_note(
        &mut self,
        id: TabId,
        anchor: Anchor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tab_notes(id).notes.len() >= MAX_NOTES {
            self.show_flash(
                Flash::warning("Send or remove notes before adding more"),
                cx,
            );
            return;
        }
        self.tab_notes(id).pending = Some(anchor);
        // The page holds the keyboard natively; the note is typed here.
        #[cfg(any(target_os = "macos", windows))]
        self.browser.pages.blur(id, cx);
        let input = self.browser.annotations.input.clone();
        input.update(cx, |input, cx| input.clear(cx));
        let focus = input.read(cx).focus.clone();
        window.focus(&focus, cx);
        cx.notify();
    }

    pub(super) fn add_note(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.browser.annotations.input.read(cx).text().to_owned();
        let Some(anchor) = self.tab_notes(id).pending.clone() else {
            return;
        };
        let Some(note) = Note::new(anchor, &text) else {
            self.show_flash(Flash::warning("Write what should change first"), cx);
            return;
        };
        let notes = self.tab_notes(id);
        notes.pending = None;
        notes.notes.push(note);
        self.browser
            .annotations
            .input
            .update(cx, |input, cx| input.clear(cx));
        self.refresh_markers(id, cx);
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn refresh_markers(&mut self, id: TabId, cx: &mut Context<Self>) {
        if self.browser.annotations.armed(id) {
            self.arm_page(id, cx);
        }
    }

    fn remove_note(&mut self, id: TabId, index: usize, cx: &mut Context<Self>) {
        let notes = self.tab_notes(id);
        if index < notes.notes.len() {
            notes.notes.remove(index);
        }
        self.refresh_markers(id, cx);
        cx.notify();
    }

    fn notes_prompt(&mut self, tab: &Tab) -> Option<String> {
        let notes = &self.tab_notes(tab.id).notes;
        (!notes.is_empty()).then(|| annotate::prompt(tab, notes, &crate::control::reload_command()))
    }

    fn clear_notes(&mut self, id: TabId, cx: &mut Context<Self>) {
        self.tab_notes(id).notes.clear();
        self.refresh_markers(id, cx);
        cx.notify();
    }

    fn copy_notes(&mut self, tab: &Tab, cx: &mut Context<Self>) {
        if let Some(text) = self.notes_prompt(tab) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.show_flash(Flash::success("Notes copied"), cx);
        }
    }

    /// Where Send delivers: the pane of the agent that opened the page,
    /// while this window shows that pane's daemon.
    fn origin_pane<'a>(&'a self, tab: &'a Tab) -> Option<(&'a str, &'a ClientShellSnapshot)> {
        let pane = tab.origin.as_deref()?;
        let snapshot = self.live.snapshot.as_deref()?;
        let here = super::view::scope(&self.endpoints[self.selected_endpoint]) == tab.scope;
        (here
            && snapshot
                .panes
                .iter()
                .any(|candidate| candidate.pane_id == pane))
        .then_some((pane, snapshot))
    }

    /// Sends the queued notes to the agent that opened the page: to it
    /// directly when it waits in `browser feedback`, otherwise into its pane
    /// once it is idle, and kept for `browser feedback` when its pane is gone.
    pub(super) fn send_notes(&mut self, tab: &Tab, cx: &mut Context<Self>) {
        let Some(text) = self.notes_prompt(tab) else {
            return;
        };
        let Some(pane_id) = tab.origin.clone() else {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.clear_notes(tab.id, cx);
            self.show_flash(
                Flash::success("No agent opened this page, so the notes were copied"),
                cx,
            );
            return;
        };
        let waiting = cx
            .try_global::<Feedback>()
            .is_some_and(|feedback| feedback.is_waiting(&pane_id));
        let flash = if waiting {
            cx.default_global::<Feedback>()
                .keep(Batch { pane_id, text });
            Flash::success("Notes sent to the waiting agent")
        } else if let Some((pane, snapshot)) = self
            .origin_pane(tab)
            .filter(|(pane, snapshot)| agent(snapshot, pane).is_some())
        {
            let busy = agent(snapshot, pane).is_some_and(|agent| {
                matches!(
                    agent.agent_status,
                    AgentStatus::Working | AgentStatus::Blocked
                )
            });
            let delivery = Delivery {
                pane_id: pane.to_owned(),
                boot_id: snapshot.boot_id.clone(),
                text,
                until: Instant::now() + HOLD,
            };
            self.browser.annotations.deliveries.push(delivery);
            if busy {
                Flash::success("Notes will go to the agent once it is idle")
            } else {
                Flash::success("Notes sent to the agent")
            }
        } else if self.origin_pane(tab).is_some() {
            // A shell, not an agent: Enter there would run the notes.
            cx.default_global::<Feedback>()
                .keep(Batch { pane_id, text });
            Flash::warning("No agent runs in that pane; notes kept for `browser feedback`")
        } else {
            cx.default_global::<Feedback>()
                .keep(Batch { pane_id, text });
            Flash::warning("The agent's pane is not here; notes kept for `browser feedback`")
        };
        self.clear_notes(tab.id, cx);
        self.show_flash(flash, cx);
    }

    /// Pastes held notes into agents that became idle. Runs every tick.
    pub(crate) fn poll_deliveries(&mut self, cx: &mut Context<Self>) {
        if self.browser.annotations.deliveries.is_empty() {
            return;
        }
        let now = Instant::now();
        let deliveries = std::mem::take(&mut self.browser.annotations.deliveries);
        for delivery in deliveries {
            let waiting = cx
                .try_global::<Feedback>()
                .is_some_and(|feedback| feedback.is_waiting(&delivery.pane_id));
            let snapshot = self.live.snapshot.clone();
            let present = snapshot.as_deref().filter(|snapshot| {
                snapshot.boot_id == delivery.boot_id
                    && snapshot
                        .panes
                        .iter()
                        .any(|pane| pane.pane_id == delivery.pane_id)
            });
            let status = present
                .and_then(|snapshot| agent(snapshot, &delivery.pane_id))
                .map(|agent| agent.agent_status);
            let busy = matches!(status, Some(AgentStatus::Working | AgentStatus::Blocked));
            if present.is_some() && !waiting && busy && now < delivery.until {
                self.browser.annotations.deliveries.push(delivery);
                continue;
            }
            // Only a running agent's prompt is typed into. A pane whose agent
            // exited is a shell, where Enter would run the pasted text, page
            // quotes included; an agent asking the user something must not
            // have its answer typed by a paste. Both fetch the notes instead.
            let typable = matches!(
                status,
                Some(AgentStatus::Idle | AgentStatus::Done | AgentStatus::Working)
            );
            if waiting || present.is_none() || !typable {
                cx.default_global::<Feedback>().keep(Batch {
                    pane_id: delivery.pane_id,
                    text: delivery.text,
                });
                if !waiting {
                    self.show_flash(
                        Flash::warning("The agent is not ready; notes kept for `browser feedback`"),
                        cx,
                    );
                }
                continue;
            }
            self.paste_into_pane(delivery, cx);
        }
    }

    fn paste_into_pane(&mut self, delivery: Delivery, cx: &mut Context<Self>) {
        let target = InputTarget::Pane(delivery.pane_id.clone());
        let pasted = self.endpoints[self.selected_endpoint]
            .connection
            .handle
            .as_ref()
            .ok_or(crate::Error::NotConnected)
            .and_then(|handle| {
                ConnectionBridge::send_input(
                    handle,
                    &delivery.boot_id,
                    &target,
                    ClientPaneInputEvent::Paste(delivery.text.clone()),
                )
                .map_err(crate::Error::from)
            });
        if let Err(error) = pasted {
            tracing::warn!(%error, "Could not paste notes into the agent's pane");
            cx.default_global::<Feedback>().keep(Batch {
                pane_id: delivery.pane_id,
                text: delivery.text,
            });
            self.show_flash(
                Flash::warning("Could not reach the agent; notes kept for `browser feedback`"),
                cx,
            );
            return;
        }
        let timer = cx.background_executor().clone();
        let boot_id = delivery.boot_id;
        cx.spawn(async move |this, cx| {
            timer.timer(SUBMIT_DELAY).await;
            this.update(cx, |this, _| {
                let handle = this.endpoints[this.selected_endpoint]
                    .connection
                    .handle
                    .as_ref();
                if let Some(handle) = handle
                    && let Err(error) =
                        ConnectionBridge::send_input(handle, &boot_id, &target, enter())
                {
                    tracing::warn!(%error, "Could not submit notes in the agent's pane");
                }
            })
            .ok();
        })
        .detach();
    }

    /// The notes panel beside the page.
    pub(crate) fn render_annotations(&mut self, tab: &Tab, cx: &mut Context<Self>) -> AnyElement {
        let id = tab.id;
        let theme = self.theme.clone();
        let armed = self.browser.annotations.armed(id);
        let notes = self.tab_notes(id);
        let pending = notes.pending.clone();
        let list: Vec<(usize, Note)> = notes.notes.iter().cloned().enumerate().collect();
        let origin = tab.origin.is_some();
        let tab_for_send = tab.clone();
        let tab_for_copy = tab.clone();
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
        let composer = pending.map(|anchor| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .border_b_1()
                .border_color(rgb(theme.active))
                .child(
                    div()
                        .text_color(rgb(theme.muted))
                        .truncate()
                        .child(anchor.summary()),
                )
                .child(
                    div()
                        .id("annotation-input")
                        .debug_selector(|| "annotation-input".into())
                        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                            match event.keystroke.key.as_str() {
                                "enter" => this.add_note(id, window, cx),
                                "escape" => {
                                    this.tab_notes(id).pending = None;
                                    window.focus(&this.focus, cx);
                                    cx.notify();
                                }
                                _ => return,
                            }
                            cx.stop_propagation();
                        }))
                        .child(self.browser.annotations.input.clone()),
                )
                .child(div().flex().gap_1().child(
                    button("annotation-add", "Add note", true).on_click(
                        cx.listener(move |this, _, window, cx| this.add_note(id, window, cx)),
                    ),
                ))
        });
        let rows = list.into_iter().map(|(index, note)| {
            div()
                .id(("annotation-note", index))
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
                                .child(note.anchor.summary()),
                        )
                        .child(div().child(note.comment)),
                )
                .child(
                    div()
                        .id(("annotation-remove", index))
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
                            cx.listener(move |this, _, _, cx| this.remove_note(id, index, cx)),
                        ),
                )
        });
        let has_notes = !self.tab_notes(id).notes.is_empty();
        div()
            .id("annotations")
            .debug_selector(|| "annotations".into())
            .flex_none()
            .w(px(300.))
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(theme.surface))
            .border_l_1()
            .border_color(rgb(theme.active))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .p_2()
                    .border_b_1()
                    .border_color(rgb(theme.active))
                    .child("Notes")
                    .child(
                        button("annotation-page", "Note on page", false).on_click(cx.listener(
                            move |this, _, window, cx| this.begin_note(id, Anchor::Page, window, cx),
                        )),
                    ),
            )
            .children(composer)
            .child(
                div()
                    .id("annotation-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(rows)
                    .when(!has_notes && armed, |list| {
                        list.child(
                            div()
                                .p_2()
                                .text_color(rgb(theme.muted))
                                .child("Click an element or select text in the page, then describe the change."),
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
                        .when(origin, |row| {
                            row.child(button("annotation-send", "Send to agent", true).on_click(
                                cx.listener(move |this, _, _, cx| this.send_notes(&tab_for_send, cx)),
                            ))
                        })
                        .child(button("annotation-copy", "Copy", !origin).on_click(cx.listener(
                            move |this, _, _, cx| this.copy_notes(&tab_for_copy, cx),
                        ))),
                )
            })
            .into_any_element()
    }
}
