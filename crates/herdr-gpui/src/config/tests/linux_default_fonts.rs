use super::*;
use crate::config::fonts::{LINUX_MONOSPACE, LINUX_SANS, linux_default_substitute};
use std::collections::BTreeSet;

fn installed(families: &[&str]) -> BTreeSet<String> {
    families.iter().map(|family| (*family).to_owned()).collect()
}

#[test]
fn installed_defaults_are_kept() {
    let fonts = installed(&[LINUX_MONOSPACE, LINUX_SANS, "Noto Sans Mono"]);
    assert_eq!(linux_default_substitute(LINUX_MONOSPACE, &fonts), None);
    assert_eq!(linux_default_substitute(LINUX_SANS, &fonts), None);
}

#[test]
fn missing_defaults_take_the_first_known_alternative() {
    let fonts = installed(&["Ubuntu Mono", "Liberation Mono", "Cantarell", "Noto Sans"]);
    assert_eq!(
        linux_default_substitute(LINUX_MONOSPACE, &fonts).as_deref(),
        Some("Liberation Mono")
    );
    assert_eq!(
        linux_default_substitute(LINUX_SANS, &fonts).as_deref(),
        Some("Noto Sans")
    );
}

#[test]
fn missing_monospace_falls_back_to_an_installed_text_mono_family() {
    // Omarchy ships Nerd Font patched faces but no DejaVu or listed family.
    let fonts = installed(&[
        "Symbols Nerd Font Mono",
        "JetBrainsMono Nerd Font Propo",
        "JetBrainsMono Nerd Font Mono",
        "JetBrainsMono Nerd Font",
        "Noto Color Emoji",
    ]);
    assert_eq!(
        linux_default_substitute(LINUX_MONOSPACE, &fonts).as_deref(),
        Some("JetBrainsMono Nerd Font")
    );
    // With no sans family, the UI reads in monospace text, not a missing face.
    assert_eq!(
        linux_default_substitute(LINUX_SANS, &fonts).as_deref(),
        Some("JetBrainsMono Nerd Font")
    );
}

#[test]
fn chosen_and_unmatched_families_are_left_alone() {
    let fonts = installed(&["Noto Sans Mono"]);
    assert_eq!(linux_default_substitute("Iosevka", &fonts), None);
    assert_eq!(
        linux_default_substitute(LINUX_MONOSPACE, &installed(&["Zapfino"])),
        None
    );
}

#[cfg(target_os = "linux")]
#[test]
fn resolution_replaces_missing_linux_defaults_only() -> anyhow::Result<()> {
    let mut config = Config::parse("[terminal]\nfamily = 'Iosevka'")?;
    config.resolve_fonts(|| ["Noto Sans Mono".to_owned(), "Noto Sans".to_owned()]);
    assert_eq!(config.sidebar.family, "Noto Sans Mono");
    assert_eq!(config.tabs.family, "Noto Sans Mono");
    assert_eq!(config.terminal.family, "Iosevka");
    assert_eq!(config.ui.family, "Noto Sans");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
#[test]
fn resolution_keeps_families_off_linux() -> anyhow::Result<()> {
    let mut config = Config::default();
    let families = [&config.sidebar, &config.tabs, &config.terminal, &config.ui]
        .map(|face| face.family.clone());
    config.resolve_fonts(|| ["Noto Sans Mono".to_owned()]);
    assert_eq!(
        [&config.sidebar, &config.tabs, &config.terminal, &config.ui]
            .map(|face| face.family.clone()),
        families
    );
    Ok(())
}
