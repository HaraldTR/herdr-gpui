//! Native GUI settings; shared Herdr settings are read only when explicitly selected.
use crate::{Error, Result, error::ThemeParseError};
use gpui::{Font, FontFallbacks};
use serde::Deserialize;
use std::{
    env, fs,
    io::{ErrorKind, Write},
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const DEFAULT_CONFIG: &str = include_str!("../config-gpui.example.toml");
const FOLLOW_HERDR: &str = "Follow Herdr";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FontRole {
    Sidebar,
    Tabs,
    Terminal,
    Ui,
}

impl FontRole {
    pub(crate) const ALL: [Self; 4] = [Self::Sidebar, Self::Tabs, Self::Terminal, Self::Ui];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Sidebar => "Sidebar",
            Self::Tabs => "Tabs",
            Self::Terminal => "Terminal",
            Self::Ui => "UI",
        }
    }

    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Sidebar => "sidebar",
            Self::Tabs => "tabs",
            Self::Terminal => "terminal",
            Self::Ui => "ui",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub theme: String,
    pub confirm_close_tab: bool,
    pub show_agents: bool,
    pub sidebar: FontConfig,
    pub tabs: FontConfig,
    pub terminal: FontConfig,
    pub ui: FontConfig,
    pub github: GitHubConfig,
    pub features: Features,
}

/// Optional behaviors the config file turns on. Every flag is off by default,
/// so a missing or empty `[features]` table is the shipped experience.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Features {
    /// Open a space's menu when the pointer rests on its sidebar row.
    pub sidebar_hover_menu: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
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
            features: Features::default(),
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
#[serde(default, deny_unknown_fields)]
struct Settings {
    theme: Option<String>,
    confirm_close_tab: Option<bool>,
    show_agents: Option<bool>,
    sidebar: FontSettings,
    tabs: FontSettings,
    terminal: FontSettings,
    ui: FontSettings,
    github: GitHubConfig,
    features: Features,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FontSettings {
    family: Option<String>,
    size: Option<f32>,
    fallback: Option<Vec<String>>,
}

fn home() -> Result<PathBuf> {
    env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or(Error::MissingHome)
}

fn config_root() -> Result<PathBuf> {
    match env::var_os("XDG_CONFIG_HOME").filter(|value| !value.is_empty()) {
        Some(value) => {
            let path = PathBuf::from(value);
            if !path.is_absolute() {
                return Err(Error::RelativeConfigRoot);
            }
            Ok(path)
        }
        None => Ok(home()?.join(".config")),
    }
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
    pub(crate) fn font(&self, role: FontRole) -> &FontConfig {
        match role {
            FontRole::Sidebar => &self.sidebar,
            FontRole::Tabs => &self.tabs,
            FontRole::Terminal => &self.terminal,
            FontRole::Ui => &self.ui,
        }
    }

    pub fn path() -> Result<PathBuf> {
        Ok(config_root()?.join("herdr/config-gpui.toml"))
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

    pub fn load() -> Result<Self> {
        Self::load_path(&Self::path()?)
    }

    fn load_path(path: &Path) -> Result<Self> {
        let result = (|| {
            match fs::read_to_string(path) {
                Ok(text) => return Self::parse(&text),
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(mut file) => file.write_all(DEFAULT_CONFIG.as_bytes())?,
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
            Self::parse(&fs::read_to_string(path)?)
        })();
        result.map_err(|error| error.at_path(path))
    }

    fn parse(text: &str) -> Result<Self> {
        let loaded = config_loader::Config::builder()
            .add_source(config_loader::File::from_str(
                text,
                config_loader::FileFormat::Toml,
            ))
            .build()?;
        // Config's typed deserializer coerces strings/numbers. Preserve TOML
        // types so existing strict font and theme validation remains intact.
        let value: toml::Value = loaded.try_deserialize()?;
        let settings: Settings = value.try_into()?;
        let mut config = Self::default();
        settings.github.client_id_with_override(None)?;
        config.github = settings.github;
        config.features = settings.features;
        if let Some(theme) = settings.theme {
            if theme.trim().is_empty() {
                return Err(Error::EmptyTheme);
            }
            config.theme = theme;
        }
        config.confirm_close_tab = settings.confirm_close_tab.unwrap_or(true);
        config.show_agents = settings.show_agents.unwrap_or(true);
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
            if !font.size.is_finite() || !(8.0..=48.0).contains(&font.size) {
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
        self.save_theme_path(name, &Self::path()?)
    }

    fn save_theme_path(&self, name: &str, path: &Path) -> Result<()> {
        let selected = Self {
            theme: name.into(),
            ..self.clone()
        };
        selected.theme()?;
        Self::save_path(path, |document| {
            let mut value = toml_edit::Value::from(name);
            if let Some(previous) = document.get("theme").and_then(toml_edit::Item::as_value) {
                *value.decor_mut() = previous.decor().clone();
            }
            document["theme"] = toml_edit::Item::Value(value);
            Ok(())
        })
    }

    /// Persist only this face's family and size, leaving fallbacks unchanged.
    /// Synchronous disk I/O: call from a worker, not the UI thread.
    pub(crate) fn save_font(&self, role: FontRole, family: &str, size: f32) -> Result<()> {
        self.save_font_path(role, family, size, &Self::path()?)
    }

    fn save_font_path(&self, role: FontRole, family: &str, size: f32, path: &Path) -> Result<()> {
        let key = role.key();
        if family.trim().is_empty() {
            return Err(Error::EmptyFontFamily(key));
        }
        if !size.is_finite() || !(8.0..=48.0).contains(&size) {
            return Err(Error::InvalidFontSize(key));
        }
        Self::save_path(path, |document| {
            if !document.contains_key(key) {
                document[key] = toml_edit::Item::Table(toml_edit::Table::new());
            }
            let table = document[key].as_table_like_mut().ok_or_else(|| {
                Error::Toml(<toml::de::Error as serde::de::Error>::custom(format!(
                    "{key} must be a table"
                )))
            })?;
            for (field, mut value) in [
                ("family", toml_edit::Value::from(family)),
                ("size", toml_edit::Value::from(f64::from(size))),
            ] {
                if let Some(previous) = table.get(field).and_then(toml_edit::Item::as_value) {
                    *value.decor_mut() = previous.decor().clone();
                }
                table.insert(field, toml_edit::Item::Value(value));
            }
            Ok(())
        })
    }

    fn save_path(
        path: &Path,
        edit: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<()>,
    ) -> Result<()> {
        let result = (|| -> Result<()> {
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            fs::create_dir_all(parent)?;
            let parent = fs::canonicalize(parent)?;
            let denied = || {
                std::io::Error::new(
                    ErrorKind::PermissionDenied,
                    "native config saves require an owned, non-shared directory and regular files; symlink targets are refused",
                )
            };
            let uid = rustix::process::geteuid().as_raw();
            let metadata = fs::metadata(&parent)?;
            if metadata.uid() != uid || metadata.mode() & 0o022 != 0 {
                return Err(denied().into());
            }
            let name = path.file_name().ok_or_else(denied)?;
            let path = parent.join(name);
            let mut lock_name = std::ffi::OsString::from(".");
            lock_name.push(name);
            lock_name.push(".gpui-lock");
            // Never unlink this inode: waiters must lock the same file even after
            // the config is replaced. Closing the descriptor releases the lock.
            let lock = fs::File::from(
                rustix::fs::open(
                    parent.join(lock_name),
                    rustix::fs::OFlags::RDWR
                        | rustix::fs::OFlags::CREATE
                        | rustix::fs::OFlags::NOFOLLOW
                        | rustix::fs::OFlags::NONBLOCK
                        | rustix::fs::OFlags::CLOEXEC,
                    rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
                )
                .map_err(std::io::Error::from)?,
            );
            let validate_file = |metadata: fs::Metadata| -> std::io::Result<()> {
                if !metadata.is_file()
                    || metadata.uid() != uid
                    || metadata.nlink() != 1
                    || metadata.mode() & 0o022 != 0
                {
                    return Err(denied());
                }
                Ok(())
            };
            validate_file(lock.metadata()?)?;
            lock.lock()?;
            let check_target = || match fs::symlink_metadata(&path) {
                Ok(metadata) => validate_file(metadata),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            };
            check_target()?;
            let text = match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) if error.kind() == ErrorKind::NotFound => DEFAULT_CONFIG.into(),
                Err(error) => return Err(error.into()),
            };
            let mut document = text.parse::<toml_edit::DocumentMut>()?;
            edit(&mut document)?;
            static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
            let (temporary, mut file) = loop {
                let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
                let temporary =
                    parent.join(format!(".config-gpui-{}-{id}.tmp", std::process::id()));
                match fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temporary)
                {
                    Ok(file) => break (temporary, file),
                    Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(error.into()),
                }
            };
            let write_result = (|| {
                file.write_all(document.to_string().as_bytes())?;
                file.sync_all()?;
                drop(file);
                // Advisory locking coordinates native saves, not arbitrary editors.
                check_target()?;
                fs::rename(&temporary, &path)
            })();
            if let Err(error) = write_result {
                if let Err(cleanup) = fs::remove_file(&temporary) {
                    return Err(Error::Cleanup {
                        source: error,
                        path: temporary,
                        cleanup,
                    });
                }
                return Err(error.into());
            }
            Ok(())
        })();
        result.map_err(|error| error.at_path(path))
    }

    pub fn theme(&self) -> Result<Theme> {
        self.theme_with_directories(theme_directories)
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

/// Colors are packed 24-bit RGB, without an alpha channel.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub background: u32,
    pub foreground: u32,
    pub cursor: u32,
    pub surface: u32,
    pub active: u32,
    pub muted: u32,
    pub palette: [u32; 256],
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
            palette,
        }
    }
}

/// `percent` of `over` blended onto `base`, per channel.
fn mix(base: u32, over: u32, percent: u32) -> u32 {
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

    /// Dimmed foreground for rows that are not the current one: upstream's
    /// subtext sits between its text and its muted overlay.
    pub fn subtext(&self) -> u32 {
        mix(self.background, self.foreground, 78)
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
mod tests {
    use super::*;
    use anyhow::Context as _;

    #[test]
    fn primary_selection_text_contrasts_in_every_builtin_theme() {
        let luminance = |color: u32| {
            let channel = |shift: u32| ((color >> shift) & 255) as f32 / 255.;
            0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
        };
        for name in Theme::BUILTIN_NAMES {
            let theme = Theme::builtin(name).unwrap_or_else(|| panic!("missing theme {name}"));
            assert_eq!(
                theme.primary(),
                theme.palette[5],
                "{name}: accent is ANSI 5"
            );
            // The tab fill is the softened wash, not the raw accent.
            let primary = theme.primary_wash();
            let text = theme.text_on(primary);
            assert_ne!(primary, theme.surface, "{name}: selection must be visible");
            assert_ne!(
                primary, theme.active,
                "{name}: selection must outrank hover"
            );
            assert!(
                text == theme.background || text == theme.foreground,
                "{name}: text must be one of the theme's own colors"
            );
            let gap = (luminance(text) - luminance(primary)).abs();
            let other = if text == theme.background {
                theme.foreground
            } else {
                theme.background
            };
            assert!(gap >= 0.3, "{name}: unreadable selection, gap {gap}");
            assert!(
                gap >= (luminance(other) - luminance(primary)).abs(),
                "{name}: the other text color contrasts more"
            );
        }
    }

    #[test]
    fn errors_retain_paths_categories_and_parser_sources() -> anyhow::Result<()> {
        use std::error::Error as _;

        let temp = TempDirectory::new()?;
        let path = temp.0.join("invalid.toml");
        fs::write(&path, "theme = [")?;
        let error = Config::load_path(&path)
            .err()
            .ok_or_else(|| anyhow::anyhow!("accepted invalid TOML"))?;
        assert!(
            error
                .to_string()
                .starts_with(&format!("{}: ", path.display()))
        );
        let Error::Path {
            path: actual,
            source,
        } = error
        else {
            anyhow::bail!("missing path context");
        };
        assert_eq!(actual, path);
        assert!(matches!(source.as_ref(), Error::ConfigFile { .. }));
        assert!(source.source().is_some());
        assert!(matches!(
            Config::parse("[ui]\nsize = nan"),
            Err(Error::InvalidFontSize("ui"))
        ));

        let error = Theme::parse_ghostty("# ignored\npalette=bad=ffffff")
            .err()
            .ok_or_else(|| anyhow::anyhow!("accepted invalid palette index"))?;
        assert_eq!(
            error.to_string(),
            "line 2: palette: palette index must be between 0 and 255"
        );
        assert!(matches!(
            &error,
            Error::ThemeLine {
                line: 2,
                source: ThemeParseError::InvalidPaletteIndex(_),
                ..
            }
        ));
        assert!(
            error
                .source()
                .and_then(|source| source.source())
                .is_some_and(|source| source.is::<std::num::ParseIntError>())
        );
        assert!(matches!(
            Theme::parse_ghostty("palette=256=ffffff"),
            Err(Error::ThemeLine {
                source: ThemeParseError::PaletteIndexOutOfRange,
                ..
            })
        ));
        Ok(())
    }

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> std::io::Result<Self> {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            loop {
                let path = env::temp_dir().join(format!(
                    "herdr-theme-test-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Ok(Self(path)),
                    Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(error),
                }
            }
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn discovers_sorted_names_and_loads_in_precedence_order() -> anyhow::Result<()> {
        let temp = TempDirectory::new()?;
        let first = temp.0.join("first");
        let second = temp.0.join("second");
        fs::create_dir(&first)?;
        fs::create_dir(&second)?;
        fs::create_dir(first.join("not-a-theme"))?;
        for name in ["zebra", "alpha", "Nord"] {
            fs::write(first.join(name), "background=112233")?;
        }
        fs::write(second.join("Alpha"), "background=445566")?;
        fs::write(second.join("zebra"), "background=445566")?;
        let directories = vec![temp.0.join("missing"), first, second];
        let config = Config {
            theme: "alpha".into(),
            ..Config::default()
        };
        assert_eq!(
            config.available_themes_in(&directories)?,
            vec![
                "Alpha",
                "alpha",
                "Catppuccin Latte",
                "Catppuccin Mocha",
                "Default",
                "Dracula",
                "Follow Herdr",
                "Nord",
                "zebra",
            ]
        );
        assert_eq!(
            config
                .theme_with_directories(|| Ok(directories.clone()))?
                .background,
            0x112233
        );
        let builtin = Config {
            theme: "Nord".into(),
            ..Config::default()
        };
        assert_eq!(
            builtin.theme_with_directories(|| Ok(directories))?,
            Theme::builtin("Nord").context("missing builtin")?
        );
        Ok(())
    }

    #[test]
    fn discovery_includes_explicit_selection_and_reports_errors() -> anyhow::Result<()> {
        let temp = TempDirectory::new()?;
        for name in [
            temp.0.join("custom").to_string_lossy().into_owned(),
            "~/custom".into(),
        ] {
            let config = Config {
                theme: name.clone(),
                ..Config::default()
            };
            assert!(config.available_themes_in(&[])?.contains(&name));
        }
        let not_directory = temp.0.join("file");
        fs::write(&not_directory, "")?;
        assert!(
            Config::default()
                .available_themes_in(&[not_directory])
                .is_err()
        );
        for name in Theme::BUILTIN_NAMES {
            let config = Config {
                theme: (*name).into(),
                ..Config::default()
            };
            assert!(
                config
                    .theme_with_directories(|| Err(Error::MissingHome))
                    .is_ok()
            );
        }
        Ok(())
    }

    #[test]
    fn saves_only_font_fields_and_preserves_latest_settings_and_comments() -> anyhow::Result<()> {
        let temp = TempDirectory::new()?;
        let path = temp.0.join("config.toml");
        let config = Config::default();
        for (role, label) in FontRole::ALL
            .into_iter()
            .zip(["Sidebar", "Tabs", "Terminal", "UI"])
        {
            assert_eq!(role.label(), label);
            let original = format!(
                "# heading\ntheme = 'Nord' # selection\nfuture = true\n\n[{}] # fonts\nfamily = 'Old Font' # family\nsize = 19 # size\nfallback = ['Symbols Nerd Font Mono'] # keep cascade\nfuture_font = true\n\n[github]\noauth_client_id = 'Iv1.fixture'\n",
                role.key()
            );
            fs::write(&path, &original)?;
            config.save_font_path(role, "New Font", 20.5, &path)?;
            assert_eq!(
                fs::read_to_string(&path)?,
                original
                    .replace("'Old Font'", "\"New Font\"")
                    .replace("size = 19", "size = 20.5")
            );
            assert_ne!(config.font(role).family, "New Font");
            assert_ne!(config.font(role).size, 20.5);
            assert_eq!(fs::read_dir(&temp.0)?.count(), 2);
        }
        Ok(())
    }

    #[test]
    fn font_saves_create_missing_fields_and_preserve_inline_tables() -> anyhow::Result<()> {
        let temp = TempDirectory::new()?;
        let path = temp.0.join("nested/config.toml");
        let config = Config::default();
        let family = "Font \"Quoted\" \\ Face";
        config.save_font_path(FontRole::Terminal, family, 8.0, &path)?;
        let saved = Config::load_path(&path)?;
        assert_eq!(saved.terminal.family, family);
        assert_eq!(saved.terminal.size, 8.0);
        assert_eq!(saved.terminal.fallbacks, None);
        assert_eq!(saved.theme, "Default");
        for text in [
            "# no fonts\ntheme = 'Dracula'\n",
            "theme = 'Dracula'\n[ui]\nfallback = [] # no cascade\n",
            "theme = 'Dracula'\nui = { fallback = [] } # inline\n",
        ] {
            fs::write(&path, text)?;
            config.save_font_path(FontRole::Ui, family, 48.0, &path)?;
            let saved_text = fs::read_to_string(&path)?;
            let saved = Config::parse(&saved_text)?;
            assert_eq!(saved.font(FontRole::Ui).family, family);
            assert_eq!(saved.font(FontRole::Ui).size, 48.0);
            assert_eq!(saved.theme, "Dracula");
            if text.contains("fallback") {
                assert_eq!(saved.ui.fallbacks, Some(vec![]));
            } else {
                assert_eq!(saved.ui.fallbacks, None);
            }
            assert!(saved_text.contains(text.split_once('#').context("missing comment")?.1.trim()));
        }
        Ok(())
    }

    #[test]
    fn font_save_validation_leaves_files_unchanged() -> anyhow::Result<()> {
        let temp = TempDirectory::new()?;
        let path = temp.0.join("config.toml");
        let config = Config::default();
        for role in FontRole::ALL {
            for family in ["", " \t\n"] {
                assert!(matches!(
                    config.save_font_path(role, family, 12.0, &path),
                    Err(Error::EmptyFontFamily(key)) if key == role.key()
                ));
                assert!(!path.exists());
            }
            for size in [7.9, 48.1, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                assert!(matches!(
                    config.save_font_path(role, "Menlo", size, &path),
                    Err(Error::InvalidFontSize(key)) if key == role.key()
                ));
                assert!(!path.exists());
            }
        }
        for text in [
            "ui = [",
            "[ui]\nsize = 12\nsize = 14\n",
            "ui = 'not a table'",
            "ui = []",
            "[[ui]]\nsize = 12\n",
        ] {
            fs::write(&path, text)?;
            let error = config.save_font_path(FontRole::Ui, "Menlo", 12.0, &path);
            assert!(matches!(error, Err(Error::Path { path: actual, source })
                if actual == path && matches!(*source, Error::Toml(_) | Error::TomlEdit(_))));
            assert_eq!(fs::read_to_string(&path)?, text);
            assert_eq!(fs::read_dir(&temp.0)?.count(), 2);
        }
        Ok(())
    }

    #[test]
    fn saves_only_theme_and_preserves_latest_settings_and_comments() -> anyhow::Result<()> {
        let temp = TempDirectory::new()?;
        let path = temp.0.join("config.toml");
        let config = Config::default();
        // These on-disk settings differ from the in-memory snapshot, including
        // a setting this version does not understand.
        let original = "# heading\ntheme = 'Default' # selection\nfuture = true\n\n[tabs] # fonts\nsize = 19 # keep\n\n[github] # public only\noauth_client_id = 'Iv1.fixture' # keep ID\n";
        fs::write(&path, original)?;
        config.save_theme_path("Nord", &path)?;
        assert_eq!(
            fs::read_to_string(&path)?,
            original.replace("'Default'", "\"Nord\"")
        );
        assert_eq!(config.theme, "Default");
        assert_eq!(fs::read_dir(&temp.0)?.count(), 2);

        fs::write(
            &path,
            "# no theme\n[tabs]\nsize = 19\n[github]\noauth_client_id = 'Iv1.fixture'\n",
        )?;
        config.save_theme_path("Dracula", &path)?;
        let saved = fs::read_to_string(&path)?;
        let parsed = Config::parse(&saved)?;
        assert_eq!(parsed.theme, "Dracula");
        assert_eq!(parsed.tabs.size, 19.0);
        assert_eq!(
            parsed.github.oauth_client_id.as_deref(),
            Some("Iv1.fixture")
        );
        assert!(saved.contains("# no theme"));
        Ok(())
    }

    #[test]
    fn save_validates_theme_and_toml_before_writing() -> anyhow::Result<()> {
        let temp = TempDirectory::new()?;
        let path = temp.0.join("config.toml");
        let config = Config::default();
        let custom = temp.0.join("custom");
        fs::write(&custom, "background=invalid")?;
        let custom_name = custom.to_str().context("non-UTF8 temporary path")?;
        for name in ["", "../invalid", custom_name] {
            assert!(config.save_theme_path(name, &path).is_err());
            assert!(!path.exists());
        }
        for text in ["theme = [", "theme = 'Nord'\ntheme = 'Dracula'\n"] {
            fs::write(&path, text)?;
            assert!(config.save_theme_path("Nord", &path).is_err());
            assert_eq!(fs::read_to_string(&path)?, text);
            assert_eq!(fs::read_dir(&temp.0)?.count(), 3);
        }
        fs::write(&custom, "background=112233")?;
        let new_path = temp.0.join("nested/config.toml");
        config.save_theme_path(custom_name, &new_path)?;
        assert_eq!(Config::load_path(&new_path)?.theme()?.background, 0x112233);
        assert_eq!(
            fs::read_dir(new_path.parent().context("missing parent")?)?.count(),
            2
        );
        Ok(())
    }

    #[test]
    fn native_config_writer_child() -> anyhow::Result<()> {
        let Some(path) = env::var_os("HERDR_NATIVE_CONFIG_WRITER_TEST") else {
            return Ok(());
        };
        let path = PathBuf::from(path);
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path.with_file_name(".config.toml.gpui-lock"))?;
        assert!(matches!(lock.try_lock(), Err(fs::TryLockError::WouldBlock)));
        println!("native writer ready");
        std::io::stdout().flush()?;
        if env::var_os("HERDR_NATIVE_CONFIG_WRITER_THEME").is_some() {
            Config::default().save_theme_path("Dracula", &path)?;
        } else {
            Config::default().save_font_path(FontRole::Terminal, "Child Font", 21.0, &path)?;
        }
        Ok(())
    }

    #[test]
    fn concurrent_native_saves_read_after_acquiring_the_process_lock() -> anyhow::Result<()> {
        use std::{
            io::{BufRead as _, BufReader},
            process::{Child, Command, Stdio},
            sync::mpsc,
            time::{Duration, Instant},
        };

        struct Writer(Child);
        impl Drop for Writer {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        let temp = TempDirectory::new()?;
        let path = temp.0.join("config.toml");
        for theme in [false, true] {
            fs::write(&path, "theme = 'Nord'\n[sidebar]\nsize = 12\n")?;
            let mut writer = None;
            Config::save_path(&path, |document| {
                let mut command = Command::new(env::current_exe()?);
                command
                    .args([
                        "--exact",
                        "config::tests::native_config_writer_child",
                        "--nocapture",
                    ])
                    .env("HERDR_NATIVE_CONFIG_WRITER_TEST", &path)
                    .env_remove("HERDR_NATIVE_CONFIG_WRITER_THEME")
                    .stdout(Stdio::piped());
                if theme {
                    command.env("HERDR_NATIVE_CONFIG_WRITER_THEME", "1");
                }
                let mut child = Writer(command.spawn()?);
                let stdout = child
                    .0
                    .stdout
                    .take()
                    .ok_or_else(|| std::io::Error::other("missing child stdout"))?;
                let (ready_tx, ready_rx) = mpsc::channel();
                std::thread::spawn(move || {
                    for line in BufReader::new(stdout).lines() {
                        if line.is_ok_and(|line| line == "native writer ready") {
                            let _ = ready_tx.send(());
                        }
                    }
                });
                ready_rx
                    .recv_timeout(Duration::from_secs(10))
                    .map_err(|error| std::io::Error::new(ErrorKind::TimedOut, error))?;
                // The other process has proved this transaction owns the lock.
                // Its narrow edit must read the document only after this commits.
                document["sidebar"]["family"] = toml_edit::value("Parent Font");
                document["sidebar"]["size"] = toml_edit::value(18.0);
                writer = Some(child);
                Ok(())
            })?;
            let mut writer = writer.context("missing child")?;
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(status) = writer.0.try_wait()? {
                    assert!(status.success(), "child failed: {status}");
                    break;
                }
                anyhow::ensure!(Instant::now() < deadline, "child save timed out");
                std::thread::sleep(Duration::from_millis(10));
            }
            let saved = Config::load_path(&path)?;
            assert_eq!(saved.sidebar.family, "Parent Font");
            assert_eq!(saved.sidebar.size, 18.0);
            if theme {
                assert_eq!(saved.theme, "Dracula");
            } else {
                assert_eq!(saved.terminal.family, "Child Font");
                assert_eq!(saved.terminal.size, 21.0);
                assert_eq!(saved.theme, "Nord");
            }
        }
        Ok(())
    }

    #[test]
    fn native_saves_refuse_symlinks_and_release_locks_on_errors() -> anyhow::Result<()> {
        use std::os::unix::fs::symlink;

        let temp = TempDirectory::new()?;
        let target = temp.0.join("target.toml");
        let path = temp.0.join("config.toml");
        let config = Config::default();
        fs::write(&target, "theme = 'Nord'\n")?;
        symlink(&target, &path)?;
        assert!(config.save_theme_path("Dracula", &path).is_err());
        assert!(
            config
                .save_font_path(FontRole::Ui, "Menlo", 14.0, &path)
                .is_err()
        );
        assert!(fs::symlink_metadata(&path)?.file_type().is_symlink());
        assert_eq!(fs::read_to_string(&target)?, "theme = 'Nord'\n");
        fs::remove_file(&target)?;
        assert!(config.save_theme_path("Dracula", &path).is_err());
        assert!(!target.exists());
        fs::remove_file(&path)?;
        config.save_theme_path("Dracula", &path)?;
        assert_eq!(Config::load_path(&path)?.theme, "Dracula");

        let lock_path = temp.0.join(".config.toml.gpui-lock");
        fs::remove_file(&lock_path)?;
        symlink(&path, &lock_path)?;
        let original = fs::read_to_string(&path)?;
        assert!(config.save_theme_path("Nord", &path).is_err());
        assert_eq!(fs::read_to_string(&path)?, original);
        Ok(())
    }

    #[test]
    fn defaults_and_partial_settings() -> anyhow::Result<()> {
        // Sidebar, tabs, terminal, ui: only the status bar and modals are sans.
        #[cfg(target_os = "linux")]
        let families = [
            "DejaVu Sans Mono",
            "DejaVu Sans Mono",
            "DejaVu Sans Mono",
            "DejaVu Sans",
        ];
        #[cfg(not(target_os = "linux"))]
        let families = ["Menlo", "Menlo", "Menlo", ".SystemUIFont"];

        for config in [
            Config::default(),
            Config::parse("")?,
            Config::parse(DEFAULT_CONFIG)?,
        ] {
            assert_eq!(config.theme()?, Theme::default());
            assert!(config.github.oauth_client_id.is_none());
            // Every feature ships off, including in the example config.
            assert_eq!(config.features, Features::default());
            assert!(!config.features.sidebar_hover_menu);
            assert_eq!(config.terminal.line_height(), 20.0);
            for ((font, family), size) in [config.sidebar, config.tabs, config.terminal, config.ui]
                .into_iter()
                .zip(families)
                .zip([12.0, 12.0, 14.0, 12.0])
            {
                assert_eq!(font.family, family);
                assert_eq!(font.size, size);
            }
        }

        for settings in ["", "size = 18", "family = 'Custom Font'"] {
            let text = ["sidebar", "tabs", "terminal", "ui"]
                .map(|section| format!("[{section}]\n{settings}\n"))
                .join("\n");
            let config = Config::parse(&text)?;
            for ((font, family), size) in [config.sidebar, config.tabs, config.terminal, config.ui]
                .into_iter()
                .zip(families)
                .zip([12.0, 12.0, 14.0, 12.0])
            {
                assert_eq!(
                    font.family,
                    if settings.starts_with("family") {
                        "Custom Font"
                    } else {
                        family
                    }
                );
                assert_eq!(
                    font.size,
                    if settings.starts_with("size") {
                        18.0
                    } else {
                        size
                    }
                );
            }
        }
        Ok(())
    }

    #[test]
    fn appearance_and_close_options_preserve_defaults() -> anyhow::Result<()> {
        for config in [
            Config::default(),
            Config::parse("")?,
            Config::parse(DEFAULT_CONFIG)?,
        ] {
            assert!(config.confirm_close_tab);
            assert!(config.show_agents);
        }
        let config = Config::parse("confirm_close_tab = false\nshow_agents = false")?;
        assert!(!config.confirm_close_tab);
        assert!(!config.show_agents);
        assert!(Config::parse("confirm_close_tab = 'false'").is_err());
        assert!(Config::parse("show_agents = 0").is_err());
        Ok(())
    }

    #[test]
    fn rejects_invalid_settings() {
        for text in [
            "unknown = 1",
            "[sidebar]\nunknown = 1",
            "[unknown]",
            "theme = ''",
            "[ui]\nfamily = '  '",
            "[tabs]\nsize = 7.9",
            "[terminal]\nsize = 48.1",
            "[sidebar]\nsize = nan",
            "[sidebar]\nsize = inf",
            "[sidebar]\nsize = -inf",
            "[tabs]\nsize = '14'",
            "[tabs]\nfamily = 14",
            "[github]\nunknown = 'value'",
            "[github]\nclient_secret = 'not-allowed'",
            "[github]\nprivate_key = 'not-allowed'",
            "[github]\ntoken = 'not-allowed'",
            "[github]\noauth_client_id = 123",
            "[github]\noauth_client_id = ''",
            "[github]\noauth_client_id = ' bad-id'",
            "[github]\noauth_client_id = 'bad/id'",
            "[github]\noauth_client_id = '\u{e9}'",
            "[features]\nunknown = true",
            "[features]\nsidebar_hover_menu = 'true'",
            "[features]\nsidebar_hover_menu = 1",
        ] {
            assert!(Config::parse(text).is_err(), "accepted {text:?}");
        }
        assert!(Config::parse("[tabs]\nsize = 8\n[ui]\nsize = 48").is_ok());
    }

    #[test]
    fn features_are_opt_in_per_flag() -> anyhow::Result<()> {
        assert!(!Config::parse("[features]")?.features.sidebar_hover_menu);
        let config = Config::parse("[features]\nsidebar_hover_menu = true")?;
        assert!(config.features.sidebar_hover_menu);
        // Turning a flag on leaves the rest of the settings at their defaults.
        assert_eq!(config.theme, Config::default().theme);
        assert!(
            !Config::parse("[features]\nsidebar_hover_menu = false")?
                .features
                .sidebar_hover_menu
        );
        Ok(())
    }

    #[test]
    fn github_public_client_id_and_explicit_environment_precedence() -> anyhow::Result<()> {
        assert!(!Config::default().github.allow_plaintext_credentials);
        assert!(
            Config::parse("[github]\nallow_plaintext_credentials = true")?
                .github
                .allow_plaintext_credentials
        );
        assert!(Config::parse("[github]\nallow_plaintext_credentials = 'true'").is_err());
        let config = Config::parse("[github]\noauth_client_id = 'Iv1.fixture'")?;
        assert_eq!(
            config.github.client_id_with_override(None)?.as_deref(),
            Some("Iv1.fixture")
        );
        assert_eq!(
            config
                .github
                .client_id_with_override(Some("override-fixture".as_ref()))?
                .as_deref(),
            Some("override-fixture")
        );
        assert_eq!(
            config.github.oauth_client_id.as_deref(),
            Some("Iv1.fixture")
        );
        assert_eq!(
            Config::default()
                .github
                .client_id_with_override(None)?
                .as_deref(),
            Some("Iv23liurUcwxPjrdIFYT")
        );
        for id in [
            "",
            " ",
            "bad\nvalue",
            "bad/value",
            "\u{e9}",
            &"a".repeat(257),
        ] {
            assert!(matches!(
                config.github.client_id_with_override(Some(id.as_ref())),
                Err(Error::InvalidClientId("HERDR_GITHUB_OAUTH_CLIENT_ID"))
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            assert!(
                config
                    .github
                    .client_id_with_override(Some(std::ffi::OsStr::from_bytes(b"\xff")))
                    .is_err()
            );
        }
        Ok(())
    }

    #[test]
    fn default_palette_and_builtins() -> anyhow::Result<()> {
        let default = Theme::default();
        assert_eq!(default.palette[16], 0);
        assert_eq!(default.palette[21], 0x0000ff);
        assert_eq!(default.palette[231], 0xffffff);
        assert_eq!(default.palette[232], 0x080808);
        assert_eq!(default.palette[255], 0xeeeeee);
        assert_eq!(default.surface, 0x1c1c22);
        for name in ["Nord", "Dracula", "Catppuccin Mocha", "Catppuccin Latte"] {
            let theme = Config {
                theme: name.into(),
                ..Config::default()
            }
            .theme()?;
            assert_ne!(theme, default);
            assert_ne!(theme.surface, theme.background);
            assert_eq!(theme.palette[255], default.palette[255]);
        }
        Ok(())
    }

    #[test]
    fn ghostty_colors_and_ignored_settings() -> anyhow::Result<()> {
        let theme = Theme::parse_ghostty(
            "# comment\nbackground = #123aBC\nforeground=abcdef\n\
             palette = 0 = #010203\npalette=255=fefefe\npalette=0=040506\n\
             font-size = nonsense\nconfig-file = /do/not/read\nignored line",
        )?;
        assert_eq!(theme.background, 0x123abc);
        assert_eq!(theme.foreground, 0xabcdef);
        assert_eq!(theme.cursor, theme.foreground);
        assert_eq!(theme.palette[0], 0x040506);
        assert_eq!(theme.palette[255], 0xfefefe);
        assert_eq!(
            Theme::parse_ghostty("cursor-color=#ffffff")?.cursor,
            0xffffff
        );
        Ok(())
    }

    #[test]
    fn ghostty_errors_have_line_numbers() {
        for line in [
            "background=red",
            "foreground=#fff",
            "cursor-color=0x123456",
            "palette=256=ffffff",
            "palette=-1=ffffff",
            "palette=0=oops",
            "palette=ffffff",
            "background",
            "foreground=#12345678",
        ] {
            let result = Theme::parse_ghostty(&format!("# comment\n{line}"));
            assert!(
                matches!(result, Err(Error::ThemeLine { line: 2, .. })),
                "{result:?}"
            );
        }
    }

    #[test]
    fn creates_config_without_overwriting_and_loads_absolute_theme() -> anyhow::Result<()> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory =
            env::temp_dir().join(format!("herdr-config-{}-{unique}", std::process::id()));
        let path = directory.join("config-gpui.toml");
        let result = (|| {
            Config::load_path(&path)?;
            assert_eq!(fs::read_to_string(&path)?, DEFAULT_CONFIG);
            fs::write(&path, "theme = 'Nord'")?;
            assert_eq!(Config::load_path(&path)?.theme, "Nord");
            assert_eq!(fs::read_to_string(&path)?, "theme = 'Nord'");
            let theme_path = directory.join("custom-theme");
            fs::write(&theme_path, "background=112233")?;
            let config = Config {
                theme: theme_path.to_string_lossy().into_owned(),
                ..Config::default()
            };
            assert_eq!(config.theme()?.background, 0x112233);
            Ok(())
        })();
        fs::remove_dir_all(directory)?;
        result
    }

    /// Installed families as macOS reports them, in arbitrary order.
    fn installed() -> Vec<String> {
        [
            "Menlo",
            "Zapfino",
            "JetBrainsMono Nerd Font Propo",
            "Symbols Nerd Font",
            "Hack Nerd Font Mono",
            "Symbols Nerd Font Mono",
            "Agave Nerd Font Mono",
            "Hack Nerd Font Mono",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }

    #[test]
    fn detection_ranks_symbol_and_mono_faces_and_ignores_text_families() {
        // Symbols first, then single-cell Mono faces, alphabetical within each
        // rank, deduplicated, and capped so the cascade stays short.
        assert_eq!(
            symbol_fallbacks(installed()),
            [
                "Symbols Nerd Font Mono",
                "Symbols Nerd Font",
                "Agave Nerd Font Mono",
            ]
        );
        assert!(symbol_fallbacks(["Menlo".to_owned(), "Zapfino".to_owned()]).is_empty());
    }

    #[test]
    fn detection_fills_only_the_faces_the_config_left_alone() -> anyhow::Result<()> {
        let mut config = Config::parse("[terminal]\nfallback = ['Menlo']\n[ui]\nfallback = []")?;
        config.resolve_font_fallbacks(installed);
        assert_eq!(
            config.terminal.fallbacks.as_deref(),
            Some(["Menlo".to_owned()].as_slice())
        );
        // An explicit empty list opts out; it is not "unset".
        assert_eq!(config.ui.fallbacks.as_deref(), Some([].as_slice()));
        assert_eq!(config.ui.font().fallbacks, None);
        let detected = symbol_fallbacks(installed());
        assert_eq!(
            config.sidebar.fallbacks.as_deref(),
            Some(detected.as_slice())
        );
        assert_eq!(config.tabs.fallbacks.as_deref(), Some(detected.as_slice()));
        Ok(())
    }

    #[test]
    fn detection_does_not_enumerate_fonts_when_every_face_is_configured() -> anyhow::Result<()> {
        // Enumerating installed families is slow, so a fully configured file
        // must not pay for it.
        let mut config = Config::parse(
            "[sidebar]\nfallback = []\n[tabs]\nfallback = []\n\
             [terminal]\nfallback = []\n[ui]\nfallback = []",
        )?;
        config.resolve_font_fallbacks(|| -> Vec<String> { panic!("enumerated installed fonts") });
        Ok(())
    }

    #[test]
    fn configured_fallbacks_reach_the_shaping_font_in_order() -> anyhow::Result<()> {
        let config = Config::parse(
            "[terminal]\nfallback = ['Symbols Nerd Font Mono', 'Hack Nerd Font Mono']",
        )?;
        let font = config.terminal.font();
        assert_eq!(font.family, config.terminal.family);
        let fallbacks = font
            .fallbacks
            .ok_or_else(|| anyhow::anyhow!("missing cascade"))?;
        assert_eq!(
            fallbacks.fallback_list(),
            ["Symbols Nerd Font Mono", "Hack Nerd Font Mono"]
        );
        // The default face shapes without a cascade until one is resolved.
        assert_eq!(Config::default().terminal.font().fallbacks, None);
        Ok(())
    }

    #[test]
    fn fallback_lists_are_validated_per_face() {
        assert!(matches!(
            Config::parse("[terminal]\nfallback = ['Menlo', '  ']"),
            Err(Error::EmptyFontFallback("terminal"))
        ));
        let list = |count: usize| {
            (0..count)
                .map(|index| format!("'face{index}'"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        assert!(matches!(
            Config::parse(&format!(
                "[sidebar]\nfallback = [{}]",
                list(MAX_FONT_FALLBACKS + 1)
            )),
            Err(Error::TooManyFontFallbacks("sidebar"))
        ));
        assert!(
            Config::parse(&format!(
                "[sidebar]\nfallback = [{}]",
                list(MAX_FONT_FALLBACKS)
            ))
            .is_ok()
        );
    }
}
