//! Search-only samples and explicit app-wide theme intents. Disk work stays off the UI thread.
use super::SettingsWindow;
use crate::{
    config::{Config, Theme, corners},
    contrast::Contrast,
    fonts::StyledFont,
    herdr_settings::{self, Edit},
    search_input::{Changed, SearchInput},
};
use gpui::{prelude::*, *};

const FOLLOW: &str = "Follow Herdr";
const LIST_HEIGHT: f32 = 168.;
const ROW_HEIGHT: f32 = 28.;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Scope {
    #[default]
    App,
    Herdr,
}

impl Scope {
    fn editable(self) -> bool {
        self == Self::App || cfg!(unix)
    }
}

#[derive(Clone)]
pub(super) struct ThemeIntent {
    pub(super) revision: u64,
    choice: Choice,
    theme: Option<Theme>,
}

#[derive(Default)]
pub(super) struct ThemeDraft {
    active: bool,
    revision: u64,
    // Store unadjusted colors so a concurrent contrast edit is applied once.
    appearance: Option<(String, Theme)>,
}
impl Global for ThemeDraft {}

pub(crate) fn theme_pending(cx: &App) -> bool {
    cx.try_global::<ThemeDraft>()
        .is_some_and(|draft| draft.active)
}

pub(crate) fn apply_theme_draft(config: &mut Config, theme: &mut Theme, cx: &App) {
    if !theme_pending(cx) {
        return;
    }
    if let Some((name, raw)) = cx
        .try_global::<ThemeDraft>()
        .and_then(|draft| draft.appearance.as_ref())
    {
        config.theme = name.clone();
        *theme = raw.clone().with_contrast(config.contrast);
    }
}

pub(crate) fn theme_load_revision(cx: &App) -> u64 {
    cx.try_global::<ThemeDraft>()
        .map_or(0, |draft| draft.revision)
}

pub(crate) fn apply_loaded_theme(config: &mut Config, theme: &mut Theme, revision: u64, cx: &App) {
    if theme_pending(cx) {
        apply_theme_draft(config, theme, cx);
    } else if revision != theme_load_revision(cx)
        && let Some((name, raw)) = cx
            .try_global::<ThemeDraft>()
            .and_then(|draft| draft.appearance.as_ref())
    {
        // This read began before the final draft was committed. Its other
        // fields remain usable, but its older theme must not replace the commit.
        config.theme = name.clone();
        *theme = raw.clone().with_contrast(config.contrast);
    }
}

pub(super) fn clear_theme_draft(cx: &mut App) {
    let draft = cx.default_global::<ThemeDraft>();
    draft.active = false;
    draft.revision = draft.revision.wrapping_add(1);
}

#[cfg(test)]
#[derive(Clone)]
pub(super) struct ThemeIo {
    pub write: std::sync::Arc<ThemeWriter>,
    pub load: std::sync::Arc<dyn Fn() -> crate::Result<super::Loaded> + Send + Sync>,
    pub resolve: Option<std::sync::Arc<ThemeResolver>>,
}

#[cfg(test)]
type ThemeResolver = dyn Fn(&str) -> crate::Result<Theme> + Send + Sync;

#[cfg(test)]
type ThemeWriter =
    dyn Fn(String, Option<herdr_settings::Settings>) -> crate::Result<()> + Send + Sync;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Choice {
    pub scope: Scope,
    pub name: String,
}

pub(super) struct ThemeBrowser {
    search: Entity<SearchInput>,
    _subscription: Subscription,
    names: Vec<String>,
    filtered: Vec<String>,
    scope: Scope,
    query: String,
    selected: Option<usize>,
    scroll: UniformListScrollHandle,
    initialized: bool,
    discovering: bool,
    catalog_error: Option<String>,
    revision: u64,
    running: Option<u64>,
    attempted: Option<u64>,
    loaded: Option<u64>,
    preview: Option<Theme>,
    preview_name: Option<String>,
    preview_error: Option<String>,
    contrast: Contrast,
}

impl ThemeBrowser {
    pub(super) fn new(cx: &mut Context<SettingsWindow>) -> Self {
        let search = cx.new(SearchInput::new);
        let subscription = cx.subscribe(&search, |this, search, _: &Changed, cx| {
            let query = search.read(cx).text().to_owned();
            if query == this.themes.query {
                return;
            }
            this.themes.query = query;
            this.themes.filter(None);
            this.request_theme_preview(cx);
        });
        Self {
            search,
            _subscription: subscription,
            names: Vec::new(),
            filtered: Vec::new(),
            scope: Scope::App,
            query: String::new(),
            selected: None,
            scroll: UniformListScrollHandle::new(),
            initialized: false,
            discovering: false,
            catalog_error: None,
            revision: 0,
            running: None,
            attempted: None,
            loaded: None,
            preview: None,
            preview_name: None,
            preview_error: None,
            contrast: Contrast::Standard,
        }
    }

    fn choice(&self) -> Option<Choice> {
        Some(Choice {
            scope: self.scope,
            name: self.filtered.get(self.selected?)?.clone(),
        })
    }

    fn filter(&mut self, preserve: Option<&str>) {
        self.filtered = match self.scope {
            Scope::App => filter_names(self.names.iter().map(String::as_str), &self.query),
            Scope::Herdr => filter_names(herdr_settings::THEME_NAMES.iter().copied(), &self.query),
        };
        self.selected = preserve
            .and_then(|name| self.filtered.iter().position(|item| item == name))
            .or_else(|| (!self.filtered.is_empty()).then_some(0));
        self.scroll
            .scroll_to_item(self.selected.unwrap_or(0), ScrollStrategy::Top);
    }

    fn finish_preview(&mut self, revision: u64, name: String, result: crate::Result<Theme>) {
        if self.running != Some(revision) {
            return;
        }
        self.running = None;
        if revision != self.revision {
            return;
        }
        match result {
            Ok(theme) => {
                self.preview = Some(theme);
                self.preview_name = Some(name);
                self.loaded = Some(revision);
                self.preview_error = None;
            }
            Err(error) => self.preview_error = Some(error.to_string()),
        }
    }
}

fn filter_names<'a>(names: impl IntoIterator<Item = &'a str>, query: &str) -> Vec<String> {
    let query = query.to_lowercase();
    let tokens: Vec<_> = query.split_whitespace().collect();
    names
        .into_iter()
        .filter(|name| {
            let name = name.to_lowercase();
            tokens.iter().all(|token| name.contains(token))
        })
        .map(str::to_owned)
        .collect()
}

impl SettingsWindow {
    fn theme_operation(
        &self,
        intent: &ThemeIntent,
    ) -> impl FnOnce(Option<herdr_settings::Settings>) -> crate::Result<()> + Send + 'static + use<>
    {
        let choice = intent.choice.clone();
        let config = self.config.clone();
        let shared = self.shared.clone();
        #[cfg(test)]
        let io = self.theme_io.clone();
        move |refreshed| {
            let shared = match choice.scope {
                Scope::App => None,
                Scope::Herdr => refreshed.or(shared),
            };
            #[cfg(test)]
            if let Some(io) = io {
                return (io.write)(choice.name, shared);
            }
            match choice.scope {
                Scope::App => config.save_theme(&choice.name),
                Scope::Herdr => shared
                    .ok_or(crate::Error::MissingHome)?
                    .save(Edit::Theme(choice.name))
                    .map(|_| ()),
            }
        }
    }

    pub(super) fn take_shutdown_theme(
        &mut self,
    ) -> Option<
        impl FnOnce(Option<herdr_settings::Settings>) -> crate::Result<()> + Send + 'static + use<>,
    > {
        let intent = self.theme_intent.take()?;
        if self.theme_saving {
            return None;
        }
        let validate = self.theme_loader(intent.choice.clone());
        let write = self.theme_operation(&intent);
        Some(move |shared| {
            if intent.theme.is_none() {
                validate()?;
            }
            write(shared)
        })
    }

    fn theme_loader(
        &self,
        choice: Choice,
    ) -> impl FnOnce() -> crate::Result<Theme> + Send + 'static + use<> {
        let mut config = self.config.clone();
        config.theme = choice.name;
        config.contrast = Contrast::Standard;
        let shared = self.shared.clone();
        let light = self.theme_light;
        #[cfg(test)]
        let resolve = self.theme_io.as_ref().and_then(|io| io.resolve.clone());
        move || {
            #[cfg(test)]
            if let Some(resolve) = resolve {
                return resolve(&config.theme);
            }
            match (choice.scope, shared) {
                (Scope::Herdr, Some(shared)) => shared
                    .preview_theme(&config.theme, light)
                    .map(|theme| theme.with_contrast(config.contrast)),
                (Scope::Herdr, None) => Err(crate::Error::MissingHome),
                (Scope::App, Some(shared)) if config.theme == FOLLOW => shared.theme(light),
                (Scope::App, None) if config.theme == FOLLOW => Err(crate::Error::MissingHome),
                _ => config.theme(),
            }
        }
    }

    pub(super) fn accept_theme_choice(&mut self, choice: Choice, cx: &mut Context<Self>) {
        if self.quitting || self.closing.is_some() {
            return;
        }
        if !choice.scope.editable() {
            self.status = Some("Shared Herdr themes are read-only on this platform. Choose This app or Follow Herdr instead.".into());
            cx.notify();
            return;
        }
        self.theme_load_failed = false;
        self.error = None;
        self.status = Some("Theme draft; saved when Settings closes".into());
        self.theme_revision = self.theme_revision.wrapping_add(1);
        self.theme_light = matches!(
            cx.window_appearance(),
            WindowAppearance::Light | WindowAppearance::VibrantLight
        );
        let theme = match (choice.scope, self.shared.as_ref()) {
            (Scope::Herdr, Some(shared)) => {
                shared.preview_theme(&choice.name, self.theme_light).ok()
            }
            (Scope::App, Some(shared)) if choice.name == FOLLOW => {
                shared.theme(self.theme_light).ok()
            }
            (Scope::App, _) => Theme::builtin(&choice.name).or_else(|| {
                self.theme_cache
                    .iter()
                    .find(|(name, _)| *name == choice.name)
                    .map(|(_, theme)| theme.clone())
            }),
            _ => None,
        };
        self.theme_intent = Some(ThemeIntent {
            revision: self.theme_revision,
            choice,
            theme,
        });
        let draft = cx.default_global::<ThemeDraft>();
        if !draft.active {
            draft.appearance = None;
        }
        draft.active = true;
        draft.revision = draft.revision.wrapping_add(1);
        self.drive_theme_intent(cx);
    }

    pub(super) fn drive_theme_intent(&mut self, cx: &mut Context<Self>) {
        if self.quitting || self.theme_load_failed {
            return;
        }
        if let Some(intent) = &mut self.theme_intent
            && intent.choice.scope == Scope::App
            && intent.choice.name == FOLLOW
            && let Some(shared) = &self.shared
        {
            let light = matches!(
                cx.window_appearance(),
                WindowAppearance::Light | WindowAppearance::VibrantLight
            );
            if let Ok(theme) = shared.theme(light) {
                intent.theme = Some(theme);
            }
        }
        let Some(intent) = self.theme_intent.clone() else {
            self.finish_close(cx);
            return;
        };
        if intent.theme.is_none() {
            if self.theme_loading {
                return;
            }
            self.theme_loading = true;
            let load = self.theme_loader(intent.choice.clone());
            let retained = cx.entity();
            let work = cx.background_executor().spawn(async move { load() });
            cx.spawn(async move |_, cx| {
                let result = work.await;
                retained.update(cx, |this, cx| {
                    this.theme_loading = false;
                    if this.quitting {
                        return;
                    }
                    if let Some(current) = &mut this.theme_intent
                        && current.revision == intent.revision
                    {
                        match result {
                            Ok(theme) => {
                                if intent.choice.scope == Scope::App && intent.choice.name != FOLLOW
                                {
                                    if this.theme_cache.len() == 64 {
                                        this.theme_cache.pop_front();
                                    }
                                    this.theme_cache
                                        .push_back((intent.choice.name, theme.clone()));
                                }
                                current.theme = Some(theme);
                            }
                            Err(error) => {
                                this.theme_load_failed = true;
                                this.closing = None;
                                this.error = Some(format!("Load theme: {error}"));
                            }
                        }
                    }
                    this.drive_theme_intent(cx);
                    cx.notify();
                });
            })
            .detach();
            return;
        }
        // A main picker at its commit boundary must reconcile before we replace it.
        if cx.windows().into_iter().any(|window| {
            window
                .downcast::<crate::HerdrWindow>()
                .is_some_and(|window| {
                    window
                        .read(cx)
                        .is_ok_and(|view| view.theme_save_in_flight())
                })
        }) {
            if self.theme_waiting {
                return;
            }
            self.theme_waiting = true;
            let retained = cx.entity();
            cx.spawn(async move |_, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(10))
                    .await;
                retained.update(cx, |this, cx| {
                    this.theme_waiting = false;
                    this.drive_theme_intent(cx);
                });
            })
            .detach();
            return;
        }
        if let Some(theme) = intent.theme {
            if intent.choice.scope == Scope::App {
                self.config.theme = intent.choice.name.clone();
                cx.default_global::<ThemeDraft>().appearance =
                    Some((self.config.theme.clone(), theme.clone()));
                self.theme = theme.with_contrast(self.config.contrast);
                self.broadcast_theme(cx);
            } else if self.config.theme == FOLLOW {
                cx.default_global::<ThemeDraft>().appearance =
                    Some((self.config.theme.clone(), theme.clone()));
                self.theme = theme.with_contrast(self.config.contrast);
                self.broadcast_theme(cx);
            }
        }
        if self.closing.is_none() || self.busy() || self.theme_saving {
            return;
        }
        let Some(intent) = self.theme_intent.as_ref() else {
            return;
        };
        let operation = self.theme_operation(intent);
        let operation = move || operation(None);
        let shared = intent.choice.scope == Scope::Herdr;
        self.theme_saving = true;
        #[cfg(test)]
        if let Some(io) = self.theme_io.clone() {
            self.save_with(operation, move || (io.load)(), shared, cx);
            return;
        }
        self.save_with(operation, Self::loader(cx), shared, cx);
    }

    pub(super) fn broadcast_theme(&mut self, cx: &mut Context<Self>) {
        for handle in cx.windows() {
            if let Some(handle) = handle.downcast::<crate::HerdrWindow>() {
                let _ = handle.update(cx, |view, window, cx| {
                    if view.theme_save_in_flight() {
                        return;
                    }
                    if view.menu.page == Some(crate::menu::Page::Themes) {
                        view.dismiss_menu(window, cx);
                    }
                    view.config.theme = self.config.theme.clone();
                    view.config.contrast = self.config.contrast;
                    view.theme = self.theme.clone();
                    if !theme_pending(cx) {
                        view.settings.shared = self.shared.clone();
                    }
                    cx.notify();
                });
            }
        }
        let mut appearance = cx
            .try_global::<crate::app::InitialAppearance>()
            .cloned()
            .unwrap_or_default();
        appearance.config.theme = self.config.theme.clone();
        appearance.config.contrast = self.config.contrast;
        appearance.theme = self.theme.clone();
        crate::log_window::set_appearance(&appearance.config, &appearance.theme, cx);
        cx.set_global(appearance);
        self.themes.search.update(cx, |input, cx| {
            input.set_appearance(self.config.ui.clone(), self.theme.clone(), cx)
        });
        cx.notify();
    }

    #[cfg(all(feature = "integration-test", target_os = "macos"))]
    pub(super) fn native_theme_search(&self, cx: &App) -> (FocusHandle, usize, bool) {
        (
            self.themes.search.read(cx).focus.clone(),
            self.themes.filtered.len(),
            self.themes.discovering
                || self.themes.running.is_some()
                || self.theme_loading
                || self.theme_waiting
                || self.saving,
        )
    }

    pub(super) fn initialize_theme_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.themes.initialized {
            return;
        }
        self.themes.initialized = true;
        self.themes.contrast = self.config.contrast;
        self.themes.preview = Some(self.theme.clone());
        self.themes.preview_name = Some(self.config.theme.clone());
        self.themes.names = Theme::BUILTIN_NAMES
            .iter()
            .map(|name| (*name).into())
            .collect();
        self.themes.names.push(self.config.theme.clone());
        self.themes.names.retain(|name| name != FOLLOW);
        self.themes.names.sort();
        self.themes.names.dedup();
        self.themes.filter(Some(&self.config.theme));
        self.themes.search.update(cx, |search, cx| {
            search.set_appearance(self.config.ui.clone(), self.theme.clone(), cx);
            window.focus(&search.focus, cx);
        });
        self.discover_settings_themes(cx);
        self.request_theme_preview(cx);
    }

    pub(super) fn sync_theme_browser(&mut self, cx: &mut Context<Self>) {
        self.themes.contrast = self.config.contrast;
        if self.config.theme != FOLLOW && !self.themes.names.contains(&self.config.theme) {
            self.themes.names.push(self.config.theme.clone());
            self.themes
                .names
                .sort_by_cached_key(|name| (name.to_lowercase(), name.clone()));
        }
        let selected = self.themes.choice();
        self.themes
            .filter(selected.as_ref().map(|choice| choice.name.as_str()));
        self.themes.search.update(cx, |search, cx| {
            search.set_appearance(self.config.ui.clone(), self.theme.clone(), cx);
        });
        self.request_theme_preview(cx);
    }

    fn discover_settings_themes(&mut self, cx: &mut Context<Self>) {
        if self.themes.discovering {
            return;
        }
        self.themes.discovering = true;
        self.themes.catalog_error = None;
        let config = self.config.clone();
        let task = cx
            .background_executor()
            .spawn(async move { config.available_themes() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.themes.discovering = false;
                match result {
                    Ok(mut names) => {
                        if this.config.theme != FOLLOW && !names.contains(&this.config.theme) {
                            names.push(this.config.theme.clone());
                        }
                        names.retain(|name| name != FOLLOW);
                        names.sort_by_cached_key(|name| (name.to_lowercase(), name.clone()));
                        names.dedup();
                        let selected = this.themes.choice();
                        this.themes.names = names;
                        this.themes
                            .filter(selected.as_ref().map(|choice| choice.name.as_str()));
                        this.request_theme_preview(cx);
                    }
                    Err(error) => this.themes.catalog_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn request_theme_preview(&mut self, cx: &mut Context<Self>) {
        self.themes.revision += 1;
        self.themes.loaded = None;
        self.themes.preview_error = None;
        self.drive_theme_preview(cx);
        cx.notify();
    }

    fn drive_theme_preview(&mut self, cx: &mut Context<Self>) {
        let browser = &mut self.themes;
        if browser.running.is_some() || browser.attempted == Some(browser.revision) {
            return;
        }
        let Some(choice) = browser.choice() else {
            return;
        };
        let revision = browser.revision;
        browser.attempted = Some(revision);
        let contrast = browser.contrast;
        let light = matches!(
            cx.window_appearance(),
            WindowAppearance::Light | WindowAppearance::VibrantLight
        );
        if choice.scope == Scope::Herdr && self.shared.is_none() {
            browser.preview_error = Some(
                "Herdr settings are unavailable. Reload settings to preview shared themes.".into(),
            );
            return;
        }
        browser.running = Some(revision);
        let mut config = self.config.clone();
        config.theme = choice.name.clone();
        config.contrast = contrast;
        let shared = self.shared.clone();
        let task = cx.background_executor().spawn(async move {
            // Config::theme already applies contrast; do not apply it twice.
            match (choice.scope, shared) {
                (Scope::Herdr, Some(shared)) => shared
                    .preview_theme(&config.theme, light)
                    .map(|theme| theme.with_contrast(contrast)),
                _ => config.theme(),
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.themes.finish_preview(revision, choice.name, result);
                this.drive_theme_preview(cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn select_settings_theme(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.quitting || self.closing.is_some() || index >= self.themes.filtered.len() {
            return;
        }
        self.themes.selected = Some(index);
        self.themes
            .scroll
            .scroll_to_item(index, ScrollStrategy::Center);
        self.request_theme_preview(cx);
        if let Some(choice) = self.themes.choice() {
            self.accept_theme_choice(choice, cx);
        }
    }

    fn theme_browser_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.themes.search.read(cx).is_composing() {
            // Do not let the window's Escape action close during IME composition.
            if event.keystroke.key == "escape" {
                cx.stop_propagation();
            }
            return;
        }
        match event.keystroke.key.as_str() {
            "escape" => {
                cx.stop_propagation();
                window.prevent_default();
                self.themes.search.update(cx, |search, cx| search.clear(cx));
            }
            "up" | "down" => {
                cx.stop_propagation();
                window.prevent_default();
                let count = self.themes.filtered.len();
                if count > 0 {
                    let index = self.themes.selected.unwrap_or(0);
                    let next = (index
                        + if event.keystroke.key == "up" {
                            count - 1
                        } else {
                            1
                        })
                        % count;
                    self.select_settings_theme(next, cx);
                }
            }
            "enter" => {
                cx.stop_propagation();
                window.prevent_default();
                if let Some(index) = self.themes.selected {
                    self.select_settings_theme(index, cx);
                }
            }
            _ => {}
        }
    }

    fn theme_preview(&self) -> Div {
        let theme = self.themes.preview.as_ref().unwrap_or(&self.theme);
        div()
            .debug_selector(|| "settings-theme-preview".into())
            .w_full()
            .h(px(225.))
            .flex_none()
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(px(corners::PANEL))
            .border_1()
            .border_color(rgb(self.theme.active))
            .bg(rgb(theme.background))
            .text_color(rgb(theme.foreground))
            .child(
                div()
                    .h(px(29.))
                    .flex_none()
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .bg(rgb(theme.surface))
                    .children([1, 3, 2].map(|i| {
                        div()
                            .size(px(6.))
                            .rounded_full()
                            .bg(rgb(theme.ink(theme.palette[i])))
                    }))
                    .child(
                        div()
                            .flex_1()
                            .text_center()
                            .text_size(px(10.))
                            .child("herdr-gpui / preview"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .debug_selector(|| "settings-theme-preview-sidebar".into())
                            .w(relative(0.28))
                            .min_w(px(90.))
                            .flex_none()
                            .p(px(10.))
                            .bg(rgb(theme.surface))
                            .overflow_hidden()
                            .text_size(px(11.))
                            .child(div().text_color(rgb(theme.subtext())).child("SPACES"))
                            .child(
                                div()
                                    .mt(px(10.))
                                    .p(px(6.))
                                    .rounded(px(corners::SMALL))
                                    .bg(rgb(theme.active))
                                    .truncate()
                                    .child("herdr-gpui"),
                            )
                            .child(div().mt(px(8.)).p(px(6.)).truncate().child("website"))
                            .child(
                                div()
                                    .mt(px(14.))
                                    .text_color(rgb(theme.subtext()))
                                    .child("AGENTS"),
                            )
                            .child(
                                div()
                                    .mt(px(8.))
                                    .text_color(rgb(theme.ink(theme.palette[3])))
                                    .child("Claude / working"),
                            ),
                    )
                    .child(
                        div()
                            .debug_selector(|| "settings-theme-preview-terminal".into())
                            .flex_1()
                            .min_w_0()
                            .p(px(14.))
                            .overflow_hidden()
                            .text_font(&self.config.terminal)
                            .text_size(px(self.config.terminal.size))
                            .line_height(px(self.config.terminal.line_height()))
                            .child(
                                div()
                                    .text_color(rgb(theme.ink(theme.palette[2])))
                                    .child("~/code/herdr-gpui"),
                            )
                            .child(div().child("$ cargo test --workspace"))
                            .child(
                                div()
                                    .mt(px(12.))
                                    .text_color(rgb(theme.subtext()))
                                    .child("Running tests..."),
                            )
                            .child(
                                div()
                                    .text_color(rgb(theme.ink(theme.palette[2])))
                                    .child("test result: ok."),
                            )
                            .child(
                                div()
                                    .mt(px(12.))
                                    .flex()
                                    .items_center()
                                    .gap(px(7.))
                                    .child("$")
                                    .child(div().w(px(7.)).h(px(14.)).bg(rgb(theme.cursor))),
                            ),
                    ),
            )
    }

    pub(super) fn render_appearance(&self, cx: &mut Context<Self>) -> Div {
        let browser = &self.themes;
        let theme = &self.theme;
        let count = if browser.scope == Scope::App {
            browser.names.len()
        } else {
            herdr_settings::THEME_NAMES.len()
        };
        let button = |id: &'static str, label: String, selected: bool| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .px(px(10.))
                .py(px(6.))
                .rounded(px(corners::CONTROL))
                .border_1()
                .border_color(rgb(if selected {
                    theme.primary()
                } else {
                    theme.active
                }))
                .bg(rgb(if selected {
                    theme.primary_wash()
                } else {
                    theme.surface
                }))
                .cursor_pointer()
                .child(label)
        };
        div()
            .debug_selector(|| "settings-appearance".into())
            .w_full()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(self.theme_preview())
            .child(
                div()
                    .debug_selector(|| "settings-theme-preview-status".into())
                    .text_size(px(11.))
                    .text_color(rgb(theme.subtext()))
                    .child(format!(
                        "Preview: {}{}",
                        browser
                            .preview_name
                            .as_deref()
                            .unwrap_or("Current appearance"),
                        if browser.running.is_some() {
                            " / loading selection..."
                        } else {
                            " / sample content"
                        }
                    )),
            )
            .child(
                div().flex().flex_wrap().gap(px(8.)).children(
                    [
                        (Scope::App, "This app", "theme-scope-app"),
                        (Scope::Herdr, if Scope::Herdr.editable() { "Herdr" } else { "Herdr (read-only)" }, "theme-scope-herdr"),
                    ]
                    .map(|(scope, label, id)| {
                        button(id, label.into(), browser.scope == scope).on_click(cx.listener(
                            move |this, _, window, cx| {
                                let focus = this.themes.search.read(cx).focus.clone();
                                window.focus(&focus, cx);
                                if this.themes.scope == scope {
                                    return;
                                }
                                this.themes.scope = scope;
                                let saved = match scope {
                                    Scope::App => Some(this.config.theme.clone()),
                                    Scope::Herdr => {
                                        this.shared.as_ref().map(|shared| shared.theme_name.clone())
                                    }
                                };
                                this.themes.filter(saved.as_deref());
                                this.request_theme_preview(cx);
                            },
                        ))
                    }),
                ),
            )
            .child(
                div()
                    .debug_selector(|| "settings-theme-scope-description".into())
                    .text_size(px(11.))
                    .text_color(rgb(theme.subtext()))
                    .child(match browser.scope {
                        Scope::App => {
                            "THIS APP / Full installed library. Applies only to this app."
                        }
                        Scope::Herdr if !Scope::Herdr.editable() => {
                            "HERDR / Read-only on this platform. Preview shared themes here; choose This app or Follow Herdr to change appearance."
                        }
                        Scope::Herdr => {
                            "HERDR / Shared themes. This app changes only when following Herdr."
                        }
                    }),
            )
            .child(
                div()
                    .debug_selector(|| "settings-theme-search".into())
                    .h(px(32.))
                    .flex_none()
                    .on_key_down(cx.listener(Self::theme_browser_key))
                    .child(browser.search.clone()),
            )
            .child(
                div()
                    .debug_selector(|| "settings-theme-count".into())
                    .text_size(px(11.))
                    .text_color(rgb(theme.subtext()))
                    .child(format!(
                        "{} of {} themes{}",
                        browser.filtered.len(),
                        count,
                        if browser.discovering {
                            " / discovering installed themes..."
                        } else {
                            ""
                        }
                    )),
            )
            .child(
                div()
                    .debug_selector(|| "settings-theme-list".into())
                    .h(px(LIST_HEIGHT))
                    .flex_none()
                    .overflow_hidden()
                    .border_1()
                    .border_color(rgb(theme.active))
                    .rounded(px(corners::CONTROL))
                    .when(browser.filtered.is_empty(), |el| {
                        el.child(
                            div()
                                .debug_selector(|| "settings-theme-empty".into())
                                .p(px(12.))
                                .child("No matching themes. Try a shorter search."),
                        )
                    })
                    .when(!browser.filtered.is_empty(), |el| {
                        el.child(
                            uniform_list(
                                "settings-theme-results",
                                browser.filtered.len(),
                                cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                                    range
                                        .map(|index| {
                                            let name = this.themes.filtered[index].clone();
                                            let draft =
                                                this.theme_intent.as_ref().is_some_and(|intent| {
                                                    intent.choice.scope == this.themes.scope
                                                        && intent.choice.name == name
                                                });
                                            let saved = match this.themes.scope {
                                                Scope::App => name == this.config.theme,
                                                Scope::Herdr => {
                                                    this.shared.as_ref().is_some_and(|shared| {
                                                        shared.theme_name == name
                                                    })
                                                }
                                            };
                                            let source = match this.themes.scope {
                                                Scope::Herdr => "Herdr",
                                                Scope::App
                                                    if Theme::BUILTIN_NAMES
                                                        .contains(&name.as_str()) =>
                                                {
                                                    "Built-in"
                                                }
                                                Scope::App
                                                    if std::path::Path::new(&name)
                                                        .is_absolute()
                                                        || name.starts_with("~/") =>
                                                {
                                                    "File"
                                                }
                                                Scope::App => "Ghostty",
                                            };
                                            div()
                                                .id(index)
                                                .debug_selector(move || {
                                                    format!("settings-theme-row-{index}")
                                                })
                                                .h(px(ROW_HEIGHT))
                                                .w_full()
                                                .px(px(10.))
                                                .flex()
                                                .items_center()
                                                .gap(px(8.))
                                                .cursor_pointer()
                                                .when(this.themes.selected == Some(index), |el| {
                                                    el.bg(rgb(this.theme.active))
                                                })
                                                .hover(|el| el.bg(rgb(this.theme.active)))
                                                .child(
                                                    div()
                                                        .debug_selector(move || {
                                                            format!("settings-theme-name-{index}")
                                                        })
                                                        .flex_1()
                                                        .min_w_0()
                                                        .truncate()
                                                        .child(name),
                                                )
                                                .when(saved || draft, |el| {
                                                    el.child(
                                                        div()
                                                            .debug_selector(move || {
                                                                format!(
                                                                    "settings-theme-saved-{index}"
                                                                )
                                                            })
                                                            .text_size(px(10.))
                                                            .child(if draft {
                                                                "Draft"
                                                            } else {
                                                                "Saved"
                                                            }),
                                                    )
                                                })
                                                .child(
                                                    div()
                                                        .debug_selector(move || {
                                                            format!("settings-theme-source-{index}")
                                                        })
                                                        .text_size(px(10.))
                                                        .text_color(rgb(this.theme.subtext()))
                                                        .child(source),
                                                )
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        let focus = this
                                                            .themes
                                                            .search
                                                            .read(cx)
                                                            .focus
                                                            .clone();
                                                        window.focus(&focus, cx);
                                                        this.select_settings_theme(index, cx);
                                                    },
                                                ))
                                        })
                                        .collect()
                                }),
                            )
                            .track_scroll(&browser.scroll)
                            .h_full()
                            .w_full(),
                        )
                    }),
            )
            .when_some(browser.catalog_error.as_ref(), |el, error| {
                el.child(
                    div()
                        .debug_selector(|| "settings-theme-catalog-error".into())
                        .child(format!(
                            "Catalog discovery failed: {error}. Built-ins remain available."
                        ))
                        .child(
                            button("theme-retry-catalog", "Retry discovery".into(), false)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.discover_settings_themes(cx)),
                                ),
                        ),
                )
            })
            .when_some(browser.preview_error.as_ref(), |el, error| {
                el.child(
                    div()
                        .debug_selector(|| "settings-theme-preview-error".into())
                        .child(format!(
                            "Preview unavailable: {error}. Keeping the last valid preview."
                        ))
                        .child(
                            button("theme-retry-preview", "Retry preview".into(), false).on_click(
                                cx.listener(|this, _, _, cx| this.request_theme_preview(cx)),
                            ),
                        ),
                )
            })
            .child(
                div()
                    .debug_selector(|| "settings-theme-actions".into())
                    .flex()
                    .flex_wrap()
                    .gap(px(8.))
                    .child(
                        button(
                            "theme-follow",
                            if self.config.theme == FOLLOW {
                                "Following Herdr"
                            } else {
                                "Follow Herdr"
                            }
                            .into(),
                            self.config.theme == FOLLOW,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.accept_theme_choice(
                                Choice {
                                    scope: Scope::App,
                                    name: FOLLOW.into(),
                                },
                                cx,
                            );
                        })),
                    )
                    .child(
                        button(
                            "theme-contrast",
                            "High contrast".into(),
                            self.config.contrast == Contrast::High,
                        )
                        .opacity(if self.busy() { 0.5 } else { 1. })
                        .on_click(cx.listener(|this, _, _, cx| {
                            if this.busy() {
                                return;
                            }
                            let contrast = if this.config.contrast == Contrast::High {
                                Contrast::Standard
                            } else {
                                Contrast::High
                            };
                            this.save_native(move || Config::save_contrast(contrast), cx);
                        })),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn shared_scope_editability_matches_platform_support() {
        assert!(Scope::App.editable());
        assert_eq!(Scope::Herdr.editable(), cfg!(unix));
    }

    fn fixture(window: &mut Window, cx: &mut Context<SettingsWindow>) -> SettingsWindow {
        let source = cx.new(|cx| crate::sidebar::layout_tests::fixture_window(window, cx));
        let mut view = SettingsWindow::new(source.downgrade(), cx);
        view.themes.names = (0..400)
            .map(|index| format!("Catalog {index:03}"))
            .collect();
        view.themes.filter(None);
        // Hold the worker slot so these tests never discover or read personal files.
        view.themes.running = Some(0);
        view.theme_loading = true;
        view.theme_io = Some(ThemeIo {
            write: std::sync::Arc::new(|_, _| Ok(())),
            load: std::sync::Arc::new(|| Ok(super::super::tests::fixture())),
            resolve: None,
        });
        view
    }

    #[test]
    fn full_catalog_and_unicode_tokens_are_not_capped() {
        let names: Vec<_> = (0..400)
            .map(|index| format!("Catalog {index:03}"))
            .collect();
        assert_eq!(
            filter_names(names.iter().map(String::as_str), "").len(),
            400
        );
        assert_eq!(
            filter_names(names.iter().map(String::as_str), "399 CATALOG"),
            ["Catalog 399"]
        );
        assert_eq!(
            filter_names(["Été 東京 Night", "Other"], "東京 ÉTÉ"),
            ["Été 東京 Night"]
        );
        assert!(filter_names(["Nord"], "not found").is_empty());
    }

    #[gpui::test]
    fn selection_filter_and_contrast_fence_old_preview(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(fixture);
        view.update(cx, |view, cx| {
            let original = view.theme.clone();
            view.themes.preview = Some(original.clone());
            view.select_settings_theme(399, cx);
            assert_eq!(view.themes.choice().unwrap().name, "Catalog 399");
            assert_eq!(view.themes.running, Some(0));
            view.themes.query = "398 catalog".into();
            view.themes.filter(None);
            view.request_theme_preview(cx);
            view.themes.contrast = Contrast::High;
            view.request_theme_preview(cx);
            view.themes
                .finish_preview(0, "obsolete".into(), Ok(Theme::builtin("Nord").unwrap()));
            assert_eq!(view.themes.preview.as_ref(), Some(&original));
            assert_eq!(view.themes.loaded, None);
            assert_eq!(view.themes.choice().unwrap().name, "Catalog 398");
            let revision = view.themes.revision;
            view.themes.running = Some(revision);
            view.themes.finish_preview(
                revision,
                "Catalog 398".into(),
                Err(crate::Error::MissingHome),
            );
            assert!(view.themes.preview_error.is_some());
            assert_eq!(view.themes.preview.as_ref(), Some(&original));
            assert_eq!(view.theme, original);
            view.themes.query = "no matches".into();
            view.themes.filter(None);
            view.request_theme_preview(cx);
            assert!(view.themes.choice().is_none());
            assert!(view.themes.running.is_none());
        });
    }

    #[gpui::test]
    fn reload_preserves_query_choice_and_fences_contrast(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(fixture);
        view.update(cx, |view, cx| {
            view.themes
                .search
                .update(cx, |search, cx| search.set_text_selected("catalog 39", cx));
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            view.select_settings_theme(8, cx);
            let choice = view.themes.choice();
            let revision = view.themes.revision;
            view.config.theme = "/custom/explicit-theme".into();
            view.config.contrast = Contrast::High;
            view.sync_theme_browser(cx);
            assert_eq!(view.themes.choice(), choice);
            assert_eq!(view.themes.query, "catalog 39");
            assert_eq!(view.themes.search.read(cx).text(), "catalog 39");
            assert_eq!(view.themes.contrast, Contrast::High);
            assert!(view.themes.names.contains(&view.config.theme));
            assert!(view.themes.revision > revision);
            assert_eq!(view.themes.running, Some(0));
            assert!(view.themes.loaded.is_none());
        });
    }

    #[gpui::test]
    fn scope_switch_cannot_accept_native_preview_for_shared_choice(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(fixture);
        view.update(cx, |view, cx| {
            view.shared = Some(
                herdr_settings::Settings::parse_text("[theme.custom]\naccent='#123456'\n").unwrap(),
            );
            view.themes.scope = Scope::Herdr;
            view.themes.query = "nord".into();
            view.themes.filter(None);
            view.request_theme_preview(cx);
            view.themes
                .finish_preview(0, "Nord".into(), Ok(Theme::builtin("Nord").unwrap()));
            assert!(view.themes.preview.is_none());
            view.drive_theme_preview(cx);
            let running = view.themes.running;
            view.themes
                .finish_preview(0, "foreign completion".into(), Ok(Theme::default()));
            assert_eq!(view.themes.running, running);
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.themes.preview_name.as_deref(), Some("nord"));
            assert_eq!(view.themes.preview.as_ref().unwrap().primary(), 0x123456);
            assert_eq!(view.themes.loaded, Some(view.themes.revision));
            assert_eq!(view.themes.choice().unwrap().scope, Scope::Herdr);
        });
    }

    #[gpui::test]
    fn row_click_and_keyboard_accept_intent_while_validation_is_held(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(fixture);
        cx.simulate_resize(size(px(960.), px(1000.)));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let row = cx.debug_bounds("settings-theme-row-1").unwrap();
        cx.simulate_click(row.center(), Modifiers::default());
        cx.update(|window, cx| {
            view.read_with(cx, |view, cx| {
                assert!(view.themes.search.read(cx).focus.is_focused(window));
                assert_eq!(view.themes.selected, Some(1));
            })
        });
        cx.simulate_keystrokes("down enter");
        view.read_with(cx, |view, _| {
            assert_eq!(view.themes.selected, Some(2));
            assert!(!view.saving);
            assert_eq!(view.config.theme, Config::default().theme);
            assert_eq!(
                view.theme_intent.as_ref().unwrap().choice.name,
                "Catalog 002"
            );
        });
        cx.simulate_keystrokes("escape");
        assert!(cx.debug_bounds("settings-theme-search").is_some());
    }

    #[gpui::test]
    fn worker_loads_only_latest_search_preview_without_applying_it(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(fixture);
        view.update(cx, |view, cx| {
            view.themes.names = vec!["Nord".into(), "Dracula".into()];
            view.themes.filter(None);
            view.themes.selected = Some(0);
            view.request_theme_preview(cx);
            view.themes.selected = Some(1);
            view.request_theme_preview(cx);
            assert_eq!(view.themes.running, Some(0));
            view.themes
                .finish_preview(0, "old".into(), Err(crate::Error::MissingHome));
            view.drive_theme_preview(cx);
            assert_eq!(view.themes.running, Some(view.themes.revision));
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.themes.preview_name.as_deref(), Some("Dracula"));
            assert_eq!(view.themes.preview, Theme::builtin("Dracula"));
            assert_eq!(view.themes.loaded, Some(view.themes.revision));
            assert!(view.themes.running.is_none());
            assert_eq!(view.config.theme, Config::default().theme);
            assert_eq!(view.theme, Theme::default());
        });
    }

    #[gpui::test]
    fn normal_window_keeps_preview_search_list_and_actions_visible(cx: &mut TestAppContext) {
        let (_, cx) = cx.add_window_view(fixture);
        cx.simulate_resize(size(px(960.), px(780.)));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let preview = cx.debug_bounds("settings-theme-preview").unwrap();
        let search = cx.debug_bounds("settings-theme-search").unwrap();
        let list = cx.debug_bounds("settings-theme-list").unwrap();
        let actions = cx.debug_bounds("settings-theme-actions").unwrap();
        assert_eq!(preview.size.height, px(225.));
        assert_eq!(search.size.height, px(32.));
        assert_eq!(list.size.height, px(LIST_HEIGHT));
        assert!(preview.bottom() <= search.top());
        assert!(search.bottom() <= list.top());
        assert!(list.bottom() <= actions.top());
        assert!(actions.bottom() <= px(780.), "actions: {actions:?}");
    }

    #[gpui::test]
    fn list_materializes_only_visible_rows_and_scrolls_to_last(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(fixture);
        cx.simulate_resize(size(px(960.), px(1000.)));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("settings-theme-row-0").is_some());
        assert!(cx.debug_bounds("settings-theme-row-399").is_none());
        view.update(cx, |view, cx| view.select_settings_theme(399, cx));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("settings-theme-row-399").is_some());
        assert!(cx.debug_bounds("settings-theme-row-0").is_none());
        assert!(cx.debug_bounds("settings-theme-row-389").is_none());
        let visible = [
            "settings-theme-row-390",
            "settings-theme-row-391",
            "settings-theme-row-392",
            "settings-theme-row-393",
            "settings-theme-row-394",
            "settings-theme-row-395",
            "settings-theme-row-396",
            "settings-theme-row-397",
            "settings-theme-row-398",
            "settings-theme-row-399",
        ]
        .into_iter()
        .filter(|selector| cx.debug_bounds(selector).is_some())
        .count();
        assert!(
            visible <= (LIST_HEIGHT / ROW_HEIGHT).ceil() as usize + 2,
            "{visible} materialized rows"
        );
    }

    #[gpui::test]
    fn composition_navigation_and_escape_remain_in_search(cx: &mut TestAppContext) {
        let (view, cx) = cx.add_window_view(fixture);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.themes.names = vec!["東京 Night".into(), "東京 Day".into()];
                view.themes.search.update(cx, |search, cx| {
                    search.replace_and_mark_text_in_range(None, "東京", Some(2..2), window, cx);
                });
            })
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                assert_eq!(view.themes.filtered.len(), 2);
                let revision = view.themes.revision;
                for key in ["up", "down", "enter", "escape"] {
                    view.theme_browser_key(
                        &KeyDownEvent {
                            keystroke: Keystroke::parse(key).unwrap(),
                            is_held: false,
                            prefer_character_input: false,
                        },
                        window,
                        cx,
                    );
                }
                assert_eq!(view.themes.revision, revision);
                assert_eq!(view.themes.search.read(cx).text(), "東京");
                view.themes
                    .search
                    .update(cx, |search, cx| search.unmark_text(window, cx));
                view.theme_browser_key(
                    &KeyDownEvent {
                        keystroke: Keystroke::parse("escape").unwrap(),
                        is_held: false,
                        prefer_character_input: false,
                    },
                    window,
                    cx,
                );
                assert_eq!(view.themes.search.read(cx).text(), "");
            })
        });
    }

    #[test]
    fn shared_preview_preserves_overrides_and_never_mutates_settings() {
        let settings = herdr_settings::Settings::parse_text("[theme]\nname='nord'\nauto_switch=true\nlight_name='one-light'\n[theme.custom]\naccent='#123456'\n").unwrap();
        let before = settings.theme(true).unwrap();
        for name in herdr_settings::THEME_NAMES {
            let preview = settings.preview_theme(name, true).unwrap();
            assert_eq!(preview.primary(), 0x123456);
        }
        assert!(
            settings
                .preview_theme("Ghostty external theme", false)
                .is_err()
        );
        assert_eq!(settings.theme_name, "nord");
        assert_eq!(settings.theme(true).unwrap(), before);
        let plain = herdr_settings::Settings::parse_text(
            "[theme]\nauto_switch=true\nlight_name='one-light'\n",
        )
        .unwrap();
        let expected = herdr_settings::Settings::parse_text("[theme]\nname='nord'\n").unwrap();
        assert_eq!(
            plain.preview_theme("nord", true).unwrap(),
            expected.theme(true).unwrap()
        );
    }
}
