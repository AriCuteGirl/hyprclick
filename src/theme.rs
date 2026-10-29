//! Colors and font from the user's Noctalia rice (~/.config/noctalia), so the
//! window matches the bar. Falls back to the built-in "Ari" palette.

use eframe::egui::Color32;
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

pub struct Theme {
    /// Window background; translucent so Hyprland's blur shows through.
    pub bg: Color32,
    pub deep: Color32,
    pub card: Color32,
    pub card_hi: Color32,
    pub line: Color32,
    pub accent: Color32,
    pub accent2: Color32,
    /// Text drawn on top of `accent`.
    pub on_accent: Color32,
    pub text: Color32,
    pub weak: Color32,
    pub good: Color32,
    pub bad: Color32,
    pub font_regular: Option<PathBuf>,
    pub font_bold: Option<PathBuf>,
}

pub fn get() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(load)
}

const BG_ALPHA: f32 = 0.82;

fn load() -> Theme {
    let dir = dirs::config_dir().unwrap_or_default().join("noctalia");
    let palette = rice_value(&dir, "custom_palette")
        .and_then(|name| std::fs::read_to_string(dir.join("palettes").join(format!("{name}.json"))).ok())
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .map(|v| v["dark"].clone())
        .unwrap_or(Value::Null);

    let color = |key: &str, fallback: &str| parse_hex(palette[key].as_str().unwrap_or(fallback)).unwrap_or(Color32::MAGENTA);
    let surface = color("mSurface", "#1E1424");
    let on_variant = color("mOnSurfaceVariant", "#B7C0EE");
    let green = palette["terminal"]["normal"]["green"].as_str().and_then(parse_hex).unwrap_or(Color32::from_rgb(0x9c, 0xd8, 0xa6));

    let family = rice_value(&dir, "font_family")
        .and_then(|f| f.split(',').next().map(|s| s.trim().to_string()))
        .unwrap_or_else(|| "JetBrains Mono Nerd Font".into());

    Theme {
        bg: surface.gamma_multiply(BG_ALPHA),
        deep: mix(surface, Color32::BLACK, 0.35),
        card: color("mSurfaceVariant", "#2E1F36"),
        card_hi: color("mHover", "#3A2844"),
        line: color("mOutline", "#792359"),
        accent: color("mPrimary", "#E76D83"),
        accent2: on_variant,
        on_accent: color("mOnPrimary", "#1E1424"),
        text: color("mOnSurface", "#F3E8F0"),
        weak: mix(on_variant, surface, 0.35),
        good: green,
        bad: color("mError", "#FF6B6B"),
        font_regular: font_file(&family, "Regular"),
        font_bold: font_file(&family, "Bold"),
    }
}

/// First `key = "value"` found in the rice's *.toml files.
fn rice_value(dir: &PathBuf, key: &str) -> Option<String> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "toml")).collect();
    files.sort();
    files.iter().filter_map(|p| std::fs::read_to_string(p).ok()).find_map(|text| {
        text.lines().find_map(|line| {
            let (k, v) = line.split_once('=')?;
            if k.trim() != key {
                return None;
            }
            let v = v.trim();
            let v = v.strip_prefix('"')?;
            Some(v[..v.find('"')?].to_string())
        })
    })
}

fn font_file(family: &str, style: &str) -> Option<PathBuf> {
    let out = Command::new("fc-match").args(["-f", "%{file}", &format!("{family}:style={style}")]).output().ok()?;
    let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    // fc-match always answers; only take it if it found the family we asked for.
    let name = path.file_name()?.to_string_lossy().to_lowercase().replace([' ', '-', '_'], "");
    let want = family.to_lowercase().replace([' ', '-', '_'], "");
    let want = want.split("nerd").next().unwrap_or(&want);
    (path.exists() && name.contains(want)).then_some(path)
}

fn parse_hex(s: &str) -> Option<Color32> {
    let s = s.trim_start_matches('#');
    let v = u32::from_str_radix(s.get(..6)?, 16).ok()?;
    Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}
