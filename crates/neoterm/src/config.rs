use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Full UI theme: terminal palette plus tab-strip chrome colors.
///
/// All color fields have Tokyo Night defaults, so old config files that only
/// set `background`/`foreground` keep parsing and inherit the rest.
/// `preset` selects a built-in pack (`"tokyo-night"`, `"catppuccin-mocha"`,
/// `"dracula"`, `"gruvbox-dark"`); `None` (or `""`) means fully custom.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    pub background: String,
    pub foreground: String,
    pub cursor: String,
    pub selection: String,
    /// 16 ANSI colors: black red green yellow blue magenta cyan white,
    /// then the 8 brights in the same order.
    pub palette: [String; 16],
    /// Strip background override. `None` (default) follows the body
    /// background: the Ghostty-style seamless strip. Set an explicit
    /// color for a contrasting band.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_bar_bg: Option<String>,
    pub tab_active_fg: String,
    /// Reserved: kept for file compat, unused by the seamless strip.
    pub tab_active_bg: String,
    /// Reserved: kept for file compat, unused by the seamless strip.
    pub tab_accent: String,
    pub tab_inactive_fg: String,
    pub tab_close_fg: String,
    pub scrollbar_track: String,
    pub scrollbar_thumb: String,
}

fn s(hex: &str) -> String {
    hex.to_string()
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            preset: None,
            background: s("#1a1b26"),
            foreground: s("#c0caf5"),
            cursor: s("#c0caf5"),
            selection: s("#33467c"),
            palette: [
                s("#414868"),
                s("#f7768e"),
                s("#9ece6a"),
                s("#e0af68"),
                s("#7aa2f7"),
                s("#bb9af7"),
                s("#7dcfff"),
                s("#a9b1d6"),
                s("#414868"),
                s("#f7768e"),
                s("#9ece6a"),
                s("#e0af68"),
                s("#7aa2f7"),
                s("#bb9af7"),
                s("#7dcfff"),
                s("#c0caf5"),
            ],
            tab_bar_bg: None,
            tab_active_fg: s("#7aa2f7"),
            tab_active_bg: s("#292e42"),
            tab_accent: s("#7aa2f7"),
            tab_inactive_fg: s("#565f89"),
            tab_close_fg: s("#737aa2"),
            scrollbar_track: s("#16161e"),
            scrollbar_thumb: s("#3b4261"),
        }
    }
}

impl Theme {
    /// Built-in preset packs. `None` = unknown name (caller warns, keeps
    /// the parsed custom values).
    pub fn preset(name: &str) -> Option<Self> {
        let mut t = Self::default();
        t.preset = Some(name.to_string());
        match name {
            "tokyo-night" => Some(t),
            "catppuccin-mocha" => {
                t.background = s("#1e1e2e");
                t.foreground = s("#cdd6f4");
                t.cursor = s("#f5e0dc");
                t.selection = s("#45475a");
                t.palette = [
                    s("#45475a"),
                    s("#f38ba8"),
                    s("#a6e3a1"),
                    s("#f9e2af"),
                    s("#89b4fa"),
                    s("#cba6f7"),
                    s("#94e2d5"),
                    s("#bac2de"),
                    s("#585b70"),
                    s("#f38ba8"),
                    s("#a6e3a1"),
                    s("#f9e2af"),
                    s("#89b4fa"),
                    s("#cba6f7"),
                    s("#94e2d5"),
                    s("#a6adc8"),
                ];
                t.tab_active_fg = s("#89b4fa");
                t.tab_active_bg = s("#313244");
                t.tab_accent = s("#89b4fa");
                t.tab_inactive_fg = s("#6c7086");
                t.tab_close_fg = s("#7f849c");
                t.scrollbar_track = s("#181825");
                t.scrollbar_thumb = s("#45475a");
                Some(t)
            }
            "dracula" => {
                t.background = s("#282a36");
                t.foreground = s("#f8f8f2");
                t.cursor = s("#f8f8f2");
                t.selection = s("#44475a");
                t.palette = [
                    s("#21222c"),
                    s("#ff5555"),
                    s("#50fa7b"),
                    s("#f1fa8c"),
                    s("#bd93f9"),
                    s("#ff79c6"),
                    s("#8be9fd"),
                    s("#f8f8f2"),
                    s("#6272a4"),
                    s("#ff6e6e"),
                    s("#69ff94"),
                    s("#ffffa5"),
                    s("#d6acff"),
                    s("#ff92df"),
                    s("#a4ffff"),
                    s("#ffffff"),
                ];
                t.tab_active_fg = s("#bd93f9");
                t.tab_active_bg = s("#44475a");
                t.tab_accent = s("#bd93f9");
                t.tab_inactive_fg = s("#6272a4");
                t.tab_close_fg = s("#6272a4");
                t.scrollbar_track = s("#21222c");
                t.scrollbar_thumb = s("#6272a4");
                Some(t)
            }
            "gruvbox-dark" => {
                t.background = s("#282828");
                t.foreground = s("#ebdbb2");
                t.cursor = s("#ebdbb2");
                t.selection = s("#504945");
                t.palette = [
                    s("#282828"),
                    s("#cc241d"),
                    s("#98971a"),
                    s("#d79921"),
                    s("#458588"),
                    s("#b16286"),
                    s("#689d6a"),
                    s("#a89984"),
                    s("#928374"),
                    s("#fb4934"),
                    s("#b8bb26"),
                    s("#fabd2f"),
                    s("#83a598"),
                    s("#d3869b"),
                    s("#8ec07c"),
                    s("#ebdbb2"),
                ];
                t.tab_active_fg = s("#fabd2f");
                t.tab_active_bg = s("#3c3836");
                t.tab_accent = s("#fabd2f");
                t.tab_inactive_fg = s("#928374");
                t.tab_close_fg = s("#928374");
                t.scrollbar_track = s("#1d2021");
                t.scrollbar_thumb = s("#504945");
                Some(t)
            }
            _ => None,
        }
    }

    pub fn palette_rgb(&self) -> [(u8, u8, u8); 16] {
        std::array::from_fn(|i| Self::parse_hex_u8(&self.palette[i]))
    }

    pub fn cursor_rgb(&self) -> (u8, u8, u8) {
        Self::parse_hex_u8(&self.cursor)
    }

    fn parse_hex_u8(s: &str) -> (u8, u8, u8) {
        NeotermConfig::parse_hex_u8(s)
    }

    fn glyphon_color(hex: &str) -> glyphon::Color {
        let (r, g, b) = Self::parse_hex_u8(hex);
        glyphon::Color::rgb(r, g, b)
    }

    fn quad_f32(hex: &str) -> [f32; 3] {
        let (r, g, b) = Self::parse_hex_u8(hex);
        [
            NeotermConfig::srgb8_to_linear(r),
            NeotermConfig::srgb8_to_linear(g),
            NeotermConfig::srgb8_to_linear(b),
        ]
    }

    pub fn selection_quad(&self) -> [f32; 3] {
        Self::quad_f32(&self.selection)
    }

    pub fn tab_bar_bg_quad(&self) -> [f32; 3] {
        match &self.tab_bar_bg {
            Some(hex) => Self::quad_f32(hex),
            None => Self::quad_f32(&self.background),
        }
    }

    pub fn scrollbar_track_quad(&self) -> [f32; 3] {
        Self::quad_f32(&self.scrollbar_track)
    }

    pub fn scrollbar_thumb_quad(&self) -> [f32; 3] {
        Self::quad_f32(&self.scrollbar_thumb)
    }

    /// Accent used for the active-tab underline (was reserved pre-v0.3).
    pub fn tab_accent_quad(&self) -> [f32; 3] {
        Self::quad_f32(&self.tab_accent)
    }

    pub fn tab_active_fg_glyphon(&self) -> glyphon::Color {
        Self::glyphon_color(&self.tab_active_fg)
    }

    pub fn tab_inactive_fg_glyphon(&self) -> glyphon::Color {
        Self::glyphon_color(&self.tab_inactive_fg)
    }

    pub fn tab_close_fg_glyphon(&self) -> glyphon::Color {
        Self::glyphon_color(&self.tab_close_fg)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NeotermConfig {
    pub font_family: String,
    pub font_size: f32,
    pub cols: u16,
    pub rows: u16,
    pub scrollback_lines: usize,
    pub shell: String,
    /// Tab-strip visibility: `"auto"` (show when 2+ tabs, Ghostty-style),
    /// `"always"`, or `"never"` (keybind-only tab switching).
    #[serde(default = "default_tab_bar")]
    pub tab_bar: String,
    /// Terminal bell: `"visual"` flashes the body, `"none"` ignores BEL.
    #[serde(default = "default_bell")]
    pub bell: String,
    pub theme: Theme,
}

fn default_tab_bar() -> String {
    "auto".to_string()
}

fn default_bell() -> String {
    "visual".to_string()
}

impl Default for NeotermConfig {
    fn default() -> Self {
        Self {
            font_family: "monospace".to_string(),
            font_size: 15.0,
            cols: 80,
            rows: 24,
            scrollback_lines: 10_000,
            shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string()),
            tab_bar: default_tab_bar(),
            bell: default_bell(),
            theme: Theme::default(),
        }
    }
}

impl NeotermConfig {
    /// Visual bell enabled? Anything other than `"none"` flashes.
    pub fn bell_enabled(&self) -> bool {
        !self.bell.eq_ignore_ascii_case("none")
    }

    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("neoterm").join("config.toml"))
    }

    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        if !path.exists() {
            let cfg = Self::default();
            let _ = cfg.save();
            return cfg;
        }
        let mut cfg: Self = match fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        };
        // Named presets fill the whole theme; `None`/`""` keeps the
        // parsed (possibly custom) values. Unknown names warn and keep
        // the parsed values (non-destructive).
        if let Some(name) = cfg.theme.preset.clone() {
            if !name.is_empty() {
                match Theme::preset(&name) {
                    Some(t) => {
                        log::info!("applying theme preset {name:?}");
                        cfg.theme = t;
                    }
                    None => log::warn!("unknown theme preset {name:?}, keeping custom theme"),
                }
            }
        }
        cfg
    }

    pub fn save(&self) -> Result<()> {
        let Some(path) = Self::path() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("create config dir")?;
        }
        let text = toml::to_string_pretty(self).context("serialize config")?;
        fs::write(&path, text).context("write config")?;
        Ok(())
    }

    pub fn parse_hex_color(s: &str) -> (f64, f64, f64) {
        let s = s.trim_start_matches('#');
        if s.len() != 6 {
            return (1.0, 1.0, 1.0);
        }
        let r = u8::from_str_radix(&s[0..2], 16).unwrap_or(255) as f64 / 255.0;
        let g = u8::from_str_radix(&s[2..4], 16).unwrap_or(255) as f64 / 255.0;
        let b = u8::from_str_radix(&s[4..6], 16).unwrap_or(255) as f64 / 255.0;
        (r, g, b)
    }

    fn parse_hex_u8(s: &str) -> (u8, u8, u8) {
        let s = s.trim_start_matches('#');
        let r = u8::from_str_radix(s.get(0..2).unwrap_or("ff"), 16).unwrap_or(255);
        let g = u8::from_str_radix(s.get(2..4).unwrap_or("ff"), 16).unwrap_or(255);
        let b = u8::from_str_radix(s.get(4..6).unwrap_or("ff"), 16).unwrap_or(255);
        (r, g, b)
    }

    /// sRGB 8-bit channel -> linear float. The swapchain is an `*Srgb`
    /// target, so shader/clear colors must be linear; feeding raw bytes
    /// washes the whole palette out (a #1a1b26 body would present as
    /// slate gray).
    pub fn srgb8_to_linear(v: u8) -> f32 {
        let c = v as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    /// Default foreground as 8-bit RGB (used when cells use `Named(Foreground)`).
    pub fn fg_rgb(&self) -> (u8, u8, u8) {
        Self::parse_hex_u8(&self.theme.foreground)
    }

    /// Default background as 8-bit RGB.
    pub fn bg_rgb(&self) -> (u8, u8, u8) {
        Self::parse_hex_u8(&self.theme.background)
    }

    pub fn bg_wgpu(&self) -> wgpu::Color {
        let (r, g, b) = Self::parse_hex_color(&self.theme.background);
        let lin = |c: f64| {
            let c = c as f32;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        wgpu::Color {
            r: lin(r) as f64,
            g: lin(g) as f64,
            b: lin(b) as f64,
            a: 1.0,
        }
    }

    pub fn fg_glyphon(&self) -> glyphon::Color {
        let s = self.theme.foreground.trim_start_matches('#');
        let r = u8::from_str_radix(s.get(0..2).unwrap_or("ff"), 16).unwrap_or(255);
        let g = u8::from_str_radix(s.get(2..4).unwrap_or("ff"), 16).unwrap_or(255);
        let b = u8::from_str_radix(s.get(4..6).unwrap_or("ff"), 16).unwrap_or(255);
        glyphon::Color::rgb(r, g, b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Old two-field theme files keep parsing and inherit the pack.
    #[test]
    fn legacy_theme_file_inherits_defaults() {
        let t: Theme = toml::from_str(
            "background = \"#000000\"\nforeground = \"#c0caf5\"\n",
        )
        .expect("legacy theme parses");
        assert_eq!(t.background, "#000000");
        assert_eq!(t.preset, None);
        assert_eq!(t.palette[4], "#7aa2f7");
        assert_eq!(t.tab_bar_bg, None);
    }

    #[test]
    fn presets_resolve_and_reject_unknown() {
        let d = Theme::preset("dracula").expect("dracula exists");
        assert_eq!(d.background, "#282a36");
        assert_eq!(d.palette[1], "#ff5555");
        assert!(Theme::preset("nope").is_none());
    }

    /// Files predating `tab_bar` keep parsing (field defaults to auto).
    /// The bell option is optional and defaults to the visual bell.
    #[test]
    fn bell_option_defaults_to_visual() {
        assert!(NeotermConfig::default().bell_enabled());

        let cfg: NeotermConfig = toml::from_str(
            r#"
            font_family = "monospace"
            font_size = 15.0
            cols = 80
            rows = 24
            scrollback_lines = 1000
            shell = "/bin/bash"
            bell = "none"

            [theme]
        "#,
        )
        .expect("config parses");
        assert!(!cfg.bell_enabled());
    }

    /// Legacy configs without a `bell` key keep the visual bell.
    #[test]
    fn legacy_config_without_bell_keeps_visual_bell() {
        let cfg: NeotermConfig = toml::from_str(
            r#"
            font_family = "monospace"
            font_size = 15.0
            cols = 80
            rows = 24
            scrollback_lines = 1000
            shell = "/bin/bash"

            [theme]
        "#,
        )
        .expect("config parses");
        assert!(cfg.bell_enabled());
    }

    /// The sRGB swapchain needs linear color values; a raw byte would
    /// wash the palette out.
    #[test]
    fn srgb_channels_linearize() {
        assert!((NeotermConfig::srgb8_to_linear(0) - 0.0).abs() < 1e-6);
        assert!((NeotermConfig::srgb8_to_linear(255) - 1.0).abs() < 1e-6);
        let expect = ((26.0f32 / 255.0 + 0.055) / 1.055).powf(2.4);
        assert!((NeotermConfig::srgb8_to_linear(26) - expect).abs() < 1e-6);
    }

    #[test]
    fn theme_quads_are_linear() {
        let q = Theme::default().selection_quad();
        assert!((q[0] - NeotermConfig::srgb8_to_linear(0x33)).abs() < 1e-6);
        assert!((q[2] - NeotermConfig::srgb8_to_linear(0x7c)).abs() < 1e-6);
    }

    #[test]
    fn legacy_config_without_tab_bar_parses() {
        let cfg: NeotermConfig = toml::from_str(
            "font_family = \"monospace\"\nfont_size = 15.0\ncols = 80\nrows = 24\n\
             scrollback_lines = 10000\nshell = \"/bin/bash\"\n\
             [theme]\nbackground = \"#000000\"\nforeground = \"#c0caf5\"\n",
        )
        .expect("legacy config parses");
        assert_eq!(cfg.tab_bar, "auto");
        assert_eq!(cfg.theme.background, "#000000");
        assert_eq!(cfg.theme.tab_bar_bg, None);
    }
}
