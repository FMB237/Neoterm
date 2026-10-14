use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

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
}

fn default_tab_bar() -> String {
    "auto".to_string()
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
        }
    }
}

impl NeotermConfig {

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
        let cfg: Self = match fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        };
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

    /// Default foreground as 8-bit RGB.
    pub fn fg_rgb(&self) -> (u8, u8, u8) {
        (0xc0, 0xca, 0xf5)
    }

    /// Default background as 8-bit RGB.
    pub fn bg_rgb(&self) -> (u8, u8, u8) {
        (0x1a, 0x1b, 0x26)
    }

    pub fn bg_wgpu(&self) -> wgpu::Color {
        let (r, g, b) = Self::parse_hex_color("#1a1b26");
        wgpu::Color { r, g, b, a: 1.0 }
    }

    pub fn fg_glyphon(&self) -> glyphon::Color {
        glyphon::Color::rgb(0xc0, 0xca, 0xf5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;



}
