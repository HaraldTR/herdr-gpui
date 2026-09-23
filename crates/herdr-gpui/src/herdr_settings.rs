//! Shared release-namespace Herdr settings. `load` and `save` do blocking I/O:
//! callers MUST run them on a background worker, never in render/input handlers.
//!
//! Schema/defaults and palette data adapted from https://github.com/herdrdev/herdr
//! revision 8ac9542757292f7a8d42a2d532bc6a8a33c7ffce (Apache-2.0), specifically
//! src/config/{model,sound,theme}.rs, src/app/{state,mod}.rs and
//! src/client/shell.rs. This adaptation
//! uses GPUI RGB colors, strict bounded reads, and targeted optimistic saves.

#[path = "herdr_settings/palette.rs"]
mod palette;
#[path = "herdr_settings/persistence.rs"]
mod persistence;
#[cfg(test)]
#[path = "herdr_settings/tests.rs"]
mod tests;

use herdr_client::protocol::AgentStatus;
use serde::Deserialize;
use std::{collections::BTreeMap, env, path::PathBuf};
use toml_edit::{DocumentMut, Item, Value};

pub(crate) const THEME_NAMES: &[&str] = &[
    "catppuccin",
    "catppuccin-latte",
    "terminal",
    "tokyo-night",
    "tokyo-night-day",
    "dracula",
    "nord",
    "gruvbox",
    "gruvbox-light",
    "one-dark",
    "one-light",
    "solarized",
    "solarized-light",
    "kanagawa",
    "kanagawa-lotus",
    "rose-pine",
    "rose-pine-dawn",
    "vesper",
];

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("shared Herdr config I/O failed")]
    Io(#[from] std::io::Error),
    #[error("shared Herdr config was replaced but completion failed; reload before saving again")]
    Committed(#[source] std::io::Error),
    #[error("invalid shared Herdr TOML")]
    Parse(#[from] toml::de::Error),
    #[error("cannot edit shared Herdr TOML")]
    Edit(#[from] toml_edit::TomlError),
    #[error("shared Herdr config changed; reload before saving")]
    Conflict,
    #[error("shared Herdr config is busy; retry saving")]
    Busy,
    #[error(
        "shared Herdr config requires an owned regular file and owned, non-writable-by-others parent; symlink targets are refused"
    )]
    UnsafePath,
    #[error("shared Herdr config exceeds the 1 MiB limit")]
    TooLarge,
    #[error("HOME is unset or the config root is not absolute")]
    ConfigRoot,
    #[error("ui.toast.delay_seconds must be between 0 and 3600")]
    ToastDelay,
    #[error("unknown Herdr theme: {0}")]
    Theme(String),
    #[error("cannot edit non-table config field {0}")]
    Table(&'static str),
    #[error("could not remove shared config temporary file: {cleanup}")]
    Cleanup {
        #[source]
        source: Option<Box<Error>>,
        cleanup: std::io::Error,
    },
}

// Keep the concrete source through the existing boxed config-error boundary.
// A dedicated root HerdrSettings(#[from] herdr_settings::Error) is optional.
impl From<Error> for crate::Error {
    fn from(source: Error) -> Self {
        Self::ConfigFile {
            uri: None,
            source: Box::new(source),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum IndicatorStyle {
    #[default]
    Dots,
    Symbols,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ToastDelivery {
    #[default]
    Off,
    Herdr,
    Terminal,
    System,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ToastPosition {
    TopLeft,
    TopRight,
    BottomLeft,
    #[default]
    BottomRight,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ClipboardPosition {
    TopLeft,
    TopCenter,
    TopRight,
    BottomLeft,
    #[default]
    BottomCenter,
    BottomRight,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentSoundSetting {
    #[default]
    Default,
    On,
    Off,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub(crate) struct Sound {
    pub enabled: bool,
    pub path: Option<PathBuf>,
    pub done_path: Option<PathBuf>,
    pub request_path: Option<PathBuf>,
    pub agents: BTreeMap<String, AgentSoundSetting>,
}

impl Default for Sound {
    fn default() -> Self {
        Self {
            enabled: true,
            path: None,
            done_path: None,
            request_path: None,
            agents: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Edit {
    Theme(String),
    Indicators(IndicatorStyle),
    Sound(bool),
    Toasts(ToastDelivery),
}

#[derive(Clone)]
pub(crate) struct Settings {
    pub path: PathBuf,
    pub theme_name: String,
    pub indicators: IndicatorStyle,
    pub sound_enabled: bool,
    pub toast_delivery: ToastDelivery,
    pub toast_delay_seconds: u64,
    pub toast_position: ToastPosition,
    pub clipboard: ClipboardToast,
    pub sound: Sound,
    palettes: [palette::Palette; 2],
    original: persistence::Snapshot,
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The original document may contain private settings unrelated to this
        // adapter. Diagnostics must not dump its contents or custom sound paths.
        f.debug_struct("Settings")
            .field("path", &self.path)
            .field("theme_name", &self.theme_name)
            .field("indicators", &self.indicators)
            .field("sound_enabled", &self.sound_enabled)
            .field("toast_delivery", &self.toast_delivery)
            .field("toast_delay_seconds", &self.toast_delay_seconds)
            .field("toast_position", &self.toast_position)
            .field("clipboard", &self.clipboard)
            .finish_non_exhaustive()
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Parsed {
    theme: palette::ThemeConfig,
    ui: Ui,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Ui {
    status_indicators: IndicatorStyle,
    sound: Sound,
    toast: RawToast,
    accent: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct RawToast {
    delivery: Option<ToastDelivery>,
    enabled: Option<bool>,
    delay_seconds: Option<u64>,
    herdr: HerdrToast,
    clipboard: ClipboardToast,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct HerdrToast {
    position: ToastPosition,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub(crate) struct ClipboardToast {
    pub enabled: bool,
    pub position: ClipboardPosition,
}

impl Default for ClipboardToast {
    fn default() -> Self {
        Self {
            enabled: true,
            position: ClipboardPosition::BottomCenter,
        }
    }
}

impl Settings {
    /// Background-only. A missing file yields defaults without creating anything.
    pub(crate) fn load() -> crate::Result<Self> {
        let path = config_path(
            env::var_os("HERDR_CONFIG_PATH"),
            env::var_os("XDG_CONFIG_HOME"),
            env::var_os("HOME"),
        )?;
        Self::load_path(path)
    }

    fn load_path(path: PathBuf) -> crate::Result<Self> {
        let result =
            persistence::read(&path).and_then(|snapshot| Self::parse(path.clone(), snapshot));
        result.map_err(|error| crate::Error::from(error).at_path(&path))
    }

    fn parse(path: PathBuf, original: persistence::Snapshot) -> Result<Self, Error> {
        if original
            .text
            .as_ref()
            .is_some_and(|text| text.len() as u64 > persistence::LIMIT)
        {
            return Err(Error::TooLarge);
        }
        let parsed: Parsed = toml::from_str(original.text.as_deref().unwrap_or(""))?;
        let toast = parsed.ui.toast;
        let delay = toast.delay_seconds.unwrap_or(1);
        if delay > 3600 {
            return Err(Error::ToastDelay);
        }
        let theme_name = parsed.theme.name.as_deref().unwrap_or("catppuccin");
        let legacy_accent = parsed
            .ui
            .accent
            .as_deref()
            .filter(|accent| *accent != "cyan");
        // Resolve arbitrary config strings once on the loader, not per rendered
        // indicator. Appearance changes only select a fixed-size palette.
        let palettes =
            [false, true].map(|light| parsed.theme.resolve(theme_name, legacy_accent, light));
        Ok(Self {
            path,
            theme_name: theme_name.into(),
            indicators: parsed.ui.status_indicators,
            sound_enabled: parsed.ui.sound.enabled,
            sound: parsed.ui.sound,
            toast_delivery: toast.delivery.unwrap_or(if toast.enabled == Some(true) {
                ToastDelivery::Herdr
            } else {
                ToastDelivery::Off
            }),
            toast_delay_seconds: delay,
            toast_position: toast.herdr.position,
            clipboard: toast.clipboard,
            palettes,
            original,
        })
    }

    /// Background-only. Returns a fresh snapshot only after durable replacement.
    /// Never merges stale edits: even unrelated external changes require reload.
    pub(crate) fn save(&self, edit: Edit) -> crate::Result<Self> {
        let result = (|| -> Result<Self, Error> {
            let mut document = self
                .original
                .text
                .as_deref()
                .unwrap_or("")
                .parse::<DocumentMut>()?;
            match edit {
                Edit::Theme(name) => {
                    let name =
                        palette::canonical(&name).ok_or_else(|| Error::Theme(name.clone()))?;
                    set(&mut document, &["theme", "name"], name.into())?;
                    set(&mut document, &["theme", "auto_switch"], false.into())?;
                }
                Edit::Indicators(style) => set(
                    &mut document,
                    &["ui", "status_indicators"],
                    match style {
                        IndicatorStyle::Dots => "dots",
                        IndicatorStyle::Symbols => "symbols",
                    }
                    .into(),
                )?,
                Edit::Sound(enabled) => {
                    set(&mut document, &["ui", "sound", "enabled"], enabled.into())?
                }
                Edit::Toasts(delivery) => {
                    set(
                        &mut document,
                        &["ui", "toast", "delivery"],
                        match delivery {
                            ToastDelivery::Off => "off",
                            ToastDelivery::Herdr => "herdr",
                            ToastDelivery::Terminal => "terminal",
                            ToastDelivery::System => "system",
                        }
                        .into(),
                    )?;
                    let mut comments = String::new();
                    if let Some(table) = document["ui"]["toast"].as_table_like_mut() {
                        if let Some(key) = table.key("enabled")
                            && let Some(prefix) =
                                key.leaf_decor().prefix().and_then(|raw| raw.as_str())
                            && prefix.contains('#')
                        {
                            comments.push_str(prefix);
                        }
                        if let Some(Item::Value(value)) = table.remove("enabled")
                            && let Some(suffix) =
                                value.decor().suffix().and_then(|raw| raw.as_str())
                            && suffix.contains('#')
                        {
                            comments.push_str(suffix);
                            comments.push('\n');
                        }
                    }
                    // The deleted legacy key has no place to keep its decor;
                    // retain its comments at EOF rather than discarding them.
                    if !comments.is_empty() {
                        document.set_trailing(format!(
                            "{}\n{comments}",
                            document.trailing().as_str().unwrap_or("")
                        ));
                    }
                }
            }
            let text = document.to_string();
            // Validate before performing any writes, including directory creation.
            let mut next = Self::parse(
                self.path.clone(),
                persistence::Snapshot {
                    text: Some(text.clone()),
                    ..self.original.clone()
                },
            )?;
            next.original = persistence::save(&self.path, &self.original, &text)?;
            Ok(next)
        })();
        result.map_err(|error| crate::Error::from(error).at_path(&self.path))
    }

    /// Pure: no filesystem access; safe to use prepared settings on the UI thread.
    pub(crate) fn theme(&self, light: bool) -> crate::Result<crate::config::Theme> {
        Ok(self.colors(light).theme())
    }

    fn colors(&self, light: bool) -> &palette::Palette {
        &self.palettes[usize::from(light)]
    }

    pub(crate) fn status_color(&self, status: AgentStatus, light: bool) -> u32 {
        self.colors(light).status(status)
    }

    /// Does not check existence or play audio. Relative paths use this snapshot's
    /// config directory, not the current environment or daemon endpoint.
    pub(crate) fn sound_path(&self, request: bool) -> Option<PathBuf> {
        let specific = if request {
            &self.sound.request_path
        } else {
            &self.sound.done_path
        };
        let path = specific.as_ref().or(self.sound.path.as_ref())?;
        Some(if path.is_absolute() {
            path.clone()
        } else {
            self.path.parent()?.join(path)
        })
    }

    /// `agent` is the daemon's canonical agent label (config keys for OpenCode,
    /// Copilot, and Antigravity are accepted too). Unknown agents inherit the
    /// global switch, just as upstream's `None`/unconfigured agents do.
    pub(crate) fn sound_allowed(&self, agent: Option<&str>) -> bool {
        if !self.sound_enabled {
            return false;
        }
        let key = match agent {
            Some("opencode" | "open-code" | "open_code") => "open_code",
            Some("copilot" | "github-copilot" | "github_copilot") => "github_copilot",
            Some("antigravity" | "agy") => "agy",
            Some(
                key @ ("pi" | "claude" | "codex" | "gemini" | "cursor" | "devin" | "cline" | "kimi"
                | "kiro" | "droid" | "amp" | "grok" | "hermes" | "kilo" | "qodercli"
                | "qwen" | "letta" | "maki" | "muse"),
            ) => key,
            Some(_) | None => return true,
        };
        let default = if key == "droid" {
            AgentSoundSetting::Off
        } else {
            AgentSoundSetting::Default
        };
        self.sound.agents.get(key).copied().unwrap_or(default) != AgentSoundSetting::Off
    }
}

fn config_path(
    explicit: Option<std::ffi::OsString>,
    xdg: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Result<PathBuf, Error> {
    let path = if let Some(path) = explicit.filter(|p| !p.is_empty()) {
        PathBuf::from(path)
    } else if let Some(root) = xdg.filter(|p| !p.is_empty()) {
        let root = PathBuf::from(root);
        if !root.is_absolute() {
            return Err(Error::ConfigRoot);
        }
        root.join("herdr/config.toml")
    } else {
        let root = PathBuf::from(home.filter(|p| !p.is_empty()).ok_or(Error::ConfigRoot)?);
        if !root.is_absolute() {
            return Err(Error::ConfigRoot);
        }
        root.join(".config/herdr/config.toml")
    };
    Ok(if path.is_absolute() {
        path
    } else {
        env::current_dir()?.join(path)
    })
}

fn set(document: &mut DocumentMut, keys: &[&'static str], mut value: Value) -> Result<(), Error> {
    let mut item = document.as_item_mut();
    for key in &keys[..keys.len() - 1] {
        let table = item.as_table_like_mut().ok_or(Error::Table(key))?;
        if !table.contains_key(key) {
            table.insert(key, Item::Table(toml_edit::Table::new()));
        }
        item = table.get_mut(key).ok_or(Error::Table(key))?;
    }
    let key = keys[keys.len() - 1];
    let table = item.as_table_like_mut().ok_or(Error::Table(key))?;
    if let Some(previous) = table.get(key).and_then(Item::as_value) {
        *value.decor_mut() = previous.decor().clone();
    }
    table.insert(key, Item::Value(value));
    Ok(())
}
