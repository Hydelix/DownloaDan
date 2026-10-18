//! Theme security model
//! --------------------
//! A theme is DATA, never code. Anything that is not on the allowlist below is rejected (not "cleaned"), so a
//! theme file cannot carry CSS, URLs, scripts, images, fonts, HTML or paths. Validation runs in the backend on
//! every import and save, and the UI only ever applies the values as CSS custom properties.

use crate::model::{Colors, Shape, Theme};
use serde::Serialize;
use serde_json::{Map, Value};

pub const COLOR_KEYS: [&str; 11] = [
    "bg", "surface", "surface2", "border", "text", "textMuted", "accent", "accentText", "success", "warning", "danger",
];
pub const FONTS: [&str; 4] = ["system", "segoe", "mono", "serif"];
pub const DENSITIES: [&str; 3] = ["compact", "comfortable", "spacious"];
pub const MAX_BYTES: usize = 8 * 1024;
pub const FORMAT: &str = "downloadan-theme";
/// Theme files exported before the app was renamed.
const LEGACY_FORMAT: &str = "fluxdm-theme";
const TOP_KEYS: [&str; 7] = ["format", "version", "name", "author", "colors", "shape", "font"];

fn is_hex_color(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

fn clip(s: &str) -> String {
    s.chars().take(30).collect()
}

/// Control characters, bidi overrides and zero-width characters (used for spoofing) are stripped.
fn clean_text(v: Option<&Value>, max: usize) -> Option<String> {
    let s = v?.as_str()?;
    let cleaned: String = s
        .chars()
        .filter(|c| {
            let u = *c as u32;
            !(u < 0x20 || u == 0x7f || (0x202a..=0x202e).contains(&u) || (0x2066..=0x2069).contains(&u) || (0x200b..=0x200f).contains(&u))
        })
        .collect();
    let t = cleaned.trim();
    let n = t.chars().count();
    (n >= 1 && n <= max).then(|| t.to_string())
}

fn luminance(hex: &str) -> f64 {
    let c: Vec<f64> = [1usize, 3, 5]
        .iter()
        .map(|&i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0) as f64 / 255.0)
        .map(|v| if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) })
        .collect();
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

pub fn contrast(a: &str, b: &str) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    let (hi, lo) = if x >= y { (x, y) } else { (y, x) };
    (hi + 0.05) / (lo + 0.05)
}

/// Themes may not make the UI unreadable (prevents "invisible button" tricks).
const CONTRAST_RULES: [(&str, &str, f64); 6] = [
    ("text", "bg", 4.5),
    ("text", "surface", 4.5),
    ("textMuted", "bg", 3.0),
    ("textMuted", "surface", 3.0),
    ("accentText", "accent", 3.0),
    ("accent", "bg", 1.5),
];

fn whole_number(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.fract() == 0.0 && f.abs() < 1e9).map(|f| f as i64)),
        _ => None,
    }
}

/// Check a theme object. On success the returned theme is normalised (lower-case colours, defaults filled in)
/// and has an empty id.
pub fn validate_theme(input: &Value) -> Result<Theme, String> {
    let obj = input.as_object().ok_or("Theme must be a JSON object.")?;
    for k in obj.keys() {
        if !TOP_KEYS.contains(&k.as_str()) {
            return Err(format!("Unknown field \"{}\".", clip(k)));
        }
    }
    if let Some(f) = obj.get("format") {
        if f.as_str() != Some(FORMAT) && f.as_str() != Some(LEGACY_FORMAT) {
            return Err("Not a DownloaDan theme.".into());
        }
    }
    if let Some(v) = obj.get("version") {
        if whole_number(v) != Some(1) {
            return Err("Unsupported theme version.".into());
        }
    }

    let name = clean_text(obj.get("name"), 40).ok_or("Theme needs a name (1-40 characters).")?;
    let author = match obj.get("author") {
        None => None,
        Some(Value::String(s)) if s.is_empty() => None,
        Some(v) => Some(clean_text(Some(v), 40).ok_or("Author must be 1-40 characters.")?),
    };

    let colors_in = obj.get("colors").and_then(Value::as_object).ok_or("Missing \"colors\".")?;
    for k in colors_in.keys() {
        if !COLOR_KEYS.contains(&k.as_str()) {
            return Err(format!("Unknown color \"{}\".", clip(k)));
        }
    }
    let mut col = Map::new();
    for k in COLOR_KEYS {
        match colors_in.get(k).and_then(Value::as_str) {
            Some(v) if is_hex_color(v) => {
                col.insert(k.to_string(), Value::String(v.to_ascii_lowercase()));
            }
            _ => return Err(format!("Color \"{k}\" must look like #RRGGBB.")),
        }
    }
    let get = |k: &str| col.get(k).and_then(Value::as_str).unwrap_or("#000000").to_string();

    let empty = Map::new();
    let shape_in = match obj.get("shape") {
        None => &empty,
        Some(v) => v.as_object().ok_or("\"shape\" must be an object.")?,
    };
    for k in shape_in.keys() {
        if k != "radius" && k != "density" {
            return Err(format!("Unknown shape field \"{}\".", clip(k)));
        }
    }
    let radius = match shape_in.get("radius") {
        None => 10,
        Some(v) => match whole_number(v) {
            Some(r) if (0..=20).contains(&r) => r,
            _ => return Err("Radius must be a whole number 0-20.".into()),
        },
    };
    let density = match shape_in.get("density") {
        None => "comfortable".to_string(),
        Some(v) => match v.as_str() {
            Some(d) if DENSITIES.contains(&d) => d.to_string(),
            _ => return Err("Invalid density.".into()),
        },
    };
    let font = match obj.get("font") {
        None => "system".to_string(),
        Some(v) => match v.as_str() {
            Some(f) if FONTS.contains(&f) => f.to_string(),
            _ => return Err("Invalid font.".into()),
        },
    };

    for (fg, bg, min) in CONTRAST_RULES {
        if contrast(&get(fg), &get(bg)) < min {
            return Err(format!("Not enough contrast between \"{fg}\" and \"{bg}\" (needs {min}:1)."));
        }
    }
    Ok(Theme {
        id: String::new(),
        name,
        author,
        colors: Colors {
            bg: get("bg"),
            surface: get("surface"),
            surface2: get("surface2"),
            border: get("border"),
            text: get("text"),
            text_muted: get("textMuted"),
            accent: get("accent"),
            accent_text: get("accentText"),
            success: get("success"),
            warning: get("warning"),
            danger: get("danger"),
        },
        shape: Shape { radius: radius as u32, density },
        font,
    })
}

/// Parse a theme file's text. The size is checked BEFORE parsing.
pub fn parse_theme_text(text: &str) -> Result<Theme, String> {
    if text.len() > MAX_BYTES {
        return Err("Theme file is too large (max 8 KB).".into());
    }
    let value: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|_| "Not valid JSON.".to_string())?;
    validate_theme(&value)
}

#[derive(Serialize)]
struct ThemeFile<'a> {
    format: &'static str,
    version: u32,
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    author: Option<&'a str>,
    colors: &'a Colors,
    shape: &'a Shape,
    font: &'a str,
}

pub fn serialize_theme(t: &Theme) -> String {
    let file = ThemeFile { format: FORMAT, version: 1, name: &t.name, author: t.author.as_deref(), colors: &t.colors, shape: &t.shape, font: &t.font };
    serde_json::to_string_pretty(&file).unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
fn builtin(id: &str, name: &str, radius: u32, density: &str, c: [&str; 11]) -> Theme {
    Theme {
        id: id.into(),
        name: name.into(),
        author: Some(if id == "midnight" || id == "forest" { "DownloaDan" } else { "DownloaDan" }.into()),
        font: "system".into(),
        shape: Shape { radius, density: density.into() },
        colors: Colors {
            bg: c[0].into(),
            surface: c[1].into(),
            surface2: c[2].into(),
            border: c[3].into(),
            text: c[4].into(),
            text_muted: c[5].into(),
            accent: c[6].into(),
            accent_text: c[7].into(),
            success: c[8].into(),
            warning: c[9].into(),
            danger: c[10].into(),
        },
    }
}

pub fn builtin_themes() -> Vec<Theme> {
    vec![
        builtin("dark", "DownloaDan Dark", 10, "comfortable", ["#0e1014", "#161920", "#1e222b", "#2a2f3a", "#e7e9ee", "#8b92a3", "#4f8cff", "#ffffff", "#3ecf8e", "#f5a524", "#ff5d6c"]),
        builtin("light", "DownloaDan Light", 10, "comfortable", ["#f4f6fa", "#ffffff", "#eef1f6", "#dde2ec", "#1a1d24", "#5d6678", "#2563eb", "#ffffff", "#16a36a", "#c77700", "#d93a4a"]),
        builtin("midnight", "Midnight", 14, "comfortable", ["#0a0e1a", "#111733", "#182044", "#26305e", "#e6e9ff", "#8f98c9", "#7c6cff", "#ffffff", "#43d9a3", "#ffb547", "#ff6b81"]),
        builtin("forest", "Forest", 8, "compact", ["#0c1210", "#131c18", "#1a2621", "#26362f", "#e4efe9", "#85a094", "#2fbf71", "#04130b", "#5ee0a0", "#e8b64a", "#f0707a"]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn good() -> Value {
        json!({
            "format": "downloadan-theme", "version": 1, "name": "Mine", "author": "Me",
            "colors": { "bg": "#0e1014", "surface": "#161920", "surface2": "#1e222b", "border": "#2a2f3a", "text": "#E7E9EE", "textMuted": "#8b92a3",
                        "accent": "#4f8cff", "accentText": "#ffffff", "success": "#3ecf8e", "warning": "#f5a524", "danger": "#ff5d6c" },
            "shape": { "radius": 12, "density": "compact" }, "font": "mono"
        })
    }

    #[test]
    fn accepts_a_good_theme_and_normalises_it() {
        let t = validate_theme(&good()).unwrap();
        assert_eq!(t.name, "Mine");
        assert_eq!(t.colors.text, "#e7e9ee");
        assert_eq!(t.shape.radius, 12);
        assert_eq!(t.font, "mono");
    }

    #[test]
    fn builtins_pass_their_own_rules() {
        for t in builtin_themes() {
            let text = serialize_theme(&t);
            let back = parse_theme_text(&text).unwrap_or_else(|e| panic!("{}: {e}", t.id));
            assert_eq!(back.colors, t.colors);
        }
    }

    #[test]
    fn rejects_anything_that_is_not_on_the_allowlist() {
        let mut v = good();
        v["colors"]["bg"] = json!("url(http://evil)");
        assert!(validate_theme(&v).is_err());
        let mut v = good();
        v["css"] = json!("body{display:none}");
        assert!(validate_theme(&v).unwrap_err().starts_with("Unknown field"));
        let mut v = good();
        v["font"] = json!("Comic Sans");
        assert!(validate_theme(&v).is_err());
        let mut v = good();
        v["shape"]["radius"] = json!(99);
        assert!(validate_theme(&v).is_err());
        let mut v = good();
        v["colors"]["extra"] = json!("#000000");
        assert!(validate_theme(&v).unwrap_err().starts_with("Unknown color"));
    }

    #[test]
    fn rejects_unreadable_contrast() {
        let mut v = good();
        v["colors"]["text"] = json!("#0f1115"); // almost the background colour
        assert!(validate_theme(&v).unwrap_err().contains("contrast"));
    }

    #[test]
    fn size_is_checked_before_parsing() {
        let big = format!("{{\"name\":\"{}\"}}", "a".repeat(9000));
        assert_eq!(parse_theme_text(&big).unwrap_err(), "Theme file is too large (max 8 KB).");
        assert_eq!(parse_theme_text("not json").unwrap_err(), "Not valid JSON.");
    }

    #[test]
    fn spoofing_characters_are_stripped_from_names() {
        let mut v = good();
        v["name"] = json!("Night\u{202e}mode\u{200b}");
        assert_eq!(validate_theme(&v).unwrap().name, "Nightmode");
        let mut v = good();
        v["name"] = json!("\u{202e}");
        assert!(validate_theme(&v).is_err());
    }
}
