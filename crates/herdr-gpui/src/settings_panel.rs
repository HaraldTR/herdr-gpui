//! Prepared preferences state and background-only configuration operations.
use crate::{
    HerdrWindow,
    config::FontRole,
    fonts::StyledFont,
    herdr_settings::{Edit, IndicatorStyle, Settings, THEME_NAMES, ToastDelivery},
    search_input::SearchInput,
};
use gpui::{prelude::*, *};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Tab {
    #[default]
    Theme,
    Indicators,
    Sound,
    Toasts,
    Integrations,
    Font,
    General,
}

impl Tab {
    pub(crate) const ALL: [Self; 7] = [
        Self::Theme,
        Self::Indicators,
        Self::Sound,
        Self::Toasts,
        Self::Integrations,
        Self::Font,
        Self::General,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Theme => "Theme",
            Self::Indicators => "Indicators",
            Self::Sound => "Sound",
            Self::Toasts => "Toasts",
            Self::Integrations => "Integrations",
            Self::Font => "Font",
            Self::General => "General",
        }
    }

    fn next(self, backwards: bool) -> Self {
        let index = Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0);
        Self::ALL[(index + if backwards { Self::ALL.len() - 1 } else { 1 }) % Self::ALL.len()]
    }
}

#[derive(Default)]
pub(crate) struct SettingsPanel {
    pub(crate) shared: Option<Settings>,
    pub(crate) task: Option<Task<()>>,
    pub(crate) loaded: bool,
    pub(crate) tab: Tab,
    pub(crate) tabs_scroll: ScrollHandle,
    pub(crate) error: Option<String>,
    status: Option<String>,
    load_status: Option<String>,
    pub(crate) native_status: Option<String>,
    pub(crate) native_error: Option<String>,
    pub(crate) native_reloading: bool,
    reload_status: Option<String>,
    native_task: Option<Task<()>>,
    font_inputs: Vec<(FontRole, Entity<SearchInput>, f32)>,
    font_inputs_deferred: bool,
}

impl SettingsPanel {
    fn ready(&self) -> bool {
        cfg!(unix)
            && self.loaded
            && self.shared.is_some()
            && self.task.is_none()
            && self.error.is_none()
    }
}

impl HerdrWindow {
    pub(crate) fn load_shared_settings(&mut self, cx: &mut Context<Self>) {
        self.load_shared_settings_with(Settings::load, cx);
    }

    fn load_shared_settings_with(
        &mut self,
        load: impl FnOnce() -> crate::Result<Settings> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.settings.task.is_some() {
            return;
        }
        self.settings.error = None;
        self.settings.load_status = Some("Loading shared settings...".into());
        let load = cx.background_executor().spawn(async move { load() });
        self.settings.task = Some(cx.spawn(async move |this, cx| {
            let result = load.await;
            let _ = this.update(cx, |this, cx| {
                this.settings.task = None;
                this.settings.loaded = true;
                match result {
                    Ok(shared) => {
                        this.settings.shared = Some(shared);
                        this.settings.load_status = Some("Loaded from local file".into());
                        this.apply_shared_theme(cx);
                        this.reload_notification_config(cx);
                    }
                    Err(error) => {
                        this.settings.load_status = None;
                        this.settings.error = Some(format!("Load shared settings: {error}"));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(crate) fn apply_shared_theme(&mut self, cx: &mut Context<Self>) {
        if self.config.theme == "Follow Herdr"
            && self.menu.page != Some(crate::menu::Page::Themes)
            && !self.theme_save_in_flight()
            && let Some(shared) = &self.settings.shared
        {
            let light = matches!(
                cx.window_appearance(),
                WindowAppearance::Light | WindowAppearance::VibrantLight
            );
            match shared.theme(light) {
                Ok(theme) => {
                    self.theme = theme;
                    if let Some(mut appearance) =
                        cx.try_global::<crate::app::InitialAppearance>().cloned()
                        && appearance.config.theme == "Follow Herdr"
                    {
                        appearance.theme = self.theme.clone();
                        cx.set_global(appearance);
                    }
                    crate::log_window::set_appearance(&self.config, &self.theme, cx);
                }
                Err(error) => self.settings.error = Some(error.to_string()),
            }
        }
    }

    fn save_shared_settings(&mut self, edit: Edit, cx: &mut Context<Self>) {
        if !self.settings.ready() {
            return;
        }
        let Some(shared) = self.settings.shared.clone() else {
            return;
        };
        self.settings.status = Some("Saving local file...".into());
        self.settings.reload_status = None;
        let save = cx
            .background_executor()
            .spawn(async move { shared.save(edit) });
        self.settings.task = Some(cx.spawn(async move |this, cx| {
            let result = save.await;
            let _ = this.update(cx, |this, cx| {
                this.settings.task = None;
                match result {
                    Ok(shared) => {
                        this.settings.shared = Some(shared);
                        this.settings.status = Some("Saved to local file".into());
                        this.apply_shared_theme(cx);
                        this.reload_notification_config(cx);
                        // Queue directly: neither dialog nor integration response slots belong to us.
                        let local = this.endpoints.iter().find(|endpoint| {
                            !matches!(
                                endpoint.connection.target,
                                herdr_client::ConnectTarget::Ssh { .. }
                            )
                        });
                        this.settings.reload_status = Some(match local {
                            Some(endpoint) => match (
                                endpoint.connection.handle.as_ref(),
                                endpoint.live.snapshot.as_ref(),
                            ) {
                                (Some(handle), Some(snapshot))
                                    if endpoint.live.status.is_connected() =>
                                {
                                    match handle.request(
                                        &snapshot.boot_id,
                                        herdr_client::Method::ServerReloadConfig,
                                        serde_json::json!({}),
                                    ) {
                                        Ok(_) => {
                                            "Local daemon reload queued (not acknowledged)".into()
                                        }
                                        Err(error) => {
                                            format!("Local daemon reload not queued: {error}")
                                        }
                                    }
                                }
                                _ => "Local daemon reload not queued: disconnected".into(),
                            },
                            None => "Local daemon reload not queued: no local endpoint".into(),
                        });
                    }
                    Err(error) => {
                        this.settings.status = None;
                        this.settings.error =
                            Some(format!("Save failed; reload before retrying: {error}"));
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(crate) fn select_settings_tab(
        &mut self,
        tab: Tab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.tab = tab;
        if let Some(index) = Tab::ALL.iter().position(|item| *item == tab) {
            self.settings.tabs_scroll.scroll_to_item(index);
        }
        self.menu.preferences_scroll.set_offset(Point::default());
        window.focus(&self.menu.focus);
        if tab == Tab::Integrations {
            self.load_integrations(cx);
        }
        if tab == Tab::Font {
            self.settings.font_inputs_deferred = true;
            self.refresh_deferred_font_inputs(cx);
        }
        cx.notify();
    }

    /// Only a tab entry requests fresh drafts; ordinary saves must retain other role edits.
    pub(crate) fn refresh_deferred_font_inputs(&mut self, cx: &mut Context<Self>) {
        if !self.settings.font_inputs_deferred
            || self.native_settings_save_in_flight()
            || self.config_load.is_some()
        {
            return;
        }
        self.settings.font_inputs_deferred = false;
        self.settings.font_inputs = FontRole::ALL
            .into_iter()
            .map(|role| {
                let font = self.config.font(role);
                let input = cx.new(SearchInput::new);
                input.update(cx, |input, cx| {
                    input.set_text_selected(&font.family, cx);
                    input.set_appearance(self.config.ui.clone(), self.theme.clone(), cx);
                });
                (role, input, font.size)
            })
            .collect();
    }

    /// Return true when the preferences page owns routing, including native IME input.
    pub(crate) fn settings_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.menu.page != Some(crate::menu::Page::Preferences) {
            return false;
        }
        let focused = self.settings.tab == Tab::Font
            && self
                .settings
                .font_inputs
                .iter()
                .any(|(_, input, _)| input.read(cx).focus.is_focused(window));
        if focused {
            if event.keystroke.key == "escape"
                && !self
                    .settings
                    .font_inputs
                    .iter()
                    .any(|(_, input, _)| input.read(cx).is_composing())
            {
                window.focus(&self.menu.focus);
                cx.stop_propagation();
                window.prevent_default();
            }
            return true;
        }
        if event.keystroke.key == "tab" {
            self.select_settings_tab(
                self.settings.tab.next(event.keystroke.modifiers.shift),
                window,
                cx,
            );
            cx.stop_propagation();
            window.prevent_default();
            return true;
        }
        false
    }

    fn save_native_settings(
        &mut self,
        font: Option<(FontRole, String, f32)>,
        cx: &mut Context<Self>,
    ) {
        let config = self.config.clone();
        self.save_native_settings_with(
            move || match font {
                Some((role, family, size)) => config.save_font(role, &family, size),
                None => config.save_theme("Follow Herdr"),
            },
            cx,
        );
    }

    fn save_native_settings_with(
        &mut self,
        save: impl FnOnce() -> crate::Result<()> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.native_settings_save_in_flight()
            || self.config_load.is_some()
            || self.theme_save_in_flight()
            || self.menu.page == Some(crate::menu::Page::Themes)
        {
            return;
        }
        self.settings.native_status = Some("Saving GUI config...".into());
        self.settings.native_error = None;
        let save = cx.background_executor().spawn(async move { save() });
        self.settings.native_task = Some(cx.spawn(async move |this, cx| {
            let result = save.await;
            let _ = this.update(cx, |this, cx| {
                this.settings.native_task = None;
                match result {
                    Ok(()) => {
                        this.settings.native_status =
                            Some("GUI config saved; reloading appearance".into());
                        this.load_gui_config(cx);
                        this.settings.native_reloading = true;
                    }
                    Err(error) => {
                        this.settings.native_status = None;
                        this.settings.native_error = Some(format!("Save GUI config: {error}"));
                        this.refresh_deferred_font_inputs(cx);
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(crate) fn native_settings_save_in_flight(&self) -> bool {
        self.settings.native_task.is_some() || self.settings.native_reloading
    }

    fn settings_button(
        &self,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        selected: bool,
        enabled: bool,
    ) -> Stateful<Div> {
        let id = id.into();
        let label: SharedString = label.into();
        div()
            .id(id.clone())
            .debug_selector(move || id.to_string())
            .px(px(8.))
            .py(px(5.))
            .rounded(px(4.))
            .border_1()
            .border_color(rgb(self.theme.active))
            .bg(rgb(if selected {
                self.theme.active
            } else {
                self.theme.background
            }))
            .when(enabled, |button| {
                button
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(self.theme.active)))
            })
            .when(!enabled, |button| button.opacity(0.5))
            .child(label)
    }

    pub(crate) fn render_preferences(&self, cx: &mut Context<Self>) -> Div {
        let tab = self.settings.tab;
        let ready = self.settings.ready();
        let native_ready = !self.native_settings_save_in_flight()
            && self.config_load.is_none()
            && !self.theme_save_in_flight();
        let mut body = div()
            .id("preferences-body")
            .debug_selector(|| "preferences-body".into())
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_y_scroll()
            .track_scroll(&self.menu.preferences_scroll)
            .p(px(12.));
        if cfg!(windows) && matches!(tab, Tab::Theme | Tab::Indicators | Tab::Sound | Tab::Toasts) {
            body = body.child(div().pb(px(8.)).child("Shared Herdr settings are read-only on Windows. Native fonts and theme overrides remain editable."));
        }
        match tab {
            Tab::Theme => {
                body = body.child(div().debug_selector(|| "preferences-theme".into()).py(px(8.)).child(format!("GUI theme: {}", self.config.theme)))
                    .child(div().flex().flex_wrap().gap(px(6.))
                        .child(self.settings_button("preferences-choose-theme", "Native override...", false, native_ready).on_click(cx.listener(|this, _, window, cx| {
                            if !this.native_settings_save_in_flight() && this.config_load.is_none() && !this.theme_save_in_flight() { this.open_theme_picker(window, cx); }
                        })))
                        .child(self.settings_button("preferences-follow-herdr", "Follow Herdr", self.config.theme == "Follow Herdr", native_ready).on_click(cx.listener(|this, _, _, cx| this.save_native_settings(None, cx)))))
                    .child(div().py(px(8.)).child("Choosing a shared theme preserves your native override. Follow Herdr to use it in the GUI."));
                let mut choices = div().flex().flex_wrap().gap(px(6.));
                for &name in THEME_NAMES {
                    choices = choices.child(
                        self.settings_button(
                            format!("shared-theme-{name}"),
                            name,
                            self.settings
                                .shared
                                .as_ref()
                                .is_some_and(|s| s.theme_name == name),
                            ready,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.save_shared_settings(Edit::Theme(name.into()), cx)
                        })),
                    );
                }
                body = body.child(choices);
            }
            Tab::Indicators => {
                use herdr_client::protocol::AgentStatus;

                body = body.child(div().py(px(8.)).child("Agent status indicators"));
                for (label, style) in [
                    ("Dots", IndicatorStyle::Dots),
                    ("Symbols", IndicatorStyle::Symbols),
                ] {
                    body = body.child(
                        self.settings_button(
                            format!("indicators-{label}"),
                            label,
                            self.settings
                                .shared
                                .as_ref()
                                .is_some_and(|s| s.indicators == style),
                            ready,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.save_shared_settings(Edit::Indicators(style), cx)
                        })),
                    );
                    if let Some(shared) = &self.settings.shared {
                        let light = matches!(
                            cx.window_appearance(),
                            WindowAppearance::Light | WindowAppearance::VibrantLight
                        );
                        let mut preview = div()
                            .debug_selector(move || format!("indicators-preview-{label}"))
                            .flex()
                            .flex_wrap()
                            .gap(px(12.))
                            .py(px(10.));
                        for (status, name, symbol) in [
                            (AgentStatus::Working, "Working", "\u{25d0}"),
                            (AgentStatus::Blocked, "Blocked", "\u{d7}"),
                            (AgentStatus::Done, "Done", "\u{2713}"),
                            (AgentStatus::Idle, "Idle", "\u{25cb}"),
                            (AgentStatus::Unknown, "Unknown", "\u{b7}"),
                        ] {
                            let color = rgb(shared.status_color(status, light));
                            let mark = div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .w(px(self.config.ui.size))
                                .h(px(self.config.ui.line_height()))
                                .when(style == IndicatorStyle::Symbols, |mark| {
                                    mark.text_color(color).child(symbol)
                                })
                                .when(style == IndicatorStyle::Dots, |mark| {
                                    mark.child(
                                        div()
                                            .size(px(if status == AgentStatus::Unknown {
                                                3.
                                            } else {
                                                7.
                                            }))
                                            .rounded_full()
                                            .border_1()
                                            .border_color(color)
                                            .when(status != AgentStatus::Idle, |dot| dot.bg(color)),
                                    )
                                });
                            preview = preview.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(4.))
                                    .child(mark)
                                    .child(name),
                            );
                        }
                        body = body.child(preview);
                    }
                }
            }
            Tab::Sound => {
                body = body.child(div().py(px(8.)).child("Agent sounds use the dedicated audio backend with shared sound paths and per-agent overrides. Missing or unusable custom sounds fall back to bundled Done/Request sounds. Playback requires an available audio device. No preview is played automatically."));
                for (label, enabled) in [("On", true), ("Off", false)] {
                    body = body.child(
                        self.settings_button(
                            format!("sound-{label}"),
                            label,
                            self.settings
                                .shared
                                .as_ref()
                                .is_some_and(|s| s.sound_enabled == enabled),
                            ready,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.save_shared_settings(Edit::Sound(enabled), cx)
                        })),
                    );
                }
                body = body.child(
                    div().py(px(8.)).child(
                        self.settings_button("sound-preview", "Play test sound (QA)", false, true)
                            .on_click(cx.listener(|this, _, _, _| this.sound.preview())),
                    ),
                );
            }
            Tab::Toasts => {
                body = body.child(div().py(px(8.)).child("Shared delivery settings also apply to other Herdr clients. This GUI uses in-app toasts; it does not deliver terminal or OS notifications. Native [notifications] overrides take precedence. QA previews work even when delivery is disabled."))
                    .child(div().py(px(8.)).child(format!(
                        "Effective in-app toasts: {} | Delay: {} seconds | Corner: {:?}",
                        if self.config.notifications.enabled { "On" } else { "Off" },
                        self.config.notifications.delay_seconds,
                        self.config.notifications.position,
                    )));
                for (label, delivery) in [
                    ("Off", ToastDelivery::Off),
                    ("Herdr", ToastDelivery::Herdr),
                    ("Terminal", ToastDelivery::Terminal),
                    ("System", ToastDelivery::System),
                ] {
                    body = body.child(
                        self.settings_button(
                            format!("toasts-{label}"),
                            label,
                            self.settings
                                .shared
                                .as_ref()
                                .is_some_and(|s| s.toast_delivery == delivery),
                            ready,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.save_shared_settings(Edit::Toasts(delivery), cx)
                        })),
                    );
                }
            }
            Tab::Font => {
                body = body.child(div().pb(px(8.)).child("Native GUI fonts. Sizes are logical pixels (8-48). Save each role to apply; configured fallbacks are preserved."));
                for (role, input, size) in &self.settings.font_inputs {
                    let role = *role;
                    let input = input.clone();
                    let mut row = div()
                        .debug_selector(move || format!("preferences-font-{}", role.key()))
                        .py(px(8.))
                        .child(role.label())
                        .child(
                            div()
                                .debug_selector(move || format!("font-input-{}", role.key()))
                                .min_w_0()
                                .when(native_ready, |row| row.child(input.clone()))
                                .when(!native_ready, |row| {
                                    row.opacity(0.5).child(input.read(cx).text().to_owned())
                                }),
                        );
                    let mut buttons = div().flex().flex_wrap().gap(px(6.)).items_center();
                    for (label, delta) in [("-", -1.), ("+", 1.)] {
                        buttons = buttons.child(
                            self.settings_button(
                                format!("font-{}-{label}", role.key()),
                                label,
                                false,
                                native_ready,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if !this.native_settings_save_in_flight()
                                        && this.config_load.is_none()
                                        && !this.theme_save_in_flight()
                                        && let Some((_, _, size)) = this
                                            .settings
                                            .font_inputs
                                            .iter_mut()
                                            .find(|(r, _, _)| *r == role)
                                    {
                                        *size = (*size + delta).clamp(8., 48.);
                                        cx.notify();
                                    }
                                },
                            )),
                        );
                    }
                    let size = *size;
                    buttons = buttons.child(format!("{size} px")).child(
                        self.settings_button(
                            format!("font-{}-save", role.key()),
                            "Save",
                            false,
                            native_ready,
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                window.focus(&this.menu.focus);
                                this.save_native_settings(
                                    Some((role, input.read(cx).text().trim().into(), size)),
                                    cx,
                                );
                            },
                        )),
                    );
                    row = row.child(buttons);
                    body = body.child(row);
                }
            }
            Tab::Integrations | Tab::General => {}
        }
        let content = match tab {
            Tab::General => self.render_general_preferences(cx).into_any_element(),
            Tab::Integrations => self.render_integrations(cx).into_any_element(),
            _ => body.into_any_element(),
        };
        let mut tabs = div()
            .id("preferences-tabs")
            .debug_selector(|| "preferences-tabs".into())
            .flex()
            .flex_none()
            .min_w_0()
            .overflow_x_scroll()
            .track_scroll(&self.settings.tabs_scroll)
            .gap(px(4.))
            .px(px(12.))
            .pb(px(8.));
        for tab in Tab::ALL {
            tabs =
                tabs.child(
                    self.settings_button(
                        format!("preferences-tab-{}", tab.label()),
                        tab.label(),
                        self.settings.tab == tab,
                        true,
                    )
                    .flex_none()
                    .whitespace_nowrap()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_settings_tab(tab, window, cx)
                    })),
                );
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .min_h_0()
            .min_w_0()
            .text_font(&self.config.ui)
            .text_size(px(self.config.ui.size))
            .text_color(rgb(self.theme.foreground))
            .child(
                div()
                    .debug_selector(|| "preferences-header".into())
                    .flex()
                    .items_center()
                    .flex_none()
                    .p(px(12.))
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Preferences"),
                    )
                    .child(
                        self.settings_button("preferences-close", "Close", false, true)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.dismiss_menu(window, cx)),
                            ),
                    ),
            )
            .child(tabs)
            .child(content)
            .child(
                div()
                    .id("preferences-status")
                    .debug_selector(|| "preferences-footer".into())
                    .flex_none()
                    .max_h(px(80.))
                    .overflow_y_scroll()
                    .px(px(12.))
                    .py(px(8.))
                    .text_size(px(self.config.ui.size * 0.85))
                    .when_some(self.settings.status.clone(), |footer, text| {
                        footer.child(text)
                    })
                    .when_some(self.settings.load_status.clone(), |footer, text| {
                        footer.child(div().child(text))
                    })
                    .when_some(self.settings.native_status.clone(), |footer, text| {
                        footer.child(div().child(text))
                    })
                    .when_some(self.settings.native_error.clone(), |footer, text| {
                        footer.child(div().child(text))
                    })
                    .when_some(self.settings.reload_status.clone(), |footer, text| {
                        footer.child(div().child(text))
                    })
                    .when_some(self.settings.error.clone(), |footer, text| {
                        footer.child(div().child(text))
                    })
                    .child(div().child("Tab / Shift-Tab: sections   Esc: close")),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[core::prelude::v1::test]
    fn tabs_cycle_in_both_directions() {
        for (index, tab) in Tab::ALL.into_iter().enumerate() {
            assert_eq!(tab.next(false), Tab::ALL[(index + 1) % Tab::ALL.len()]);
            assert_eq!(tab.next(true).next(false), tab);
        }
        assert_eq!(Tab::General.next(false), Tab::Theme);
        assert_eq!(Tab::Theme.next(true), Tab::General);
    }

    #[gpui::test]
    fn general_retains_layout_summary_and_sound_uses_explicit_preview(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
        cx.simulate_resize(size(px(800.), px(600.)));
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.open_menu(window, cx);
                view.menu.page = Some(crate::menu::Page::Preferences);
                view.config.layout.mode = crate::config::LayoutMode::Compact;
                view.config.layout.sidebar_gap = 16.;
                view.select_settings_tab(Tab::General, window, cx);
            });
            window.draw(cx).clear();
        });
        for selector in ["preferences-layout", "preferences-sidebar-gap"] {
            assert!(cx.debug_bounds(selector).is_some(), "{selector}");
        }
        assert!(cx.debug_bounds("sound-preview").is_none());
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.select_settings_tab(Tab::Sound, window, cx);
            });
            window.draw(cx).clear();
        });
        assert!(cx.debug_bounds("sound-preview").is_some());
        assert!(cx.debug_bounds("preferences-layout").is_none());
        assert!(cx.debug_bounds("preferences-shared-path").is_none());
        assert!(cx.debug_bounds("preferences-reload-shared").is_none());
    }

    #[gpui::test]
    #[allow(clippy::unwrap_used)]
    fn followed_theme_updates_startup_cache_without_persisting_session_font_size(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
        let shared = Settings::parse_text("[theme]\nname = 'nord'").unwrap();
        let expected = shared.theme(false).unwrap();
        view.update(cx, |view, cx| {
            view.settings.shared = Some(shared);
            view.load_gui_config_with(
                || {
                    Ok((
                        crate::config::Config {
                            theme: "Follow Herdr".into(),
                            ..Default::default()
                        },
                        Default::default(),
                    ))
                },
                cx,
            );
        });
        cx.run_until_parked();
        view.read_with(cx, |view, cx| {
            assert_eq!(view.theme, expected);
            assert_eq!(cx.global::<crate::app::InitialAppearance>().theme, expected);
        });
        let shared = Settings::parse_text("[theme]\nname = 'dracula'").unwrap();
        let expected = shared.theme(false).unwrap();
        view.update(cx, |view, cx| {
            let saved_size = cx
                .global::<crate::app::InitialAppearance>()
                .config
                .terminal
                .size;
            view.config.terminal.size = 28.;
            view.settings.shared = Some(shared);
            view.apply_shared_theme(cx);
            let appearance = cx.global::<crate::app::InitialAppearance>();
            assert_eq!(appearance.theme, expected);
            assert_eq!(appearance.config.terminal.size, saved_size);
        });
    }

    #[gpui::test]
    fn failed_load_is_bounded_and_keeps_native_appearance(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
        view.update(cx, |view, cx| {
            view.load_shared_settings_with(|| Err(crate::Error::MissingHome), cx);
            view.load_shared_settings_with(|| panic!("only one shared load at a time"), cx);
            assert!(view.settings.task.is_some());
            assert!(!view.settings.ready());
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.settings.loaded);
            assert!(view.settings.task.is_none());
            assert!(view.settings.error.is_some());
            assert!(!view.settings.ready());
            assert!(view.settings.status.is_none());
            assert_eq!(view.config.theme, "Default");
        });
    }

    #[gpui::test]
    fn native_edits_serialize_and_do_not_clear_shared_save_status(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.settings.status = Some("Saved to local file".into());
                view.settings.error = Some("shared conflict".into());
                view.save_native_settings_with(|| Err(crate::Error::MissingHome), cx);
                view.save_native_settings_with(|| panic!("second native write started"), cx);
                view.load_gui_config(cx);
                view.open_theme_picker(window, cx);
                assert!(view.native_settings_save_in_flight());
                assert!(view.config_load.is_none());
                assert!(view.menu.page != Some(crate::menu::Page::Themes));
            })
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(!view.native_settings_save_in_flight());
            assert!(view.settings.native_error.is_some());
            assert_eq!(view.settings.status.as_deref(), Some("Saved to local file"));
            assert_eq!(view.settings.error.as_deref(), Some("shared conflict"));
        });
    }

    #[gpui::test]
    fn shared_reload_keeps_saved_status(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
        view.update(cx, |view, cx| {
            view.settings.status = Some("Saved to local file".into());
            view.settings.native_status = Some("GUI config saved and applied".into());
            view.load_shared_settings_with(|| Err(crate::Error::MissingHome), cx);
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.settings.status.as_deref(), Some("Saved to local file"));
            assert_eq!(
                view.settings.native_status.as_deref(),
                Some("GUI config saved and applied")
            );
        });
    }

    #[gpui::test]
    fn font_tab_entry_waits_for_startup_or_post_save_config(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
        for reenter in [false, true] {
            cx.update(|window, cx| {
                view.update(cx, |view, cx| {
                    if reenter {
                        view.select_settings_tab(Tab::Font, window, cx);
                        view.select_settings_tab(Tab::General, window, cx);
                        // Hold a native write slot before its synthetic post-save reload.
                        view.settings.native_task = Some(Task::ready(()));
                        view.select_settings_tab(Tab::Font, window, cx);
                        assert!(view.settings.font_inputs_deferred);
                        view.settings.native_task = None;
                    }
                    let mut config = view.config.clone();
                    config.sidebar.family = if reenter {
                        "Saved Sidebar"
                    } else {
                        "Loaded Sidebar"
                    }
                    .into();
                    config.sidebar.size = if reenter { 23. } else { 19. };
                    view.load_gui_config_with(move || Ok((config, Default::default())), cx);
                    view.settings.native_reloading = reenter;
                    // Completion cannot run until this UI update returns. No disk writes are needed.
                    view.select_settings_tab(Tab::Font, window, cx);
                    assert!(view.config_load.is_some());
                    assert!(view.settings.font_inputs_deferred);
                })
            });
            cx.run_until_parked();
            view.read_with(cx, |view, cx| {
                assert!(!view.settings.font_inputs_deferred);
                assert!(!view.native_settings_save_in_flight());
                assert!(view.config_load.is_none());
                let (_, input, size) = &view.settings.font_inputs[0];
                assert_eq!(
                    input.read(cx).text(),
                    if reenter {
                        "Saved Sidebar"
                    } else {
                        "Loaded Sidebar"
                    }
                );
                assert_eq!(*size, if reenter { 23. } else { 19. });
            });
        }
    }

    #[gpui::test]
    fn config_completion_preserves_font_drafts_without_tab_reentry(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(crate::sidebar::layout_tests::fixture_window);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.select_settings_tab(Tab::Font, window, cx);
                let (_, input, size) = &mut view.settings.font_inputs[0];
                input.update(cx, |input, cx| {
                    input.set_text_selected("Unsaved family", cx)
                });
                *size = 27.;
                view.load_gui_config_with(|| Ok((Default::default(), Default::default())), cx);
                view.settings.native_reloading = true;
                assert!(!view.settings.font_inputs_deferred);
            })
        });
        cx.run_until_parked();
        view.read_with(cx, |view, cx| {
            let (_, input, size) = &view.settings.font_inputs[0];
            assert_eq!(input.read(cx).text(), "Unsaved family");
            assert_eq!(*size, 27.);
            assert!(!view.settings.font_inputs_deferred);
        });
    }

    #[gpui::test]
    fn preferences_font_editor_is_not_interactive_during_native_reload(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(|window, cx| {
            crate::bind_keys(cx);
            crate::sidebar::layout_tests::fixture_window(window, cx)
        });
        cx.simulate_resize(size(px(800.), px(600.)));
        let original = cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.open_menu(window, cx);
                view.menu.page = Some(crate::menu::Page::Preferences);
                view.select_settings_tab(Tab::Font, window, cx);
                view.settings.native_reloading = true;
                view.settings.font_inputs[0].1.read(cx).text().to_owned()
            })
        });
        cx.update(|window, cx| window.draw(cx).clear());
        let Some(input) = cx.debug_bounds("font-input-sidebar") else {
            panic!("font editor");
        };
        cx.simulate_click(input.center(), Default::default());
        cx.simulate_keystrokes("cmd-a b a d");
        view.read_with(cx, |view, cx| {
            assert_eq!(view.settings.font_inputs[0].1.read(cx).text(), original);
        });
        cx.simulate_keystrokes("tab");
        view.read_with(cx, |view, _| assert_eq!(view.settings.tab, Tab::General));
    }
}
