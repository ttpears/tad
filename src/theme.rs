//! Color themes for the dashboard. Built-in palettes by name; custom via
//! `~/.config/tad/config.yaml`.

use anyhow::{Context, Result};
use ratatui::style::Color;
use serde::Deserialize;

/// Canonical built-in theme names, listed in the order they appear in the
/// config picker. Aliases (e.g. "tokyo-night") aren't included.
pub const BUILTIN_THEMES: &[&str] = &[
    "tokyonight",
    "tokyonight-storm",
    "dracula",
    "nord",
    "gruvbox",
    "catppuccin",
    "solarized-dark",
    "onedark",
    "terminal",
];

/// Path to `~/.config/tad/config.yaml`.
pub fn config_path() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|p| p.join("tad").join("config.yaml"))
}

/// Read-modify-write the `theme:` field of `config.yaml`. Other keys are
/// preserved exactly. Creates the file (and parent dir) if missing.
pub fn save_theme_name(name: &str) -> Result<()> {
    let path = config_path().context("no config dir available")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let mut root: serde_yml::Value = match std::fs::read_to_string(&path) {
        Ok(text) if !text.trim().is_empty() => {
            serde_yml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?
        }
        _ => serde_yml::Value::Mapping(serde_yml::Mapping::new()),
    };
    let map = root
        .as_mapping_mut()
        .context("config.yaml root must be a mapping")?;
    map.insert("theme".into(), serde_yml::Value::String(name.to_string()));
    let text = serde_yml::to_string(&root)?;
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Read the currently configured theme name (if it's a named built-in, not
/// a custom inline mapping). Used to mark the active row in the picker.
pub fn current_name() -> Option<String> {
    let path = config_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    let cfg: AppConfig = serde_yml::from_str(&text).ok()?;
    match cfg.theme {
        ThemeSpec::Named(n) => Some(n),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub fg: Color,
    pub muted: Color,
    pub accent: Color,
    pub accent_bold: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub selection_bg: Color,
    pub border: Color,
}

/// Canonical names of built-in themes, picker order.
pub fn builtin_names() -> &'static [&'static str] {
    BUILTIN_THEMES
}

/// Theme by name (same aliases the loader accepts). None for unknown.
pub fn by_name(name: &str) -> Option<Theme> {
    builtin(name)
}

/// Built-in named themes. Add more by extending this match.
fn builtin(name: &str) -> Option<Theme> {
    Some(match name {
        "tokyonight" | "tokyo-night" => Theme {
            fg: rgb(0xa9, 0xb1, 0xd6),
            muted: rgb(0x56, 0x5f, 0x89),
            accent: rgb(0x7a, 0xa2, 0xf7),
            accent_bold: rgb(0xbb, 0x9a, 0xf7),
            success: rgb(0x9e, 0xce, 0x6a),
            warning: rgb(0xe0, 0xaf, 0x68),
            error: rgb(0xf7, 0x76, 0x8e),
            selection_bg: rgb(0x2a, 0x2e, 0x42),
            border: rgb(0x3b, 0x42, 0x61),
        },
        "tokyonight-storm" | "tokyo-night-storm" => Theme {
            fg: rgb(0xc0, 0xca, 0xf5),
            muted: rgb(0x56, 0x5f, 0x89),
            accent: rgb(0x7a, 0xa2, 0xf7),
            accent_bold: rgb(0xbb, 0x9a, 0xf7),
            success: rgb(0x9e, 0xce, 0x6a),
            warning: rgb(0xe0, 0xaf, 0x68),
            error: rgb(0xf7, 0x76, 0x8e),
            selection_bg: rgb(0x32, 0x44, 0x4a),
            border: rgb(0x41, 0x4a, 0x6d),
        },
        "dracula" => Theme {
            fg: rgb(0xf8, 0xf8, 0xf2),
            muted: rgb(0x62, 0x72, 0xa4),
            accent: rgb(0x8b, 0xe9, 0xfd),
            accent_bold: rgb(0xff, 0x79, 0xc6),
            success: rgb(0x50, 0xfa, 0x7b),
            warning: rgb(0xf1, 0xfa, 0x8c),
            error: rgb(0xff, 0x55, 0x55),
            selection_bg: rgb(0x44, 0x47, 0x5a),
            border: rgb(0x62, 0x72, 0xa4),
        },
        "nord" => Theme {
            fg: rgb(0xd8, 0xde, 0xe9),
            muted: rgb(0x4c, 0x56, 0x6a),
            accent: rgb(0x88, 0xc0, 0xd0),
            accent_bold: rgb(0xb4, 0x8e, 0xad),
            success: rgb(0xa3, 0xbe, 0x8c),
            warning: rgb(0xeb, 0xcb, 0x8b),
            error: rgb(0xbf, 0x61, 0x6a),
            selection_bg: rgb(0x3b, 0x42, 0x52),
            border: rgb(0x43, 0x4c, 0x5e),
        },
        "gruvbox" | "gruvbox-dark" => Theme {
            fg: rgb(0xeb, 0xdb, 0xb2),
            muted: rgb(0x92, 0x83, 0x74),
            accent: rgb(0x83, 0xa5, 0x98),
            accent_bold: rgb(0xd3, 0x86, 0x9b),
            success: rgb(0xb8, 0xbb, 0x26),
            warning: rgb(0xfa, 0xbd, 0x2f),
            error: rgb(0xfb, 0x49, 0x34),
            selection_bg: rgb(0x3c, 0x38, 0x36),
            border: rgb(0x50, 0x49, 0x45),
        },
        "catppuccin" | "catppuccin-mocha" => Theme {
            fg: rgb(0xcd, 0xd6, 0xf4),
            muted: rgb(0x6c, 0x70, 0x86),
            accent: rgb(0x89, 0xb4, 0xfa),
            accent_bold: rgb(0xcb, 0xa6, 0xf7),
            success: rgb(0xa6, 0xe3, 0xa1),
            warning: rgb(0xf9, 0xe2, 0xaf),
            error: rgb(0xf3, 0x8b, 0xa8),
            selection_bg: rgb(0x31, 0x32, 0x44),
            border: rgb(0x45, 0x47, 0x5a),
        },
        "solarized-dark" | "solarized" => Theme {
            fg: rgb(0x83, 0x94, 0x96),
            muted: rgb(0x58, 0x6e, 0x75),
            accent: rgb(0x26, 0x8b, 0xd2),
            accent_bold: rgb(0xd3, 0x36, 0x82),
            success: rgb(0x85, 0x99, 0x00),
            warning: rgb(0xb5, 0x89, 0x00),
            error: rgb(0xdc, 0x32, 0x2f),
            selection_bg: rgb(0x07, 0x36, 0x42),
            border: rgb(0x58, 0x6e, 0x75),
        },
        "onedark" | "one-dark" => Theme {
            fg: rgb(0xab, 0xb2, 0xbf),
            muted: rgb(0x5c, 0x63, 0x70),
            accent: rgb(0x61, 0xaf, 0xef),
            accent_bold: rgb(0xc6, 0x78, 0xdd),
            success: rgb(0x98, 0xc3, 0x79),
            warning: rgb(0xe5, 0xc0, 0x7b),
            error: rgb(0xe0, 0x6c, 0x75),
            selection_bg: rgb(0x3e, 0x44, 0x51),
            border: rgb(0x4b, 0x52, 0x63),
        },
        // Terminal-default — uses the user's 16-color palette.
        "terminal" | "default" => Theme {
            fg: Color::Reset,
            muted: Color::DarkGray,
            accent: Color::Cyan,
            accent_bold: Color::Magenta,
            success: Color::Green,
            warning: Color::Yellow,
            error: Color::Red,
            selection_bg: Color::DarkGray,
            border: Color::Gray,
        },
        _ => return None,
    })
}

/// Schema for ~/.config/tad/config.yaml:
///   theme: tokyonight              # named built-in
///   # or:
///   theme:
///     fg: "#a9b1d6"
///     accent: "#7aa2f7"
///     ...
#[derive(Debug, Deserialize, Default)]
#[serde(untagged)]
enum ThemeSpec {
    Named(String),
    Custom(CustomTheme),
    #[default]
    None,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct CustomTheme {
    fg: Option<String>,
    muted: Option<String>,
    accent: Option<String>,
    accent_bold: Option<String>,
    success: Option<String>,
    warning: Option<String>,
    error: Option<String>,
    selection_bg: Option<String>,
    border: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct AppConfig {
    #[serde(default)]
    theme: ThemeSpec,
}

pub fn load() -> Theme {
    let path = match dirs::config_dir() {
        Some(p) => p.join("tad").join("config.yaml"),
        None => return tokyonight(),
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return tokyonight(),
    };
    let cfg: AppConfig = serde_yml::from_str(&text).unwrap_or_default();
    match cfg.theme {
        ThemeSpec::Named(name) => builtin(&name).unwrap_or_else(|| {
            eprintln!("tad: unknown theme '{}', using tokyonight", name);
            tokyonight()
        }),
        ThemeSpec::Custom(c) => {
            let base = tokyonight();
            Theme {
                fg: c.fg.and_then(parse_color).unwrap_or(base.fg),
                muted: c.muted.and_then(parse_color).unwrap_or(base.muted),
                accent: c.accent.and_then(parse_color).unwrap_or(base.accent),
                accent_bold: c
                    .accent_bold
                    .and_then(parse_color)
                    .unwrap_or(base.accent_bold),
                success: c.success.and_then(parse_color).unwrap_or(base.success),
                warning: c.warning.and_then(parse_color).unwrap_or(base.warning),
                error: c.error.and_then(parse_color).unwrap_or(base.error),
                selection_bg: c
                    .selection_bg
                    .and_then(parse_color)
                    .unwrap_or(base.selection_bg),
                border: c.border.and_then(parse_color).unwrap_or(base.border),
            }
        }
        ThemeSpec::None => tokyonight(),
    }
}

fn tokyonight() -> Theme {
    builtin("tokyonight").expect("builtin 'tokyonight' theme must exist")
}

fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

/// Parse a "#RRGGBB" or "RRGGBB" hex string to a Color.
fn parse_color(s: String) -> Option<Color> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_name_resolves_via_by_name() {
        for name in builtin_names() {
            assert!(
                by_name(name).is_some(),
                "builtin_names() entry {:?} did not resolve via by_name()",
                name
            );
        }
    }

    #[test]
    fn by_name_accepts_known_alias() {
        assert!(by_name("tokyo-night").is_some());
    }

    #[test]
    fn by_name_unknown_is_none() {
        assert!(by_name("nope").is_none());
    }

    #[test]
    fn builtin_names_matches_public_const() {
        assert_eq!(builtin_names(), BUILTIN_THEMES);
    }
}
