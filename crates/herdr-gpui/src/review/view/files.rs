//! The review's changed files, listed left of the diff as a pull request's
//! file tree is: grouped under their folders, each with how it changed, its
//! line counts and the notes queued on it. Clicking one brings its header to
//! the top of the diff; the file at the top of the diff is marked.
use super::{Layout, Review};
use crate::{
    HerdrWindow,
    config::Theme,
    panel_resize::PanelDrag,
    review::diff::{Anchor, Diff, FileEntry, SplitRow},
};
use gpui::{prelude::*, *};
use std::collections::HashMap;

/// One line of the file list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum FileItem {
    /// A folder heading, its path from the checkout's root.
    Folder(String),
    /// A file, as an index into the review's file entries.
    File(usize),
}

/// The folder part of `path`, or `None` at the checkout's root.
fn folder(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(folder, _)| folder)
}

/// Groups `entries` under folder headings, in the diff's order. Git lists
/// paths sorted, so a folder's files are together; a root file after a folder
/// gets the root's own heading, so it never reads as part of that folder.
pub(super) fn file_items(diff: &Diff, entries: &[FileEntry]) -> Vec<FileItem> {
    let mut items = Vec::with_capacity(entries.len() * 2);
    let mut current: Option<Option<&str>> = None;
    for (index, entry) in entries.iter().enumerate() {
        let path = diff.files.get(entry.file).map_or("", String::as_str);
        let here = folder(path);
        if current != Some(here) {
            match here {
                Some(folder) => items.push(FileItem::Folder(folder.to_owned())),
                None if current.is_some() => items.push(FileItem::Folder("/".to_owned())),
                None => {}
            }
            current = Some(here);
        }
        items.push(FileItem::File(index));
    }
    items
}

/// The letter a file's change shows as, and its colour.
fn status(theme: &Theme, status: &str) -> (&'static str, u32) {
    match status {
        "new" | "untracked" => ("A", theme.ink(theme.palette[2])),
        "deleted" => ("D", theme.ink(theme.palette[1])),
        "renamed" => ("R", theme.ink(theme.palette[4])),
        "too large to show" => ("!", theme.muted),
        _ => ("M", theme.ink(theme.palette[3])),
    }
}

impl Review {
    /// The list position of the row at the top of the diff.
    fn top_row(&self) -> Option<usize> {
        // Rows are one height, so the scroll offset says which is at the
        // top: the list's content is its rows end to end.
        let position = {
            let state = self.scroll.0.borrow();
            let rows = self.row_count();
            let height = f32::from(state.last_item_size?.contents.height) / rows.max(1) as f32;
            let offset = -f32::from(state.base_handle.offset().y);
            if height <= 0. {
                return None;
            }
            ((offset.max(0.) / height) as usize).min(rows.saturating_sub(1))
        };
        match self.layout {
            Layout::Unified => Some(position),
            Layout::Split => match *self.split.get(position)? {
                SplitRow::Across(row) => Some(row),
                SplitRow::Sides { left, right } => left.or(right),
            },
        }
    }

    /// The file whose rows are at the top of the diff.
    pub(super) fn top_file(&self) -> Option<usize> {
        let loaded = self.loaded()?;
        let row = self.top_row()?;
        loaded.diff.rows.get(row).map(|row| row.file)
    }

    /// Where the header of file entry `index` sits in the list as drawn.
    fn header_position(&self, index: usize) -> Option<usize> {
        let header = self.files.get(index)?.header;
        match self.layout {
            Layout::Unified => Some(header),
            Layout::Split => self
                .split
                .iter()
                .position(|row| *row == SplitRow::Across(header)),
        }
    }
}

impl HerdrWindow {
    /// Brings file entry `index`'s header to the top of the diff.
    pub(super) fn jump_to_review_file(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(review) = self.menu.review.as_ref() else {
            return;
        };
        if let Some(position) = review.header_position(index) {
            review.scroll.scroll_to_item(position, ScrollStrategy::Top);
        }
        cx.notify();
    }

    /// The file list, resizable by its right edge.
    pub(super) fn render_review_files(
        &self,
        review: &Review,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = &self.theme;
        let line_height = self.config.ui.line_height() + 8.;
        let count = review.file_items.len();
        let files = review.files.len();
        let panel = div()
            .id("review-files")
            .debug_selector(|| "review-files".into())
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(rgb(theme.active))
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_color(rgb(theme.muted))
                    .child(if files == 1 {
                        "1 file changed".to_owned()
                    } else {
                        format!("{files} files changed")
                    }),
            )
            .child(
                uniform_list(
                    "review-file-list",
                    count,
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        this.review_file_rows(range, line_height, cx)
                    }),
                )
                .track_scroll(&review.files_scroll)
                .flex_1()
                .min_h_0(),
            );
        self.resizable_panel(panel, "review-files-resize", PanelDrag::ReviewFiles, cx)
    }

    fn review_file_rows(
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
        let diff = &loaded.diff;
        let current = review.top_file();
        let mut notes: HashMap<&str, usize> = HashMap::new();
        for note in &review.notes {
            let (Anchor::File { path } | Anchor::Line { path, .. }) = &note.anchor;
            *notes.entry(path.as_str()).or_default() += 1;
        }
        range
            .filter_map(|position| {
                let row = div()
                    .h(px(line_height))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_1()
                    .whitespace_nowrap()
                    .overflow_hidden();
                Some(match review.file_items.get(position)? {
                    FileItem::Folder(folder) => row
                        .text_color(rgb(theme.muted))
                        .child(div().min_w_0().truncate().child(folder.clone()))
                        .into_any_element(),
                    FileItem::File(index) => {
                        let index = *index;
                        let entry = review.files.get(index)?;
                        let path = diff.files.get(entry.file)?;
                        let name = path.rsplit('/').next().unwrap_or(path).to_owned();
                        let (letter, colour) = status(theme, &entry.status);
                        let noted = notes.get(path.as_str()).copied().unwrap_or(0);
                        let skipped = entry.status == "too large to show";
                        row.id(("review-file", index))
                            .debug_selector(move || format!("review-file-{index}"))
                            .pl(px(if folder(path).is_some() { 18. } else { 8. }))
                            .cursor_pointer()
                            .rounded(px(crate::config::corners::CONTROL))
                            .when(current == Some(entry.file), |row| row.bg(rgb(theme.active)))
                            .hover(|row| row.bg(rgb(theme.active)))
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(12.))
                                    .text_color(rgb(colour))
                                    .child(letter),
                            )
                            .child(div().flex_1().min_w_0().truncate().child(name))
                            .when(noted > 0, |row| {
                                row.child(
                                    div()
                                        .flex_none()
                                        .px_1()
                                        .rounded_full()
                                        .bg(rgb(theme.palette[3]))
                                        .text_color(rgb(theme.text_on(theme.palette[3])))
                                        .text_size(px(10.))
                                        .child(noted.to_string()),
                                )
                            })
                            .when(!skipped, |row| {
                                row.child(
                                    div()
                                        .flex_none()
                                        .flex()
                                        .gap_1()
                                        .text_size(px(11.))
                                        .when(entry.added > 0, |counts| {
                                            counts.child(
                                                div()
                                                    .text_color(rgb(theme.ink(theme.palette[2])))
                                                    .child(format!("+{}", entry.added)),
                                            )
                                        })
                                        .when(entry.removed > 0, |counts| {
                                            counts.child(
                                                div()
                                                    .text_color(rgb(theme.ink(theme.palette[1])))
                                                    .child(format!("\u{2212}{}", entry.removed)),
                                            )
                                        }),
                                )
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.jump_to_review_file(index, cx);
                            }))
                            .into_any_element()
                    }
                })
            })
            .collect()
    }
}
