//! GUI settings. The daemon's own config is read only where the GUI honors a
//! preference the user already expressed there, never written and never used
//! to change daemon behavior. Managed defaults are refreshed from the binary;
//! `config-gpui.local.toml` holds persistent user overrides.
use crate::{
    Error, Result,
    contrast::Contrast,
    error::ThemeParseError,
    keymap::{Binding, DaemonKeys, Keymap},
};
pub(crate) mod preferences;
pub(crate) mod sidebar;
pub(crate) mod watch;

use gpui::{Font, FontFallbacks};
use serde::Deserialize;
pub(crate) use sidebar::{
    AgentLayout, AgentToken, Rows, SidebarLayout, SpaceLayout, SpaceToken, TokenStyle,
};
use std::{
    collections::BTreeMap,
    env, fs,
    io::{ErrorKind, Write},
    ops::RangeInclusive,
    path::{Component, Path, PathBuf},
};

const DEFAULT_CONFIG: &str = include_str!("../config-gpui.example.toml");
const FOLLOW_HERDR: &str = "Follow Herdr";
// Compare the first line so Windows checkouts and editors can use CRLF.
const MANAGED_HEADER: &str = "# DO NOT EDIT -- WILL BE OVERWRITTEN";
/// Seeds the overrides file on first launch only. Existing overrides and
/// migrated personal configs are never rewritten, so settings placed here
/// reach new installs without changing what current users see.
const LOCAL_CONFIG: &str = "# Herdr GPUI overrides. Saved changes reload automatically.\n# Unset keys inherit config-gpui.toml; tables merge key by key.\n\n# New installs start with the roomy rounded sidebar. Remove this line for\n# the managed default, or pick another layout listed in config-gpui.toml.\nlayout = \"comfortable-rounded\"\n";

/// Every face is held to this range, whether it comes from the config file or
/// from a runtime adjustment, so the two can never disagree on what is valid.
pub const FONT_SIZE_RANGE: RangeInclusive<f32> = 8.0..=48.0;

/// One logical pixel: the smallest step that can move the terminal cell grid.
pub const FONT_SIZE_STEP: f32 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FontFace {
    Sidebar,
    Tabs,
    Terminal,
    Ui,
}

impl FontFace {
    pub(crate) fn set_size(self, config: &mut Config, size: f32) {
        match self {
            Self::Sidebar => config.sidebar.size = size,
            Self::Tabs => config.tabs.size = size,
            Self::Terminal => config.terminal.size = size,
            Self::Ui => config.ui.size = size,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Sidebar => "sidebar",
            Self::Tabs => "tabs",
            Self::Terminal => "terminal",
            Self::Ui => "ui",
        }
    }

    pub(crate) fn size(self, config: &Config) -> f32 {
        match self {
            Self::Sidebar => config.sidebar.size,
            Self::Tabs => config.tabs.size,
            Self::Terminal => config.terminal.size,
            Self::Ui => config.ui.size,
        }
    }
}

/// Shared logical-pixel radii for native-style chrome, independent of the
/// terminal grid. Small badges/keycaps retain a tighter curve than controls.
pub(crate) mod corners {
    pub(crate) const PANEL: f32 = 12.;
    pub(crate) const CONTROL: f32 = 8.;
    pub(crate) const SMALL: f32 = 4.;
}

#[derive(Clone, Debug)]
pub struct Config {
    pub theme: String,
    pub confirm_close_tab: bool,
    pub show_agents: bool,
    /// CPU and memory of the selected host in the status bar.
    pub show_system_load: bool,
    /// How far the app's own marks and labels stand off its chrome.
    pub contrast: Contrast,
    pub usage: crate::usage::UsageConfig,
    pub option_as_alt: OptionAsAlt,
    pub open_links_in: LinkTarget,
    /// Whether a terminal selection stays highlighted, and readable by
    /// selection tools, after it is copied.
    pub keep_selection_after_copy: bool,
    pub sidebar: FontConfig,
    pub tabs: FontConfig,
    pub terminal: FontConfig,
    pub ui: FontConfig,
    pub github: GitHubConfig,
    pub features: Features,
    pub notifications: NotificationConfig,
    pub(crate) notification_overrides: NotificationSettings,
    pub clipboard_toast: ClipboardToast,
    pub bell: BellConfig,
    pub layout: Layout,
    /// Daemon sidebar rows, falling back to defaults when invalid.
    pub sidebar_layout: SidebarLayout,
    pub keybindings: Keymap,
    /// The `[keybindings]` table `keybindings` was built from, kept so a
    /// device's server keys can be layered under the same GUI overrides.
    pub(crate) keybinding_overrides: BTreeMap<String, Binding>,
    /// Per saved device, by catalog profile ID.
    pub(crate) devices: BTreeMap<String, DeviceSettings>,
    pub palette: crate::palette::PaletteConfig,
    /// Keys the file names that this build does not know, sorted. They are
    /// ignored, as Herdr ignores its own, so a config written by a newer
    /// build or with a typo still loads; `diagnostic` reports them.
    pub unknown_keys: Vec<String>,
}

/// A device list larger than any real catalog is a config mistake.
const MAX_DEVICES: usize = 256;

/// Whose `[keys]` a saved device answers to, as `herdr --remote-keybindings`
/// chooses for the TUI. Local is upstream's default: muscle memory stays the
/// same on every host. Only keybindings follow the server; themes, sidebar,
/// and toasts stay local either way.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum KeybindingSource {
    #[default]
    Local,
    /// The host's published `server_keybindings_toml`.
    Server,
}

/// One `[devices.<profile-id>]` table.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct DeviceSettings {
    pub(crate) keybindings: KeybindingSource,
}

impl Config {
    /// Whose keybindings the endpoint uses. Local and explicit sockets are not
    /// saved devices, so they always use the local ones.
    pub(crate) fn keybinding_source(&self, endpoint_id: &str) -> KeybindingSource {
        crate::endpoint::saved_profile_id(endpoint_id)
            .and_then(|profile| self.devices.get(profile))
            .map(|device| device.keybindings)
            .unwrap_or_default()
    }
}

/// Where a clicked terminal link opens. Alt-click (Option on macOS) opens it
/// in the other one.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum LinkTarget {
    #[default]
    System,
    /// A browser tab in the workspace, where the build can show pages.
    BrowserTab,
}

/// Whether macOS Option sends Alt shortcuts to a pane or types the character
/// the keyboard layout puts on it. Other platforms have no Option layer, so
/// Alt always reaches the pane there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OptionAsAlt {
    /// Alt on the U.S. and ABC layouts, whose Option layer only holds symbols
    /// like `π`; typing elsewhere, where it holds `@`, `[`, or letters.
    #[default]
    Auto,
    Always,
    Never,
}

impl OptionAsAlt {
    /// macOS layouts whose Option characters a terminal user rarely types.
    const ALT_LAYOUTS: [&'static str; 2] = ["com.apple.keylayout.US", "com.apple.keylayout.ABC"];

    /// Whether Option-modified keys go to the pane as Alt under `layout`, the
    /// platform keyboard layout ID.
    pub fn sends_alt(self, layout: &str) -> bool {
        if !cfg!(target_os = "macos") {
            return true;
        }
        match self {
            Self::Auto => Self::ALT_LAYOUTS.contains(&layout),
            Self::Always => true,
            Self::Never => false,
        }
    }
}

impl<'de> Deserialize<'de> for OptionAsAlt {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Value {
            Bool(bool),
            Name(String),
        }
        match Value::deserialize(deserializer)? {
            Value::Bool(true) => Ok(Self::Always),
            Value::Bool(false) => Ok(Self::Never),
            Value::Name(name) if name == "auto" => Ok(Self::Auto),
            Value::Name(name) => Err(serde::de::Error::unknown_variant(&name, &["auto"])),
        }
    }
}

/// Where the "copied to clipboard" flash sits, and whether it appears at all.
/// Resolved from the daemon's `[ui.toast.clipboard]`, then from this GUI's own
/// `[clipboard_toast]`, so one terminal preference covers both clients.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClipboardToast {
    pub enabled: bool,
    pub position: ClipboardToastPosition,
}

impl Default for ClipboardToast {
    fn default() -> Self {
        // herdr's own defaults, so an unconfigured pair of clients agrees.
        Self {
            enabled: true,
            position: ClipboardToastPosition::BottomCenter,
        }
    }
}

/// From herdr src/config/model.rs.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ClipboardToastPosition {
    TopLeft,
    TopCenter,
    TopRight,
    BottomLeft,
    #[default]
    BottomCenter,
    BottomRight,
}

/// What a pane's terminal bell does. Herdr forwards each bell to its
/// foreground client and leaves the reaction to it, as an outer terminal's
/// own bell settings would, so this is the GUI's `[bell]` alone.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct BellConfig {
    /// Ask for attention (bounce the Dock icon) while the window is inactive.
    pub attention: bool,
    /// Play the system alert sound.
    pub sound: bool,
}

impl Default for BellConfig {
    fn default() -> Self {
        Self {
            attention: true,
            sound: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct NotificationConfig {
    /// In-app toasts, which take precedence over OS notifications.
    pub enabled: bool,
    /// Shared `system` delivery: post daemon notifications to the OS
    /// notification center. Not a native key, so a local override of
    /// `enabled` decides between the two.
    #[serde(skip)]
    pub system: bool,
    #[serde(deserialize_with = "notification_delay")]
    pub delay_seconds: u64,
    pub position: herdr_client::protocol::ToastHerdrPosition,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            system: false,
            delay_seconds: 1,
            position: herdr_client::protocol::ToastHerdrPosition::BottomRight,
        }
    }
}

/// Only explicitly configured GUI keys override the shared Herdr preferences.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct NotificationSettings {
    enabled: Option<bool>,
    #[serde(deserialize_with = "optional_notification_delay")]
    delay_seconds: Option<u64>,
    position: Option<herdr_client::protocol::ToastHerdrPosition>,
}

/// Where a daemon notification that passes the shared policy is presented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NotificationDelivery {
    Off,
    InApp,
    System,
}

impl NotificationConfig {
    pub(crate) fn delivery(self) -> NotificationDelivery {
        if self.enabled {
            NotificationDelivery::InApp
        } else if self.system {
            NotificationDelivery::System
        } else {
            NotificationDelivery::Off
        }
    }
}

impl NotificationSettings {
    fn resolve(self, base: NotificationConfig) -> NotificationConfig {
        NotificationConfig {
            enabled: self.enabled.unwrap_or(base.enabled),
            system: base.system,
            delay_seconds: self.delay_seconds.unwrap_or(base.delay_seconds),
            position: self.position.unwrap_or(base.position),
        }
    }
}

fn optional_notification_delay<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<u64>, D::Error> {
    notification_delay(d).map(Some)
}

fn notification_delay<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<u64, D::Error> {
    let seconds = u64::deserialize(d)?;
    if seconds > 3600 {
        return Err(serde::de::Error::custom(
            "notifications.delay_seconds must be between 0 and 3600",
        ));
    }
    Ok(seconds)
}

/// Sidebar layout and spacing the config file can adjust.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub mode: LayoutMode,
    /// Blank space between the sidebar and the terminal it borders. Applies
    /// only while the sidebar is on screen, and narrows the terminal, so the
    /// daemon is told about the columns it actually has.
    pub sidebar_gap: f32,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            mode: LayoutMode::default(),
            sidebar_gap: DEFAULT_SIDEBAR_GAP,
        }
    }
}

/// How much the sidebar fits: spacing, indents, and which details show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Density {
    #[default]
    Normal,
    Compact,
    Comfortable,
}

/// How sidebar rows are drawn, independent of how dense they are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Style {
    /// Edge-to-edge rows with square highlights and tree lines.
    #[default]
    Flat,
    /// Inset rows with rounded, bordered highlights.
    Rounded,
}

/// A named sidebar layout. Each one draws its rows differently: Herdr's own
/// rows at a density, flat or rounded, or a design of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutMode {
    /// Herdr's rows: `normal`, `compact`, `comfortable`, or any of them with a
    /// `-rounded` suffix.
    Classic { density: Density, style: Style },
    /// Single-line rows with an icon slot and pull request counts.
    Superset,
    /// Rounded cards with a meta line for host, branch, and pull request.
    Orca,
    /// One line per row with only the status and the name.
    Minimal,
}

impl Default for LayoutMode {
    fn default() -> Self {
        Self::new(Density::Normal, Style::Flat)
    }
}

impl LayoutMode {
    /// `ALL`'s names, for errors that list what a config may say.
    const NAMES: &'static [&'static str] = &[
        "normal",
        "compact",
        "comfortable",
        "normal-rounded",
        "compact-rounded",
        "comfortable-rounded",
        "superset",
        "orca",
        "minimal",
    ];

    /// Every named layout, in the order menus list them.
    pub const ALL: [Self; 9] = [
        Self::new(Density::Normal, Style::Flat),
        Self::new(Density::Compact, Style::Flat),
        Self::new(Density::Comfortable, Style::Flat),
        Self::new(Density::Normal, Style::Rounded),
        Self::new(Density::Compact, Style::Rounded),
        Self::new(Density::Comfortable, Style::Rounded),
        Self::Superset,
        Self::Orca,
        Self::Minimal,
    ];

    pub const fn new(density: Density, style: Style) -> Self {
        Self::Classic { density, style }
    }

    /// The spacing the list around the rows uses. Layouts with their own
    /// design fix theirs, so no second setting half-changes them.
    pub const fn density(self) -> Density {
        match self {
            Self::Classic { density, .. } => density,
            Self::Superset | Self::Minimal => Density::Normal,
            Self::Orca => Density::Comfortable,
        }
    }

    /// The highlight shape and heading case the list uses.
    pub const fn style(self) -> Style {
        match self {
            Self::Classic { style, .. } => style,
            Self::Superset | Self::Minimal => Style::Flat,
            Self::Orca => Style::Rounded,
        }
    }

    /// The config value that selects it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Classic { density, style } => match (density, style) {
                (Density::Normal, Style::Flat) => "normal",
                (Density::Compact, Style::Flat) => "compact",
                (Density::Comfortable, Style::Flat) => "comfortable",
                (Density::Normal, Style::Rounded) => "normal-rounded",
                (Density::Compact, Style::Rounded) => "compact-rounded",
                (Density::Comfortable, Style::Rounded) => "comfortable-rounded",
            },
            Self::Superset => "superset",
            Self::Orca => "orca",
            Self::Minimal => "minimal",
        }
    }

    /// How menus title it.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Classic { density, style } => match (density, style) {
                (Density::Normal, Style::Flat) => "Normal",
                (Density::Compact, Style::Flat) => "Compact",
                (Density::Comfortable, Style::Flat) => "Comfortable",
                (Density::Normal, Style::Rounded) => "Normal Rounded",
                (Density::Compact, Style::Rounded) => "Compact Rounded",
                (Density::Comfortable, Style::Rounded) => "Comfortable Rounded",
            },
            Self::Superset => "Superset",
            Self::Orca => "Orca",
            Self::Minimal => "Minimal",
        }
    }
}

impl From<Density> for LayoutMode {
    fn from(density: Density) -> Self {
        Self::new(density, Style::Flat)
    }
}

impl TryFrom<&str> for LayoutMode {
    type Error = Error;

    fn try_from(name: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|mode| mode.name() == name)
            .ok_or_else(|| Error::UnknownLayout(name.to_owned()))
    }
}

impl std::fmt::Display for LayoutMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl<'de> Deserialize<'de> for LayoutMode {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Self::try_from(name.as_str())
            .map_err(|_| serde::de::Error::unknown_variant(&name, Self::NAMES))
    }
}

impl<'de> Deserialize<'de> for Layout {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        // Keep shipped [layout] spacing settings readable alongside named
        // layouts. A visitor rather than an untagged enum, so a key this build
        // does not know is reported as ignored instead of buffered away.
        #[derive(Default, Deserialize)]
        #[serde(default)]
        struct Options {
            mode: LayoutMode,
            sidebar_gap: Option<f32>,
        }

        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Layout;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a layout name or a [layout] table")
            }

            fn visit_str<E: serde::de::Error>(self, name: &str) -> std::result::Result<Layout, E> {
                let mode = LayoutMode::try_from(name)
                    .map_err(|_| E::unknown_variant(name, LayoutMode::NAMES))?;
                Ok(Layout {
                    mode,
                    ..Layout::default()
                })
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> std::result::Result<Layout, A::Error> {
                let Options { mode, sidebar_gap } =
                    Options::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                Ok(Layout {
                    mode,
                    sidebar_gap: sidebar_gap.unwrap_or(DEFAULT_SIDEBAR_GAP),
                })
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

/// Optional behaviors the config file turns on. Every flag is off by default,
/// so a missing or empty `[features]` table is the shipped experience.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Features {
    /// Open a space's menu when the pointer rests on its sidebar row.
    pub sidebar_hover_menu: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct GitHubConfig {
    pub oauth_client_id: Option<String>,
    pub allow_plaintext_credentials: bool,
}

impl GitHubConfig {
    pub fn client_id(&self) -> Result<Option<String>> {
        self.client_id_with_override(env::var_os("HERDR_GITHUB_OAUTH_CLIENT_ID").as_deref())
    }

    fn client_id_with_override(&self, value: Option<&std::ffi::OsStr>) -> Result<Option<String>> {
        let (id, source) = match value {
            Some(value) => (
                Some(value.to_str().ok_or(Error::ClientIdEncoding)?),
                "HERDR_GITHUB_OAUTH_CLIENT_ID",
            ),
            None => (
                Some(
                    self.oauth_client_id
                        .as_deref()
                        .unwrap_or("Iv23liurUcwxPjrdIFYT"),
                ),
                "github.oauth_client_id",
            ),
        };
        if let Some(id) = id
            && (id.is_empty()
                || id.len() > 256
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')))
        {
            return Err(Error::InvalidClientId(source));
        }
        Ok(id.map(str::to_owned))
    }
}

/// Keep the terminal flush with the divider unless spacing is requested.
const DEFAULT_SIDEBAR_GAP: f32 = 0.;

/// A gap wider than this stops reading as spacing and starts eating columns the
/// terminal needs, so the config file is held to a band a window can afford.
const MAX_SIDEBAR_GAP: f32 = 64.;

/// The daemon's config is read for a handful of keys, so a file far larger
/// than any hand-written config is skipped rather than parsed on every load.
const MAX_DAEMON_CONFIG_BYTES: u64 = 1 << 20;

/// Upper bound on a configured cascade. Every entry is searched for each
/// uncovered codepoint, so a long list costs shaping time and covers nothing a
/// short one does not. Names that are not installed are ignored by the platform.
const MAX_FONT_FALLBACKS: usize = 8;

/// Nerd Font patches keep this marker in every patched family name, so matching
/// it finds the installed icon faces without naming individual fonts.
const SYMBOL_FAMILY_MARKER: &str = "nerd font";

/// A cascade is searched in order for every uncovered codepoint, so automatic
/// detection keeps only the best-ranked few families.
const MAX_DETECTED_FALLBACKS: usize = 3;

#[derive(Clone, Debug)]
pub struct FontConfig {
    pub family: String,
    pub size: f32,
    /// Families searched, nearest first, for glyphs `family` lacks. `None`
    /// until the config names them or [`Config::resolve_font_fallbacks`]
    /// detects them; an empty list opts out of any cascade.
    pub fallbacks: Option<Vec<String>>,
}

impl FontConfig {
    pub fn line_height(&self) -> f32 {
        self.size * 20.0 / 14.0
    }

    /// The shaping font for this face. Terminal prompts draw powerline
    /// separators and Nerd Font icons from the Private Use Area, which no text
    /// face and no platform default cascade covers, so those cells shape to the
    /// missing-glyph box unless the cascade names an icon font explicitly.
    pub fn font(&self) -> Font {
        let mut font = gpui::font(self.family.clone());
        font.fallbacks = self
            .fallbacks
            .as_ref()
            .filter(|families| !families.is_empty())
            .map(|families| FontFallbacks::from_fonts(families.clone()));
        font
    }
}

/// Ranks an installed Nerd Font family for the automatic cascade. Symbols-only
/// faces carry the icon ranges without replacing any text glyph, and `Mono`
/// variants keep every icon inside a single terminal cell, so both come first.
fn fallback_rank(family: &str) -> u8 {
    let lowercase = family.to_lowercase();
    let symbols = lowercase.starts_with("symbols nerd font");
    let mono = lowercase.ends_with(" mono");
    match (symbols, mono) {
        (true, true) => 0,
        (true, false) => 1,
        (false, true) => 2,
        (false, false) => 3,
    }
}

/// Picks the installed icon families to search for Private Use Area glyphs.
/// Ranking then alphabetical order keeps one machine's font set mapping to one
/// cascade, so a rendering report describes a reproducible configuration.
pub fn symbol_fallbacks(installed: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut families: Vec<String> = installed
        .into_iter()
        .filter(|family| family.to_lowercase().contains(SYMBOL_FAMILY_MARKER))
        .collect();
    families.sort_unstable();
    families.dedup();
    families.sort_by_key(|family| fallback_rank(family));
    families.truncate(MAX_DETECTED_FALLBACKS);
    families
}

impl Default for Config {
    fn default() -> Self {
        let (monospace, ui) = if cfg!(target_os = "linux") {
            ("DejaVu Sans Mono", "DejaVu Sans")
        } else {
            ("Menlo", ".SystemUIFont")
        };
        let font = |family: &str, size| FontConfig {
            family: family.into(),
            size,
            fallbacks: None,
        };
        Self {
            theme: "Default".into(),
            github: GitHubConfig::default(),
            confirm_close_tab: true,
            show_agents: true,
            show_system_load: true,
            contrast: Contrast::default(),
            usage: crate::usage::UsageConfig::default(),
            option_as_alt: OptionAsAlt::default(),
            open_links_in: LinkTarget::default(),
            keep_selection_after_copy: true,
            features: Features::default(),
            notifications: NotificationConfig::default(),
            notification_overrides: NotificationSettings::default(),
            clipboard_toast: ClipboardToast::default(),
            bell: BellConfig::default(),
            layout: Layout::default(),
            sidebar_layout: SidebarLayout::default(),
            keybindings: Keymap::default(),
            keybinding_overrides: BTreeMap::new(),
            devices: BTreeMap::new(),
            unknown_keys: Vec::new(),
            palette: crate::palette::PaletteConfig::default(),
            sidebar: font(monospace, 12.0),
            // Tabs are terminal chrome, so they read in the monospace face the
            // sidebar and terminal use, as they do in the reference UI.
            tabs: font(monospace, 12.0),
            terminal: font(monospace, 14.0),
            ui: font(ui, 12.0),
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Settings {
    theme: Option<String>,
    confirm_close_tab: Option<bool>,
    show_agents: Option<bool>,
    show_system_load: Option<bool>,
    contrast: Contrast,
    usage: crate::usage::UsageConfig,
    option_as_alt: OptionAsAlt,
    open_links_in: LinkTarget,
    keep_selection_after_copy: Option<bool>,
    sidebar: FontSettings,
    tabs: FontSettings,
    terminal: FontSettings,
    ui: FontSettings,
    github: GitHubConfig,
    features: Features,
    notifications: NotificationSettings,
    clipboard_toast: ClipboardToastSettings,
    bell: BellConfig,
    layout: Layout,
    keybindings: BTreeMap<String, Binding>,
    devices: BTreeMap<String, DeviceSettings>,
    palette: crate::palette::PaletteConfig,
}

/// Each key overrides the daemon's answer on its own, so naming one of them
/// here does not silently reset the other to a GUI default.
#[derive(Default, Deserialize)]
#[serde(default)]
struct ClipboardToastSettings {
    enabled: Option<bool>,
    position: Option<ClipboardToastPosition>,
}

impl ClipboardToastSettings {
    fn resolve(self, base: ClipboardToast) -> ClipboardToast {
        ClipboardToast {
            enabled: self.enabled.unwrap_or(base.enabled),
            position: self.position.unwrap_or(base.position),
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct FontSettings {
    family: Option<String>,
    size: Option<f32>,
    fallback: Option<Vec<String>>,
}

/// Windows sets `USERPROFILE` rather than `HOME`, and upstream Herdr reads both.
pub(crate) fn home() -> Result<PathBuf> {
    let variable = |name| env::var_os(name).filter(|value: &std::ffi::OsString| !value.is_empty());
    variable("HOME")
        .or_else(|| {
            if cfg!(windows) {
                variable("USERPROFILE")
            } else {
                None
            }
        })
        .map(PathBuf::from)
        .ok_or(Error::MissingHome)
}

/// The directory holding this app's `herdr` configuration directory. Upstream
/// Herdr puts it under `%APPDATA%` on Windows, and the GUI config lives beside
/// the daemon's, so the same root has to be used on both sides.
fn config_root() -> Result<PathBuf> {
    match env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        Some(value) => {
            let path = PathBuf::from(value);
            if !path.is_absolute() {
                return Err(Error::RelativeConfigRoot);
            }
            Ok(path)
        }
        None => {
            #[cfg(windows)]
            if let Some(roaming) = env::var_os("APPDATA").filter(|value| !value.is_empty()) {
                return Ok(PathBuf::from(roaming));
            }
            #[cfg(windows)]
            return Ok(home()?.join("AppData").join("Roaming"));
            #[cfg(not(windows))]
            Ok(home()?.join(".config"))
        }
    }
}

/// The daemon's own config file, resolved exactly as herdr resolves it. Every
/// GUI reader of those settings shares this one answer.
pub(crate) fn daemon_config_path(get: impl Fn(&str) -> Option<std::ffi::OsString>) -> PathBuf {
    if let Some(path) = get("HERDR_CONFIG_PATH") {
        return path.into();
    }
    let root = get("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            #[cfg(windows)]
            {
                if let Some(root) = get("APPDATA") {
                    return PathBuf::from(root);
                }
                get("HOME")
                    .or_else(|| get("USERPROFILE"))
                    .map(PathBuf::from)
                    .map(|home| home.join("AppData/Roaming"))
                    .unwrap_or_else(env::temp_dir)
            }
            #[cfg(not(windows))]
            get("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
                .unwrap_or_else(env::temp_dir)
        });
    // Share production TUI settings even in a debug GUI build or SSH session.
    root.join("herdr/config.toml")
}

/// What the GUI honors from the daemon's own config.
#[derive(Clone, Debug, Default)]
struct Daemon {
    clipboard_toast: ClipboardToast,
    keys: DaemonKeys,
    sidebar_layout: SidebarLayout,
}

/// A config file the GUI does not own can hold anything, including settings
/// from a newer herdr, so only the keys read here matter and anything
/// unreadable, oversized, malformed, or unrecognized leaves the defaults alone.
fn daemon_settings(path: &Path) -> Daemon {
    if fs::metadata(path).is_ok_and(|data| data.len() > MAX_DAEMON_CONFIG_BYTES) {
        return Daemon::default();
    }
    let Some(table) = fs::read_to_string(path)
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
    else {
        return Daemon::default();
    };
    Daemon {
        clipboard_toast: daemon_clipboard_toast(&table),
        keys: DaemonKeys::from_table(table.get("keys").and_then(toml::Value::as_table)),
        sidebar_layout: SidebarLayout::from_daemon_config(&table).unwrap_or_default(),
    }
}

fn daemon_clipboard_toast(table: &toml::Table) -> ClipboardToast {
    let mut resolved = ClipboardToast::default();
    let Some(clipboard) = table
        .get("ui")
        .and_then(|ui| ui.get("toast")?.get("clipboard")?.as_table())
    else {
        return resolved;
    };
    if let Some(enabled) = clipboard.get("enabled").and_then(toml::Value::as_bool) {
        resolved.enabled = enabled;
    }
    if let Some(position) = clipboard
        .get("position")
        .cloned()
        .and_then(|position| position.try_into().ok())
    {
        resolved.position = position;
    }
    resolved
}

fn theme_directories() -> Result<Vec<PathBuf>> {
    let root = config_root()?;
    let mut directories = vec![root.join("herdr/themes"), root.join("ghostty/themes")];
    if let Some(resources) = env::var_os("GHOSTTY_RESOURCES_DIR").filter(|value| !value.is_empty())
    {
        directories.push(PathBuf::from(resources).join("themes"));
    }
    directories.push(PathBuf::from(
        "/Applications/Ghostty.app/Contents/Resources/ghostty/themes",
    ));
    if let Some(data) = env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        directories.push(PathBuf::from(data).join("ghostty/themes"));
    } else if let Ok(home) = home() {
        directories.push(home.join(".local/share/ghostty/themes"));
    }
    let data_dirs = env::var_os("XDG_DATA_DIRS")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    directories.extend(env::split_paths(&data_dirs).map(|dir| dir.join("ghostty/themes")));
    Ok(directories)
}

impl Config {
    /// Pure application of a prepared shared snapshot. Native explicit keys win;
    /// system delivery posts OS notifications instead of in-app toasts, and
    /// terminal delivery has no outer terminal to reach from this GUI.
    pub(crate) fn apply_shared_notifications(&mut self, shared: &crate::herdr_settings::Settings) {
        self.notifications = self.notification_overrides.resolve(NotificationConfig {
            enabled: shared.toast_delivery == crate::herdr_settings::ToastDelivery::Herdr,
            system: shared.toast_delivery == crate::herdr_settings::ToastDelivery::System,
            delay_seconds: shared.toast_delay_seconds,
            position: shared.toast_position,
        });
    }

    pub fn path() -> Result<PathBuf> {
        Ok(config_root()?.join("herdr/config-gpui.toml"))
    }

    pub fn local_path() -> Result<PathBuf> {
        Ok(Self::path()?.with_extension("local.toml"))
    }

    /// Gives every face the config left alone an automatic icon-font cascade.
    /// `installed` is consulted only when some face still needs one, because
    /// enumerating system fonts is slow enough to keep off the UI thread.
    pub fn resolve_font_fallbacks<I>(&mut self, installed: impl FnOnce() -> I)
    where
        I: IntoIterator<Item = String>,
    {
        let faces = [
            &mut self.sidebar,
            &mut self.tabs,
            &mut self.terminal,
            &mut self.ui,
        ];
        if faces.iter().all(|face| face.fallbacks.is_some()) {
            return;
        }
        let detected = symbol_fallbacks(installed());
        for face in faces {
            if face.fallbacks.is_none() {
                face.fallbacks = Some(detected.clone());
            }
        }
    }

    /// A one-line warning naming the keys this build ignored, if any.
    pub(crate) fn diagnostic(&self) -> Option<String> {
        const LISTED: usize = 5;
        if self.unknown_keys.is_empty() {
            return None;
        }
        let listed = self.unknown_keys[..self.unknown_keys.len().min(LISTED)].join(", ");
        let more = match self.unknown_keys.len().saturating_sub(LISTED) {
            0 => String::new(),
            more => format!(" and {more} more"),
        };
        Some(format!(
            "config-gpui.local.toml: ignoring unknown keys {listed}{more}"
        ))
    }

    pub fn load() -> Result<Self> {
        Self::load_path(&Self::path()?, &daemon_config_path(|key| env::var_os(key)))
    }

    /// First-frame settings only: no lock, migration, writes, or fsync. The
    /// background load performs maintenance after the window has appeared.
    pub(crate) fn load_startup() -> Result<Self> {
        Self::load_startup_path(&Self::path()?, &daemon_config_path(|key| env::var_os(key)))
    }

    fn load_startup_path(path: &Path, daemon: &Path) -> Result<Self> {
        let local = path.with_extension("local.toml");
        let (text, source) = match fs::read_to_string(&local) {
            Ok(text) => (text, local),
            // Without overrides or a personal config to migrate, maintenance
            // will seed the first-launch overrides; show them from frame one.
            Err(error) if error.kind() == ErrorKind::NotFound => match fs::read_to_string(path) {
                Ok(text) if text.lines().next() != Some(MANAGED_HEADER) => (text, path.to_owned()),
                Ok(_) => (LOCAL_CONFIG.into(), local),
                Err(error) if error.kind() == ErrorKind::NotFound => (LOCAL_CONFIG.into(), local),
                Err(error) => return Err(Error::from(error).at_path(path)),
            },
            Err(error) => return Err(Error::from(error).at_path(&local)),
        };
        Self::parse_layers([DEFAULT_CONFIG, &text], &daemon_settings(daemon))
            .map_err(|error| error.at_path(&source))
    }

    /// `daemon` is the herdr config whose settings this GUI also honors. It is
    /// read for those keys alone and never written; a missing one is normal.
    fn load_path(path: &Path, daemon: &Path) -> Result<Self> {
        let base = daemon_settings(daemon);
        let (_lock, local) = Self::prepare_files(path)?;
        let text =
            fs::read_to_string(&local).map_err(|error| Error::from(error).at_path(&local))?;
        // Validate the override independently so bad types/unknown keys cannot
        // disappear inside the merge. Empty arrays explicitly replace defaults.
        Self::parse_over(&text, &base).map_err(|error| error.at_path(&local))?;
        Self::parse_layers([DEFAULT_CONFIG, &text], &base).map_err(|error| error.at_path(&local))
    }

    /// Serialize migration, defaults refresh, and theme saves across GUI windows
    /// and processes. This is only called by background config workers.
    fn prepare_files(path: &Path) -> Result<(fs::File, PathBuf)> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(|error| Error::from(error).at_path(parent))?;
        let lock_path = path.with_extension("lock");
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| Error::from(error).at_path(&lock_path))?;
        lock.lock()
            .map_err(|error| Error::from(error).at_path(&lock_path))?;
        let local = path.with_extension("local.toml");
        let original = match fs::read_to_string(path) {
            Ok(text) => Some(text),
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => return Err(Error::from(error).at_path(path)),
        };
        let legacy = original
            .as_deref()
            .filter(|text| text.lines().next() != Some(MANAGED_HEADER));
        if let Some(text) = legacy {
            // Never replace an old user's file until its exact contents are
            // safely stored in the local file. A conflict needs human resolution.
            Self::parse_over(text, &Daemon::default()).map_err(|error| error.at_path(path))?;
        }
        match fs::read_to_string(&local) {
            Ok(text) if legacy.is_some_and(|legacy| legacy != text) => {
                return Err(Error::ConfigMigrationConflict {
                    original: path.into(),
                    local,
                });
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {
                let mut file = tempfile::NamedTempFile::new_in(parent)
                    .map_err(|error| Error::from(error).at_path(&local))?;
                file.write_all(legacy.unwrap_or(LOCAL_CONFIG).as_bytes())
                    .map_err(|error| Error::from(error).at_path(&local))?;
                file.as_file()
                    .sync_all()
                    .map_err(|error| Error::from(error).at_path(&local))?;
                file.persist_noclobber(&local)
                    .map_err(|error| Error::from(error.error).at_path(&local))?;
            }
            Err(error) => return Err(Error::from(error).at_path(&local)),
        }
        if legacy.is_some() {
            // Windows FlushFileBuffers requires write access, including when
            // resuming a migration whose local copy already exists. Never truncate.
            fs::OpenOptions::new()
                .write(true)
                .open(&local)
                .and_then(|file| file.sync_all())
                .map_err(|error| Error::from(error).at_path(&local))?;
            // Publish the migration copy durably before replacing the only old
            // copy. Windows does not expose directory sync through std::fs.
            #[cfg(unix)]
            fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| Error::from(error).at_path(parent))?;
        }
        if original.as_deref() != Some(DEFAULT_CONFIG) {
            write_config(path, DEFAULT_CONFIG)?;
        }
        Ok((lock, local))
    }

    /// The GUI file on its own, with nothing layered under it: the shape the
    /// tests below read, since loading also consults the daemon's config.
    #[cfg(test)]
    fn parse(text: &str) -> Result<Self> {
        Self::parse_over(text, &Daemon::default())
    }

    /// `base` is what the daemon's own config asked for, which every key this
    /// file names overrides.
    fn parse_over(text: &str, base: &Daemon) -> Result<Self> {
        Self::parse_layers([text], base)
    }

    fn parse_layers<'a>(texts: impl IntoIterator<Item = &'a str>, base: &Daemon) -> Result<Self> {
        let mut builder = config_loader::Config::builder();
        for text in texts {
            builder = builder.add_source(config_loader::File::from_str(
                text,
                config_loader::FileFormat::Toml,
            ));
        }
        let loaded = builder.build()?;
        // Config's typed deserializer coerces strings/numbers. Preserve TOML
        // types so existing strict font and theme validation remains intact.
        let value: toml::Value = loaded.try_deserialize()?;
        let mut unknown_keys = Vec::new();
        let mut settings: Settings = serde_ignored::deserialize(value, |path| {
            unknown_keys.push(path.to_string());
        })?;
        settings.keybindings.retain(|name, _| {
            let known = crate::controls::COMMANDS
                .iter()
                .any(|info| info.name == name);
            if !known {
                unknown_keys.push(format!("keybindings.{name}"));
            }
            known
        });
        unknown_keys.extend(settings.usage.retain_known());
        // Only catalog profile IDs name a device; anything else is ignored
        // and reported like any other unknown key.
        settings.devices.retain(|id, _| {
            let known = herdr_client::valid_profile_id(id);
            if !known {
                unknown_keys.push(format!("devices.{id}"));
            }
            known
        });
        // Unknown keys are ignored, but a credential pasted into the file is
        // refused so it is noticed and removed rather than left on disk.
        if let Some(name) = ["client_secret", "private_key", "token"]
            .into_iter()
            .find(|name| {
                unknown_keys
                    .iter()
                    .any(|key| key == &format!("github.{name}"))
            })
        {
            return Err(Error::GitHubSecretInConfig(name));
        }
        unknown_keys.sort();
        unknown_keys.dedup();
        let mut config = Self {
            unknown_keys,
            ..Self::default()
        };
        settings.github.client_id_with_override(None)?;
        config.github = settings.github;
        config.features = settings.features;
        config.notification_overrides = settings.notifications;
        config.notifications = settings
            .notifications
            .resolve(NotificationConfig::default());
        config.clipboard_toast = settings.clipboard_toast.resolve(base.clipboard_toast);
        config.bell = settings.bell;
        config.sidebar_layout = base.sidebar_layout.clone();
        if !settings.layout.sidebar_gap.is_finite()
            || !(0.0..=MAX_SIDEBAR_GAP).contains(&settings.layout.sidebar_gap)
        {
            return Err(Error::InvalidSidebarGap);
        }
        config.layout = settings.layout;
        config.keybindings = Keymap::with_overrides(&settings.keybindings, &base.keys)?;
        config.keybinding_overrides = settings.keybindings;
        if settings.devices.len() > MAX_DEVICES {
            return Err(Error::TooManyDevices(MAX_DEVICES));
        }
        config.devices = settings.devices;
        settings.palette.validate()?;
        config.palette = settings.palette;
        if let Some(theme) = settings.theme {
            if theme.trim().is_empty() {
                return Err(Error::EmptyTheme);
            }
            config.theme = theme;
        }
        config.confirm_close_tab = settings.confirm_close_tab.unwrap_or(true);
        config.show_agents = settings.show_agents.unwrap_or(true);
        config.show_system_load = settings.show_system_load.unwrap_or(true);
        config.contrast = settings.contrast;
        config.usage = settings.usage;
        config.option_as_alt = settings.option_as_alt;
        config.open_links_in = settings.open_links_in;
        config.keep_selection_after_copy = settings.keep_selection_after_copy.unwrap_or(true);
        for (name, font, settings) in [
            ("sidebar", &mut config.sidebar, settings.sidebar),
            ("tabs", &mut config.tabs, settings.tabs),
            ("terminal", &mut config.terminal, settings.terminal),
            ("ui", &mut config.ui, settings.ui),
        ] {
            if let Some(family) = settings.family {
                font.family = family;
            }
            if let Some(size) = settings.size {
                font.size = size;
            }
            if let Some(fallback) = settings.fallback {
                if fallback.len() > MAX_FONT_FALLBACKS {
                    return Err(Error::TooManyFontFallbacks(name));
                }
                if fallback.iter().any(|family| family.trim().is_empty()) {
                    return Err(Error::EmptyFontFallback(name));
                }
                font.fallbacks = Some(fallback);
            }
            if font.family.trim().is_empty() {
                return Err(Error::EmptyFontFamily(name));
            }
            if !font.size.is_finite() || !FONT_SIZE_RANGE.contains(&font.size) {
                return Err(Error::InvalidFontSize(name));
            }
        }
        Ok(config)
    }

    /// Discover names without parsing every theme. On failure, callers can use
    /// `Theme::BUILTIN_NAMES`, which remain loadable without any directories.
    pub fn available_themes(&self) -> Result<Vec<String>> {
        self.available_themes_in(&theme_directories()?)
    }

    fn available_themes_in(&self, directories: &[PathBuf]) -> Result<Vec<String>> {
        let mut names: Vec<String> = Theme::BUILTIN_NAMES
            .iter()
            .copied()
            .chain([FOLLOW_HERDR])
            .map(str::to_owned)
            .collect();
        for directory in directories {
            let entries = match fs::read_dir(directory) {
                Ok(entries) => entries,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => return Err(Error::from(error).at_path(directory)),
            };
            for entry in entries {
                let entry = entry.map_err(|error| Error::from(error).at_path(directory))?;
                // Follow symlinks just as the named theme loader does.
                let metadata = match fs::metadata(entry.path()) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == ErrorKind::NotFound => continue,
                    Err(error) => return Err(Error::from(error).at_path(&entry.path())),
                };
                if metadata.is_file()
                    && let Some(name) = entry.file_name().to_str()
                {
                    names.push(name.to_owned());
                }
            }
        }
        let selected = self.theme.trim();
        if Path::new(selected).is_absolute() || selected.starts_with("~/") {
            names.push(self.theme.clone());
        }
        names.sort_by_cached_key(|name| (name.to_lowercase(), name.clone()));
        names.dedup();
        Ok(names)
    }

    /// Persist only the theme selection, retaining the latest on-disk settings.
    pub fn save_theme(&self, name: &str) -> Result<()> {
        self.save_theme_at(name, &Self::path()?)
    }

    fn save_theme_at(&self, name: &str, path: &Path) -> Result<()> {
        let (_lock, local) = Self::prepare_files(path)?;
        self.save_theme_path(name, &local)
    }

    fn save_theme_path(&self, name: &str, path: &Path) -> Result<()> {
        let selected = Self {
            theme: name.into(),
            ..self.clone()
        };
        selected.theme()?;
        let result = (|| -> Result<()> {
            let text = match fs::read_to_string(path) {
                Ok(text) => text,
                Err(error) if error.kind() == ErrorKind::NotFound => LOCAL_CONFIG.into(),
                Err(error) => return Err(error.into()),
            };
            let mut document = text.parse::<toml_edit::DocumentMut>()?;
            let mut value = toml_edit::Value::from(name);
            if let Some(previous) = document.get("theme").and_then(toml_edit::Item::as_value) {
                *value.decor_mut() = previous.decor().clone();
            }
            document["theme"] = toml_edit::Item::Value(value);
            write_config(path, &document.to_string())?;
            Ok(())
        })();
        result.map_err(|error| error.at_path(path))
    }

    /// Persist only the sidebar layout, retaining the latest on-disk
    /// settings: a `layout = "..."` name is replaced in place, and a
    /// `[layout]` table gets its `mode`.
    pub fn save_layout(mode: LayoutMode) -> Result<()> {
        let (_lock, local) = Self::prepare_files(&Self::path()?)?;
        Self::save_layout_path(mode, &local)
    }

    fn save_layout_path(mode: LayoutMode, path: &Path) -> Result<()> {
        let result = (|| -> Result<()> {
            let text = match fs::read_to_string(path) {
                Ok(text) => text,
                Err(error) if error.kind() == ErrorKind::NotFound => LOCAL_CONFIG.into(),
                Err(error) => return Err(error.into()),
            };
            let mut document = text.parse::<toml_edit::DocumentMut>()?;
            match document.get_mut("layout") {
                Some(item) if item.is_table_like() => {
                    if let Some(layout) = item.as_table_like_mut() {
                        layout.insert("mode", toml_edit::value(mode.name()));
                    }
                }
                Some(toml_edit::Item::Value(named)) => {
                    let decor = named.decor().clone();
                    *named = toml_edit::Value::from(mode.name());
                    *named.decor_mut() = decor;
                }
                _ => {
                    document.insert("layout", toml_edit::value(mode.name()));
                }
            }
            write_config(path, &document.to_string())
        })();
        result.map_err(|error| error.at_path(path))
    }

    /// Persist a batch of logical pixel sizes without replacing other overrides.
    /// The lock also serializes this edit with migration and other GUI saves.
    pub(crate) fn save_font_sizes(sizes: &[(FontFace, f32)]) -> Result<()> {
        let (_lock, local) = Self::prepare_files(&Self::path()?)?;
        Self::save_font_sizes_path(sizes, &local)
    }

    /// Persist usage visibility without replacing provider settings.
    pub(crate) fn save_usage_visibility(show: bool) -> Result<()> {
        let (_lock, local) = Self::prepare_files(&Self::path()?)?;
        Self::save_usage_visibility_path(show, &local)
    }

    fn save_usage_visibility_path(show: bool, path: &Path) -> Result<()> {
        let result = (|| -> Result<()> {
            let text = match fs::read_to_string(path) {
                Ok(text) => text,
                Err(error) if error.kind() == ErrorKind::NotFound => LOCAL_CONFIG.into(),
                Err(error) => return Err(error.into()),
            };
            let mut document = text.parse::<toml_edit::DocumentMut>()?;
            let usage = document
                .entry("usage")
                .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
                .as_table_like_mut()
                .ok_or(Error::InvalidUsageTable)?;
            let mut value = toml_edit::Value::from(show);
            if let Some(previous) = usage.get("show").and_then(toml_edit::Item::as_value) {
                *value.decor_mut() = previous.decor().clone();
            }
            usage.insert("show", toml_edit::Item::Value(value));
            write_config(path, &document.to_string())
        })();
        result.map_err(|error| error.at_path(path))
    }

    /// Persist only the contrast setting, keeping the rest of the local file.
    pub(crate) fn save_contrast(contrast: Contrast) -> Result<()> {
        let (_lock, local) = Self::prepare_files(&Self::path()?)?;
        Self::save_contrast_path(contrast, &local)
    }

    fn save_contrast_path(contrast: Contrast, path: &Path) -> Result<()> {
        let result = (|| -> Result<()> {
            let text = match fs::read_to_string(path) {
                Ok(text) => text,
                Err(error) if error.kind() == ErrorKind::NotFound => LOCAL_CONFIG.into(),
                Err(error) => return Err(error.into()),
            };
            let mut document = text.parse::<toml_edit::DocumentMut>()?;
            let mut value = toml_edit::Value::from(contrast.name());
            if let Some(previous) = document.get("contrast").and_then(toml_edit::Item::as_value) {
                *value.decor_mut() = previous.decor().clone();
            }
            document["contrast"] = toml_edit::Item::Value(value);
            write_config(path, &document.to_string())
        })();
        result.map_err(|error| error.at_path(path))
    }

    /// Persist one device's keybinding source, keeping the rest of the local
    /// file. Local is the default, so choosing it removes the entry.
    pub(crate) fn save_device_keybindings(profile: &str, source: KeybindingSource) -> Result<()> {
        let (_lock, local) = Self::prepare_files(&Self::path()?)?;
        Self::save_device_keybindings_path(profile, source, &local)
    }

    fn save_device_keybindings_path(
        profile: &str,
        source: KeybindingSource,
        path: &Path,
    ) -> Result<()> {
        if !herdr_client::valid_profile_id(profile) {
            return Err(Error::InvalidDeviceId(profile.to_owned()));
        }
        let result = (|| -> Result<()> {
            let text = match fs::read_to_string(path) {
                Ok(text) => text,
                Err(error) if error.kind() == ErrorKind::NotFound => LOCAL_CONFIG.into(),
                Err(error) => return Err(error.into()),
            };
            let mut document = text.parse::<toml_edit::DocumentMut>()?;
            match source {
                KeybindingSource::Server => {
                    let mut devices = toml_edit::Table::new();
                    devices.set_implicit(true);
                    let device = document
                        .entry("devices")
                        .or_insert(toml_edit::Item::Table(devices))
                        .as_table_like_mut()
                        .ok_or(Error::InvalidDevicesTable)?
                        .entry(profile)
                        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
                        .as_table_like_mut()
                        .ok_or(Error::InvalidDevicesTable)?;
                    device.insert("keybindings", toml_edit::value("server"));
                }
                KeybindingSource::Local => {
                    let Some(devices) = document
                        .get_mut("devices")
                        .and_then(toml_edit::Item::as_table_like_mut)
                    else {
                        return Ok(());
                    };
                    if let Some(device) = devices
                        .get_mut(profile)
                        .and_then(toml_edit::Item::as_table_like_mut)
                    {
                        device.remove("keybindings");
                        if device.is_empty() {
                            devices.remove(profile);
                        }
                    }
                    if devices.is_empty() {
                        document.remove("devices");
                    }
                }
            }
            write_config(path, &document.to_string())
        })();
        result.map_err(|error| error.at_path(path))
    }

    /// Persist only the Agents section visibility, keeping the rest of the local file.
    pub(crate) fn save_show_agents(show: bool) -> Result<()> {
        let (_lock, local) = Self::prepare_files(&Self::path()?)?;
        Self::save_show_agents_path(show, &local)
    }

    fn save_show_agents_path(show: bool, path: &Path) -> Result<()> {
        let result = (|| -> Result<()> {
            let text = match fs::read_to_string(path) {
                Ok(text) => text,
                Err(error) if error.kind() == ErrorKind::NotFound => LOCAL_CONFIG.into(),
                Err(error) => return Err(error.into()),
            };
            let mut document = text.parse::<toml_edit::DocumentMut>()?;
            let mut value = toml_edit::Value::from(show);
            if let Some(previous) = document
                .get("show_agents")
                .and_then(toml_edit::Item::as_value)
            {
                *value.decor_mut() = previous.decor().clone();
            }
            document["show_agents"] = toml_edit::Item::Value(value);
            write_config(path, &document.to_string())
        })();
        result.map_err(|error| error.at_path(path))
    }

    /// `None` removes the local override, inheriting the platform's managed default.
    pub(crate) fn save_font_family(face: FontFace, family: Option<&str>) -> Result<()> {
        let (_lock, local) = Self::prepare_files(&Self::path()?)?;
        Self::save_font_family_path(face, family, &local)
    }

    pub(crate) fn save_all_font_families(family: Option<&str>) -> Result<()> {
        let (_lock, local) = Self::prepare_files(&Self::path()?)?;
        Self::save_font_families_path(
            &[
                FontFace::Sidebar,
                FontFace::Tabs,
                FontFace::Terminal,
                FontFace::Ui,
            ],
            family,
            &local,
        )
    }

    fn save_font_family_path(face: FontFace, family: Option<&str>, path: &Path) -> Result<()> {
        Self::save_font_families_path(&[face], family, path)
    }

    fn save_font_families_path(
        faces: &[FontFace],
        family: Option<&str>,
        path: &Path,
    ) -> Result<()> {
        if family.is_some_and(|name| name.trim().is_empty()) {
            return Err(Error::EmptyFontFamily("fonts"));
        }
        let result = (|| -> Result<()> {
            let text = fs::read_to_string(path)?;
            let mut document = text.parse::<toml_edit::DocumentMut>()?;
            for face in faces {
                if let Some(family) = family {
                    let font = document
                        .entry(face.name())
                        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
                    let table = font
                        .as_table_like_mut()
                        .ok_or(Error::EmptyFontFamily(face.name()))?;
                    let mut value = toml_edit::Value::from(family);
                    if let Some(previous) = table.get("family").and_then(toml_edit::Item::as_value)
                    {
                        *value.decor_mut() = previous.decor().clone();
                    }
                    table.insert("family", toml_edit::Item::Value(value));
                } else if let Some(table) = document
                    .get_mut(face.name())
                    .and_then(toml_edit::Item::as_table_like_mut)
                {
                    table.remove("family");
                }
            }
            write_config(path, &document.to_string())
        })();
        result.map_err(|error| error.at_path(path))
    }

    fn save_font_sizes_path(sizes: &[(FontFace, f32)], path: &Path) -> Result<()> {
        for &(face, size) in sizes {
            if !size.is_finite() || !FONT_SIZE_RANGE.contains(&size) {
                return Err(Error::InvalidFontSize(face.name()));
            }
        }
        let result = (|| -> Result<()> {
            let text = fs::read_to_string(path)?;
            let mut document = text.parse::<toml_edit::DocumentMut>()?;
            for &(face, size) in sizes {
                let font = document
                    .entry(face.name())
                    .or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
                let table = font
                    .as_table_like_mut()
                    .ok_or(Error::InvalidFontSize(face.name()))?;
                let mut value = toml_edit::Value::from(size as f64);
                if let Some(previous) = table.get("size").and_then(toml_edit::Item::as_value) {
                    *value.decor_mut() = previous.decor().clone();
                }
                table.insert("size", toml_edit::Item::Value(value));
            }
            write_config(path, &document.to_string())
        })();
        result.map_err(|error| error.at_path(path))
    }

    pub fn theme(&self) -> Result<Theme> {
        self.theme_with_directories(theme_directories)
            .map(|theme| theme.with_contrast(self.contrast))
    }

    fn theme_with_directories(
        &self,
        directories: impl FnOnce() -> Result<Vec<PathBuf>>,
    ) -> Result<Theme> {
        let name = self.theme.trim();
        if name == FOLLOW_HERDR {
            return crate::herdr_settings::Settings::load()?.theme(false);
        }
        if let Some(theme) = Theme::builtin(name) {
            return Ok(theme);
        }
        let path = if let Some(relative) = name.strip_prefix("~/") {
            home()?.join(relative)
        } else if Path::new(name).is_absolute() {
            PathBuf::from(name)
        } else {
            if name.is_empty()
                || Path::new(name).components().count() != 1
                || !matches!(
                    Path::new(name).components().next(),
                    Some(Component::Normal(_))
                )
            {
                return Err(Error::InvalidThemePath);
            }
            let directories = directories()?;
            let mut found = None;
            for directory in &directories {
                let candidate = directory.join(name);
                match fs::metadata(&candidate) {
                    Ok(metadata) if metadata.is_file() => {
                        found = Some(candidate);
                        break;
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == ErrorKind::NotFound => {}
                    Err(error) => return Err(Error::from(error).at_path(&candidate)),
                }
            }
            found.ok_or_else(|| Error::ThemeNotFound {
                name: name.into(),
                directories,
            })?
        };
        let text = fs::read_to_string(&path).map_err(|error| Error::from(error).at_path(&path))?;
        Theme::parse_ghostty(&text).map_err(|error| error.at_path(&path))
    }
}

fn write_config(path: &Path, text: &str) -> Result<()> {
    let result = (|| -> std::io::Result<()> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(text.as_bytes())?;
        file.as_file().sync_all()?;
        file.persist(path).map_err(|error| error.error)?;
        Ok(())
    })();
    result.map_err(|error| Error::from(error).at_path(path))
}

/// Colors are packed 24-bit RGB, without an alpha channel.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub background: u32,
    pub foreground: u32,
    pub cursor: u32,
    pub surface: u32,
    pub active: u32,
    pub muted: u32,
    /// Herdr's optional `sidebar_bg`, which colors only the sidebar. Unset,
    /// the sidebar stays on [`Self::surface`].
    pub sidebar: Option<u32>,
    pub palette: [u32; 256],
    /// Applied by [`Theme::with_contrast`]; every theme loads as `Standard`.
    pub contrast: Contrast,
}

impl Default for Theme {
    fn default() -> Self {
        let mut palette = [0; 256];
        palette[..16].copy_from_slice(&[
            0x000000, 0x800000, 0x008000, 0x808000, 0x000080, 0x800080, 0x008080, 0xc0c0c0,
            0x808080, 0xff0000, 0x00ff00, 0xffff00, 0x0000ff, 0xff00ff, 0x00ffff, 0xffffff,
        ]);
        for (index, color) in palette.iter_mut().enumerate().skip(16) {
            let n = index as u32;
            *color = if n < 232 {
                let n = n - 16;
                let level = |v| if v == 0 { 0 } else { 55 + v * 40 };
                (level(n / 36) << 16) | (level(n / 6 % 6) << 8) | level(n % 6)
            } else {
                (8 + (n - 232) * 10) * 0x010101
            };
        }
        Self {
            background: 0x101419,
            foreground: 0xd8dee9,
            cursor: 0xd8dee9,
            surface: 0x1c1c22,
            active: 0x2b2933,
            muted: 0x827e91,
            sidebar: None,
            palette,
            contrast: Contrast::Standard,
        }
    }
}

/// `percent` of `over` blended onto `base`, per channel.
pub(crate) fn mix(base: u32, over: u32, percent: u32) -> u32 {
    let channel = |shift: u32| {
        let base = (base >> shift) & 255;
        let over = (over >> shift) & 255;
        (base * (100 - percent) + over * percent) / 100
    };
    (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

impl Theme {
    pub const BUILTIN_NAMES: &'static [&'static str] = &[
        "Default",
        "Nord",
        "Dracula",
        "Catppuccin Mocha",
        "Catppuccin Latte",
    ];

    /// The theme's primary accent, used for selection colors that must read as
    /// chosen rather than merely hovered.
    pub fn primary(&self) -> u32 {
        self.palette[5]
    }

    /// The sidebar's fill: Herdr's `sidebar_bg` when set, else the surface.
    pub fn sidebar_background(&self) -> u32 {
        self.sidebar.unwrap_or(self.surface)
    }

    /// Dimmed foreground for rows that are not the current one: upstream's
    /// subtext sits between its text and its muted overlay.
    pub fn subtext(&self) -> u32 {
        self.ink(mix(self.background, self.foreground, 78))
    }

    /// A configured `dim = true` token: the color faded toward the panel.
    pub fn dimmed(&self, color: u32) -> u32 {
        mix(self.surface, color, 55)
    }

    /// A wash of [`Self::primary`] over the chrome, for filled selections such
    /// as the current tab. Large areas of the full accent shout; this keeps the
    /// hue while staying quiet enough to sit behind text all day.
    pub fn primary_wash(&self) -> u32 {
        mix(self.surface, self.primary(), 22)
    }

    /// Whichever of the theme's two text colors contrasts more with `fill`.
    /// A fixed light-or-dark rule breaks on light themes, where the accent and
    /// the background sit on the same side of any threshold.
    pub fn text_on(&self, fill: u32) -> u32 {
        let luminance = |color: u32| {
            let channel = |shift: u32| ((color >> shift) & 255) as f32 / 255.;
            0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
        };
        let fill = luminance(fill);
        if (luminance(self.background) - fill).abs() >= (luminance(self.foreground) - fill).abs() {
            self.background
        } else {
            self.foreground
        }
    }

    /// `color` as a colored mark or label drawn on this theme's chrome: moved
    /// only as far as the contrast setting needs to read on the background,
    /// the surface, and a selected row, keeping its hue. Never for terminal
    /// cells, whose colors belong to the program that wrote them.
    pub fn ink(&self, color: u32) -> u32 {
        crate::contrast::ink_on_chrome(
            color,
            [self.background, self.surface, self.active],
            self.contrast.mark_ratio(),
        )
    }

    /// High contrast parts selected rows further from the surface and raises
    /// dim labels to text contrast. Standard leaves the theme as drawn.
    pub fn with_contrast(mut self, contrast: Contrast) -> Self {
        self.contrast = contrast;
        if contrast == Contrast::High {
            self.active = mix(self.active, self.foreground, 12);
            self.muted = self.ink(self.muted);
        }
        self
    }

    fn derive_chrome(&mut self) {
        let blend = |percent| mix(self.background, self.foreground, percent);
        self.surface = blend(5);
        self.active = blend(12);
        self.muted = blend(55);
    }

    pub(super) fn builtin(name: &str) -> Option<Self> {
        // Small hand-authored palettes; no external theme assets are bundled.
        let (background, foreground, ansi) = match name {
            "Default" => return Some(Self::default()),
            "Nord" => (
                0x2e3440,
                0xd8dee9,
                [
                    0x3b4252, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x88c0d0, 0xe5e9f0,
                    0x4c566a, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x8fbcbb, 0xeceff4,
                ],
            ),
            "Dracula" => (
                0x282a36,
                0xf8f8f2,
                [
                    0x21222c, 0xff5555, 0x50fa7b, 0xf1fa8c, 0xbd93f9, 0xff79c6, 0x8be9fd, 0xf8f8f2,
                    0x6272a4, 0xff6e6e, 0x69ff94, 0xffffa5, 0xd6acff, 0xff92df, 0xa4ffff, 0xffffff,
                ],
            ),
            "Catppuccin Mocha" => (
                0x1e1e2e,
                0xcdd6f4,
                [
                    0x45475a, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xbac2de,
                    0x585b70, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xa6adc8,
                ],
            ),
            "Catppuccin Latte" => (
                0xeff1f5,
                0x4c4f69,
                [
                    0x5c5f77, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xacb0be,
                    0x6c6f85, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xbcc0cc,
                ],
            ),
            _ => return None,
        };
        let mut theme = Self {
            background,
            foreground,
            cursor: foreground,
            ..Self::default()
        };
        theme.palette[..16].copy_from_slice(&ansi);
        theme.derive_chrome();
        Some(theme)
    }

    fn parse_ghostty(text: &str) -> Result<Self> {
        let mut theme = Self::default();
        let mut cursor_set = false;
        for (index, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line.split_once('=').unwrap_or((line, ""));
            let key = key.trim();
            let value = value.trim();
            let error = |source| Error::ThemeLine {
                line: index + 1,
                key: key.into(),
                source,
            };
            let color = |value: &str| -> Result<u32> {
                let hex = value.strip_prefix('#').unwrap_or(value);
                if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err(error(ThemeParseError::InvalidColor));
                }
                u32::from_str_radix(hex, 16)
                    .map_err(|source| error(ThemeParseError::InvalidHex(source)))
            };
            match key {
                "background" => theme.background = color(value)?,
                "foreground" => theme.foreground = color(value)?,
                "cursor-color" => {
                    theme.cursor = color(value)?;
                    cursor_set = true;
                }
                "palette" => {
                    let (index, value) = value
                        .split_once('=')
                        .ok_or_else(|| error(ThemeParseError::MissingPaletteColor))?;
                    let index = index
                        .trim()
                        .parse::<usize>()
                        .map_err(|source| error(ThemeParseError::InvalidPaletteIndex(source)))?;
                    if index >= 256 {
                        return Err(error(ThemeParseError::PaletteIndexOutOfRange));
                    }
                    theme.palette[index] = color(value.trim())?;
                }
                _ => {} // Never interpret includes, commands, or unrelated Ghostty settings.
            }
        }
        if !cursor_set {
            theme.cursor = theme.foreground;
        }
        theme.derive_chrome();
        Ok(theme)
    }
}

#[cfg(test)]
mod tests;
