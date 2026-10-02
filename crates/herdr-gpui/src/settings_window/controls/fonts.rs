//! Compact per-role controls and a root-level, transient family chooser.
use super::*;
use crate::{config::FontConfig, fonts::StyledFont};

#[cfg(all(feature = "integration-test", target_os = "macos"))]
#[derive(Default)]
struct NativeBounds(std::collections::HashMap<String, Bounds<Pixels>>);
#[cfg(all(feature = "integration-test", target_os = "macos"))]
impl Global for NativeBounds {}

#[cfg(all(feature = "integration-test", target_os = "macos"))]
fn native_probe(id: impl Into<String>) -> impl IntoElement {
    let id = id.into();
    canvas(
        |_, _, _| (),
        move |bounds, _, _, cx| {
            cx.default_global::<NativeBounds>()
                .0
                .insert(id.clone(), bounds);
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

#[cfg(all(feature = "integration-test", target_os = "macos"))]
impl SettingsWindow {
    pub(in crate::settings_window) fn native_font_bounds(
        &self,
        id: &str,
        cx: &App,
    ) -> Option<Bounds<Pixels>> {
        cx.try_global::<NativeBounds>()?.0.get(id).copied()
    }

    pub(in crate::settings_window) fn native_font_search(
        &self,
        cx: &App,
    ) -> (FocusHandle, String, usize, usize, bool) {
        let input = self.controls.search.read(cx);
        (
            input.focus.clone(),
            input.text().to_owned(),
            self.controls.filtered.len(),
            self.controls.names.len(),
            self.controls.discovering,
        )
    }

    pub(in crate::settings_window) fn native_font_selection(
        &self,
    ) -> (Option<FontTarget>, FontFace, FontConfig) {
        (
            self.controls.picker,
            self.controls.active_face,
            self.control_specimen_font(),
        )
    }

    pub(in crate::settings_window) fn native_font_editor(
        &self,
        cx: &App,
    ) -> Option<(FocusHandle, String)> {
        let input = self.controls.size_editor.as_ref()?.input.read(cx);
        Some((input.focus.clone(), input.text().to_owned()))
    }

    pub(in crate::settings_window) fn native_font_sizes_idle(&self) -> bool {
        self.controls.pending_sizes.is_empty() && self.controls.saving_sizes.is_empty()
    }
}

#[cfg(test)]
type FamilyWriter = dyn Fn(FontTarget, Option<String>) -> crate::Result<()> + Send + Sync;

#[cfg(test)]
#[derive(Clone)]
pub(super) struct FamilyIo {
    write: std::sync::Arc<FamilyWriter>,
    load: std::sync::Arc<dyn Fn() -> crate::Result<super::super::Loaded> + Send + Sync>,
}

fn face_font(config: &Config, face: FontFace) -> &FontConfig {
    match face {
        FontFace::Terminal => &config.terminal,
        FontFace::Sidebar => &config.sidebar,
        FontFace::Tabs => &config.tabs,
        FontFace::Ui => &config.ui,
    }
}

fn family_label(family: &str) -> &str {
    if family == ".SystemUIFont" {
        "System font"
    } else {
        family
    }
}

fn role_label(face: FontFace) -> &'static str {
    match face {
        FontFace::Terminal => "Terminal",
        FontFace::Sidebar => "Sidebar",
        FontFace::Tabs => "Tabs",
        FontFace::Ui => "Interface",
    }
}

impl SettingsWindow {
    pub(in crate::settings_window) fn open_control_font_picker(
        &mut self,
        target: FontTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.finish_control_size_edit(true, cx) {
            self.finish_control_size_edit(false, cx);
        }
        if let FontTarget::Face(face) = target {
            self.controls.active_face = face;
        }
        self.controls.picker = Some(target);
        self.controls.search.update(cx, |input, cx| input.clear(cx));
        self.controls.filtered = filter_fonts(&self.controls.names, "");
        self.controls.selected = 0;
        self.controls.scroll.scroll_to_item(0, ScrollStrategy::Top);
        window.focus(&self.controls.search.read(cx).focus.clone(), cx);
        cx.notify();
    }

    pub(in crate::settings_window) fn dismiss_control_font_picker(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.controls.picker.take().is_some() {
            window.focus(&self.focus, cx);
            cx.notify();
        }
    }

    fn commit_control_family(
        &mut self,
        target: FontTarget,
        family: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Rendered callbacks own their target, never a later row's mutable selection.
        if self.busy() || self.controls.picker != Some(target) {
            return;
        }
        self.dismiss_control_font_picker(window, cx);
        #[cfg(test)]
        if let Some(io) = self.controls.family_io.clone() {
            self.save_with(
                move || (io.write)(target, family),
                move || (io.load)(),
                false,
                cx,
            );
            return;
        }
        self.save_native(
            move || match target {
                FontTarget::All => Config::save_all_font_families(family.as_deref()),
                FontTarget::Face(face) => Config::save_font_family(face, family.as_deref()),
            },
            cx,
        );
    }

    fn control_font_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.controls.picker else {
            return;
        };
        let key = event.keystroke.key.as_str();
        if !matches!(key, "up" | "down" | "enter" | "escape") {
            return;
        }
        cx.stop_propagation();
        if self.controls.search.read(cx).is_composing() {
            return;
        }
        window.prevent_default();
        match key {
            "escape" => self.dismiss_control_font_picker(window, cx),
            "enter" => {
                if let Some(index) = self.controls.filtered.get(self.controls.selected) {
                    self.commit_control_family(
                        target,
                        Some(self.controls.names[*index].clone()),
                        window,
                        cx,
                    );
                }
            }
            _ => {
                self.controls.selected = if key == "up" {
                    self.controls.selected.saturating_sub(1)
                } else {
                    (self.controls.selected + 1).min(self.controls.filtered.len().saturating_sub(1))
                };
                self.controls
                    .scroll
                    .scroll_to_item(self.controls.selected, ScrollStrategy::Top);
                cx.notify();
            }
        }
    }

    fn font_button(&self, id: String, label: impl Into<SharedString>) -> Stateful<Div> {
        let selector = id.clone();
        #[cfg(all(feature = "integration-test", target_os = "macos"))]
        let probe = native_probe(id.clone());
        div()
            .id(id)
            .debug_selector(move || selector.clone())
            .flex()
            .items_center()
            .justify_center()
            .h(px(28.))
            .px(px(8.))
            .min_w_0()
            .rounded(px(corners::CONTROL))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(self.theme.active)))
            .child(label.into())
            .map(|button| {
                #[cfg(all(feature = "integration-test", target_os = "macos"))]
                let button = button.child(probe);
                button
            })
    }

    fn render_font_stepper(&self, face: FontFace, cx: &Context<Self>) -> Div {
        let size = self.controls.size(face, &self.config);
        let mut buttons = div()
            .flex()
            .items_center()
            .flex_none()
            .w(px(104.))
            .h(px(32.))
            .border_1()
            .border_color(rgb(self.theme.active))
            .rounded(px(corners::CONTROL));
        for (symbol, step, enabled) in [("-", -1., size > 8.), ("+", 1., size < 48.)] {
            if step > 0. {
                buttons = buttons.child(match &self.controls.size_editor {
                    Some(editor) if editor.face == face => div()
                        .flex_1()
                        .min_w_0()
                        .on_key_down(cx.listener(Self::control_size_key))
                        .when(editor.invalid, |field| {
                            field.border_b_1().border_color(rgb(self.theme.primary()))
                        })
                        .child(editor.input.clone())
                        .into_any_element(),
                    _ => div()
                        .id(format!("settings-size-value-{}", face.name()))
                        .debug_selector(move || format!("settings-size-value-{}", face.name()))
                        .flex_1()
                        .min_w_0()
                        .text_center()
                        .cursor_pointer()
                        .child(size.to_string())
                        .map(|value| {
                            #[cfg(all(feature = "integration-test", target_os = "macos"))]
                            let value = value.child(native_probe(format!(
                                "settings-size-value-{}",
                                face.name()
                            )));
                            value
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.begin_control_size_edit(face, window, cx);
                        }))
                        .into_any_element(),
                });
            }
            buttons = buttons.child(
                self.font_button(format!("settings-size-{}-{symbol}", face.name()), symbol)
                    .w(px(28.))
                    .px_0()
                    .flex_none()
                    .when(!enabled, |button| button.opacity(0.5))
                    .when(enabled, |button| {
                        button.on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            if !this.finish_control_size_edit(true, cx) {
                                this.finish_control_size_edit(false, cx);
                            }
                            window.focus(&this.focus, cx);
                            this.step_control_size(face, step, cx);
                        }))
                    }),
            );
        }
        #[cfg(all(feature = "integration-test", target_os = "macos"))]
        let buttons = buttons.child(native_probe(format!("settings-size-{}", face.name())));
        buttons
    }

    fn control_specimen_font(&self) -> FontConfig {
        let face = self.controls.active_face;
        let mut font = face_font(&self.config, face).clone();
        font.size = self.controls.size(face, &self.config);
        font
    }

    pub(super) fn render_font_controls(&self, cx: &mut Context<Self>) -> Div {
        #[cfg(all(feature = "integration-test", target_os = "macos"))]
        cx.default_global::<NativeBounds>().0.clear();
        let mut rows = div()
            .debug_selector(|| "settings-font-rows".into())
            .flex()
            .flex_col()
            .min_w_0()
            .gap(px(8.))
            .child(
                div().flex().justify_end().child(
                    self.font_button("settings-font-all".into(), "Set all fonts...")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_control_font_picker(FontTarget::All, window, cx)
                        })),
                ),
            );
        for (face, label) in FACES {
            rows = rows.child(
                div()
                    .id(format!("settings-font-row-{}", face.name()))
                    .map(|row| {
                        #[cfg(all(feature = "integration-test", target_os = "macos"))]
                        let row =
                            row.child(native_probe(format!("settings-font-row-{}", face.name())));
                        row
                    })
                    .debug_selector(move || format!("settings-font-row-{}", face.name()))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(52.))
                    .flex_none()
                    .px(px(8.))
                    .min_w_0()
                    .rounded(px(corners::CONTROL))
                    .border_1()
                    .border_color(rgb(if face == self.controls.active_face {
                        self.theme.primary()
                    } else {
                        self.theme.active
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.controls.active_face = face;
                        cx.notify();
                    }))
                    .child(div().w(px(64.)).flex_none().child(label))
                    .child(
                        self.font_button(format!("settings-font-family-{}", face.name()), "")
                            .flex_1()
                            .justify_between()
                            .gap(px(6.))
                            .border_1()
                            .border_color(rgb(self.theme.active))
                            .bg(rgb(self.theme.background))
                            .child(div().min_w_0().flex_1().truncate().child(
                                family_label(&face_font(&self.config, face).family).to_owned(),
                            ))
                            .child(
                                svg()
                                    .path("icons/chevron-down.svg")
                                    .size(px(12.))
                                    .flex_none()
                                    .text_color(rgb(self.theme.muted)),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.open_control_font_picker(FontTarget::Face(face), window, cx);
                            })),
                    )
                    .child(self.render_font_stepper(face, cx)),
            );
        }
        let font = self.control_specimen_font();
        let sample = match self.controls.active_face {
            FontFace::Terminal => "let answer = 42;\nif answer != 0 {\n    println!(\"ready\");\n}",
            FontFace::Sidebar => {
                "herdr-gpui / workspace\n  claude  Working\n  codex   Ready for review"
            }
            FontFace::Tabs => "main.rs   agent / review\nChanges   Terminal   Preview",
            FontFace::Ui => "Make room for focused work.\nYour preferences, your workspace.",
        };
        rows.child(
            div()
                .debug_selector(|| "settings-font-specimen".into())
                .map(|specimen| {
                    #[cfg(all(feature = "integration-test", target_os = "macos"))]
                    let specimen = specimen.child(native_probe("settings-font-specimen"));
                    specimen
                })
                .mt(px(16.))
                .flex()
                .flex_col()
                .min_w_0()
                .overflow_hidden()
                .gap(px(20.))
                .p(px(20.))
                .rounded(px(corners::CONTROL))
                .bg(rgb(self.theme.surface))
                .child(self.control_note(format!(
                    "{} / {} px",
                    role_label(self.controls.active_face),
                    font.size
                )))
                .child(
                    div()
                        .text_font(&font)
                        .text_size(px(font.size))
                        .line_height(px(font.line_height()))
                        .child(sample),
                )
                .child(
                    div()
                        .text_font(&font)
                        .text_size(px(font.size))
                        .line_height(px(font.line_height()))
                        .child("0O 1lI {} [] != <="),
                ),
        )
    }

    pub(in crate::settings_window) fn render_control_font_picker(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        if self.section != Section::Fonts {
            return None;
        }
        let target = self.controls.picker?;
        let label = match target {
            FontTarget::All => "All roles",
            FontTarget::Face(face) => role_label(face),
        };
        let ready = !self.busy();
        let list_height = (f32::from(window.viewport_size().height) - 340.).clamp(84., 336.);
        Some(
            div()
                .absolute()
                .top(px(36.))
                .bottom(px(36.))
                .left(px(184.))
                .right_0()
                .occlude()
                .bg(rgb(self.theme.background).opacity(0.75))
                .child(
                    div()
                        .id("settings-font-backdrop")
                        .absolute()
                        .inset_0()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.dismiss_control_font_picker(window, cx);
                            }),
                        ),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(64.))
                        .left(px(20.))
                        .right(px(20.))
                        .occlude()
                        .id("settings-font-picker")
                        .debug_selector(|| "settings-font-picker".into())
                        .map(|picker| {
                            #[cfg(all(feature = "integration-test", target_os = "macos"))]
                            let picker = picker.child(native_probe("settings-font-picker"));
                            picker
                        })
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_key_down(cx.listener(Self::control_font_key))
                        .p(px(16.))
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .gap(px(12.))
                        .rounded(px(corners::PANEL))
                        .border_1()
                        .border_color(rgb(self.theme.active))
                        .bg(rgb(self.theme.surface))
                        .shadow_lg()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(format!("Font family / {label}"))
                                .child(
                                    self.font_button("settings-font-picker-close".into(), "Close")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.dismiss_control_font_picker(window, cx)
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .debug_selector(|| "settings-font-search".into())
                                .map(|search| {
                                    #[cfg(all(feature = "integration-test", target_os = "macos"))]
                                    let search = search.child(native_probe("settings-font-search"));
                                    search
                                })
                                .child(self.controls.search.clone()),
                        )
                        .child(self.control_note(if self.controls.discovering {
                            "Loading installed fonts...".into()
                        } else {
                            format!(
                                "{} of {} installed families",
                                self.controls.filtered.len(),
                                self.controls.names.len()
                            )
                        }))
                        .child(
                            uniform_list(
                                "settings-font-results",
                                self.controls.filtered.len(),
                                cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                                    range
                                        .map(|index| {
                                            let family = this.controls.names
                                                [this.controls.filtered[index]]
                                                .clone();
                                            this.font_button(
                                                format!("settings-font-result-{index}"),
                                                "",
                                            )
                                            .w_full()
                                            .h(px(28.))
                                            .justify_start()
                                            .overflow_hidden()
                                            .when(index == this.controls.selected, |row| {
                                                row.bg(rgb(this.theme.active))
                                            })
                                            .when(this.busy(), |row| row.opacity(0.5))
                                            .child(
                                                div()
                                                    .min_w_0()
                                                    .truncate()
                                                    .child(family_label(&family).to_owned()),
                                            )
                                            .on_click(
                                                cx.listener(move |this, _, window, cx| {
                                                    this.commit_control_family(
                                                        target,
                                                        Some(family.clone()),
                                                        window,
                                                        cx,
                                                    );
                                                }),
                                            )
                                        })
                                        .collect()
                                }),
                            )
                            .debug_selector(|| "settings-font-results".into())
                            .h(px(list_height))
                            .track_scroll(&self.controls.scroll)
                            .map(|list| {
                                #[cfg(all(feature = "integration-test", target_os = "macos"))]
                                let list = div()
                                    .h(px(list_height))
                                    .child(native_probe("settings-font-results"))
                                    .child(list);
                                list
                            }),
                        )
                        .when(
                            !self.controls.discovering && self.controls.filtered.is_empty(),
                            |panel| panel.child(self.control_note("No matching fonts.")),
                        )
                        .child(
                            self.font_button(
                                "settings-font-default".into(),
                                format!("Use platform default family / {label}"),
                            )
                            .when(!ready, |button| button.opacity(0.5))
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.commit_control_family(target, None, window, cx)
                                },
                            )),
                        ),
                ),
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use std::sync::{Arc, Mutex};

    const ROWS: [&str; 4] = [
        "settings-font-row-terminal",
        "settings-font-row-sidebar",
        "settings-font-row-tabs",
        "settings-font-row-ui",
    ];
    const FAMILIES: [&str; 4] = [
        "settings-font-family-terminal",
        "settings-font-family-sidebar",
        "settings-font-family-tabs",
        "settings-font-family-ui",
    ];
    const PLUS: [&str; 4] = [
        "settings-size-terminal-+",
        "settings-size-sidebar-+",
        "settings-size-tabs-+",
        "settings-size-ui-+",
    ];

    fn fixture(window: &mut Window, cx: &mut Context<SettingsWindow>) -> SettingsWindow {
        let source = cx.new(|cx| crate::sidebar::layout_tests::fixture_window(window, cx));
        let mut view = SettingsWindow::new(source.downgrade(), cx);
        view.section = Section::Fonts;
        view.controls.names = (0..472).map(|i| format!("Family {i:03}")).collect();
        view.controls.filtered = filter_fonts(&view.controls.names, "");
        view.controls.discovering = false;
        view.sync_controls(cx);
        window.focus(&view.focus, cx);
        view
    }

    fn inside(inner: Bounds<Pixels>, outer: Bounds<Pixels>) {
        assert!(
            inner.left() >= outer.left() && inner.right() <= outer.right(),
            "{inner:?} in {outer:?}"
        );
        assert!(
            inner.top() >= outer.top() && inner.bottom() <= outer.bottom(),
            "{inner:?} in {outer:?}"
        );
    }

    #[gpui::test]
    fn compact_rows_and_unclipped_picker_at_both_window_sizes(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(fixture);
        view.update(cx, |view, cx| {
            view.config.terminal.family = "Very long installed font family ".repeat(12);
            cx.notify();
        });
        for (width, height) in [(960., 780.), (680., 560.)] {
            cx.simulate_resize(size(px(width), px(height)));
            cx.update(|window, cx| window.draw(cx).clear(cx));
            assert!(cx.debug_bounds("settings-font-results").is_none());
            assert!(cx.debug_bounds("settings-font-picker").is_none());
            let body = cx.debug_bounds("settings-body").unwrap();
            let mut previous: Option<Bounds<Pixels>> = None;
            for index in 0..4 {
                let row = cx.debug_bounds(ROWS[index]).unwrap();
                inside(row, body);
                assert_eq!(row.size.height, px(52.));
                if let Some(previous) = previous {
                    assert_eq!(row.top() - previous.bottom(), px(8.));
                }
                previous = Some(row);
                let family = cx.debug_bounds(FAMILIES[index]).unwrap();
                inside(family, row);
                assert!(family.size.width > px(100.));
                inside(cx.debug_bounds(PLUS[index]).unwrap(), row);
            }
            let button = cx.debug_bounds("settings-font-family-terminal").unwrap();
            cx.simulate_click(button.center(), Modifiers::default());
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let picker = cx.debug_bounds("settings-font-picker").unwrap();
            inside(picker, body);
            inside(cx.debug_bounds("settings-font-results").unwrap(), picker);
            assert_eq!(
                cx.debug_bounds("settings-font-result-0")
                    .unwrap()
                    .size
                    .height,
                px(28.)
            );
            assert!(cx.debug_bounds("settings-font-result-471").is_none());
            view.read_with(cx, |view, cx| {
                assert_eq!(view.controls.search.read(cx).text(), "")
            });
            cx.simulate_keystrokes("escape");
        }
    }

    #[gpui::test]
    fn picker_search_keyboard_refresh_and_dismissal_are_local(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(fixture);
        cx.simulate_resize(size(px(680.), px(560.)));
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.open_control_font_picker(FontTarget::Face(FontFace::Sidebar), window, cx);
                assert!(view.controls.search.read(cx).focus.is_focused(window));
            })
        });
        cx.simulate_input("fAmIlY 47");
        cx.run_until_parked();
        cx.simulate_keystrokes("down");
        view.update(cx, |view, cx| {
            assert_eq!(view.controls.filtered, vec![470, 471]);
            assert_eq!(view.controls.selected, 1);
            view.apply_loaded(
                Ok(super::super::super::Loaded {
                    config: view.config.clone(),
                    theme: view.theme.clone(),
                    shared: None,
                    error: None,
                }),
                cx,
            );
            assert_eq!(view.controls.search.read(cx).text(), "fAmIlY 47");
            assert_eq!(view.controls.selected, 1);
            // Browsing remains possible while another root save is pending.
            view.saving = true;
        });
        cx.simulate_keystrokes("enter");
        view.read_with(cx, |view, _| assert!(view.controls.picker.is_some()));
        cx.simulate_keystrokes("up escape");
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                assert!(view.controls.picker.is_none());
                assert!(view.focus.is_focused(window));
                view.open_control_font_picker(FontTarget::All, window, cx);
                assert_eq!(view.controls.filtered.len(), 472);
            })
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        // Outside the panel but inside its body-level backdrop.
        cx.simulate_click(point(px(190.), px(50.)), Modifiers::default());
        view.read_with(cx, |view, _| assert!(view.controls.picker.is_none()));
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.open_control_font_picker(FontTarget::All, window, cx);
                view.select_section(Section::General, window, cx);
                assert!(view.controls.picker.is_none());
                view.saving = false;
            })
        });
    }

    #[gpui::test]
    fn selected_row_specimen_uses_face_and_effective_pending_size(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(fixture);
        cx.simulate_resize(size(px(960.), px(780.)));
        view.update(cx, |view, cx| {
            view.saving = true;
            cx.notify();
        });
        for ((face, _), selector) in FACES.into_iter().zip(ROWS) {
            cx.update(|window, cx| window.draw(cx).clear(cx));
            let row = cx.debug_bounds(selector).unwrap();
            cx.simulate_click(
                point(row.left() + px(20.), row.center().y),
                Modifiers::default(),
            );
            view.update(cx, |view, cx| {
                assert_eq!(view.controls.active_face, face);
                let original = face_font(&view.config, face).clone();
                view.step_control_size(face, 1., cx);
                let specimen = view.control_specimen_font();
                assert_eq!(specimen.family, original.family);
                assert_eq!(specimen.font(), original.font());
                assert_eq!(specimen.size, original.size + 1.);
            });
        }
        view.update(cx, |view, _| {
            view.controls.pending_sizes.clear();
            view.saving = false;
        });
        assert_eq!(family_label(".SystemUIFont"), "System font");
    }

    #[gpui::test]
    fn chooser_composition_neither_navigates_commits_nor_dismisses(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(fixture);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.open_control_font_picker(FontTarget::All, window, cx);
                let search = view.controls.search.clone();
                search.update(cx, |input, cx| {
                    input.replace_and_mark_text_in_range(None, "Family", Some(6..6), window, cx);
                });
                for key in ["down", "up", "enter", "escape"] {
                    view.control_font_key(
                        &KeyDownEvent {
                            keystroke: Keystroke::parse(key).unwrap(),
                            is_held: false,
                            prefer_character_input: false,
                        },
                        window,
                        cx,
                    );
                    assert_eq!(view.controls.selected, 0);
                    assert_eq!(view.controls.picker, Some(FontTarget::All));
                    assert!(!view.busy());
                }
                search.update(cx, |input, cx| input.unmark_text(window, cx));
                view.dismiss_control_font_picker(window, cx);
            });
        });
    }

    #[gpui::test]
    fn family_commits_capture_target_and_only_change_families(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(fixture);
        let config = Arc::new(Mutex::new(
            view.read_with(cx, |view, _| view.config.clone()),
        ));
        let writes = Arc::new(Mutex::new(Vec::new()));
        view.update(cx, |view, _| {
            let state = config.clone();
            let recorded = writes.clone();
            let loaded = config.clone();
            view.controls.family_io = Some(FamilyIo {
                write: Arc::new(move |target, family| {
                    recorded.lock().unwrap().push((target, family.clone()));
                    let mut config = state.lock().unwrap();
                    let defaults = Config::default();
                    for (face, _) in FACES {
                        if target != FontTarget::All && target != FontTarget::Face(face) {
                            continue;
                        }
                        let default = face_font(&defaults, face).family.clone();
                        let font = match face {
                            FontFace::Terminal => &mut config.terminal,
                            FontFace::Sidebar => &mut config.sidebar,
                            FontFace::Tabs => &mut config.tabs,
                            FontFace::Ui => &mut config.ui,
                        };
                        font.family = family.clone().unwrap_or(default);
                    }
                    Ok(())
                }),
                load: Arc::new(move || {
                    Ok(super::super::super::Loaded {
                        config: loaded.lock().unwrap().clone(),
                        theme: Default::default(),
                        shared: None,
                        error: None,
                    })
                }),
            });
        });
        let sizes = view.read_with(cx, |view, _| FACES.map(|(face, _)| face.size(&view.config)));
        for target in FACES
            .map(|(face, _)| FontTarget::Face(face))
            .into_iter()
            .chain([FontTarget::All])
        {
            for family in [Some("Family 471".to_owned()), None] {
                cx.update(|window, cx| {
                    view.update(cx, |view, cx| {
                        view.open_control_font_picker(target, window, cx);
                        view.controls.active_face = FontFace::Ui;
                        if family.is_none() {
                            view.commit_control_family(target, None, window, cx);
                        }
                    })
                });
                if family.is_some() {
                    cx.simulate_input("fAmIlY 471");
                    cx.run_until_parked();
                    cx.simulate_keystrokes("enter");
                }
                cx.update(|window, cx| {
                    view.update(cx, |view, _| {
                        assert!(view.controls.picker.is_none());
                        assert!(view.focus.is_focused(window));
                    })
                });
                cx.run_until_parked();
                assert_eq!(
                    writes.lock().unwrap().last(),
                    Some(&(target, family.clone()))
                );
                view.read_with(cx, |view, _| {
                    assert!(!view.busy());
                    assert_eq!(FACES.map(|(face, _)| face.size(&view.config)), sizes);
                    for (face, _) in FACES {
                        let expected = if family.is_some()
                            && (target == FontTarget::All || target == FontTarget::Face(face))
                        {
                            "Family 471".to_owned()
                        } else {
                            face_font(&Config::default(), face).family.clone()
                        };
                        assert_eq!(face_font(&view.config, face).family, expected);
                    }
                });
            }
        }
        assert_eq!(writes.lock().unwrap().len(), 10);
    }
}
