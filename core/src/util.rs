//! Small helpers shared by the engine and the manager: ids, file names, formatting.

use rand::RngCore;
use std::path::Path;

pub const PART_EXT: &str = ".danpart";
/// Part files written by the old (FluxDM) versions; still counted as "taken" when picking a free name.
pub const LEGACY_PART_EXT: &str = ".fluxpart";

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// `d_1a2b3c4d5e6f` style ids (prefix + 12 hex digits).
pub fn new_id(prefix: &str) -> String {
    format!("{prefix}{}", random_hex(6))
}

/// URL-safe random token (24 random bytes, base64url, no padding), used to pair the browser extension.
pub fn new_token() -> String {
    use base64::Engine;
    let mut buf = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

/// Compare two strings without leaking, through timing, how many leading bytes matched.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Free space on the drive holding `path` (or the nearest folder that exists), in bytes.
pub fn free_space(path: &Path) -> Option<u64> {
    let mut p = path.to_path_buf();
    for _ in 0..12 {
        if p.exists() {
            break;
        }
        match p.parent() {
            Some(parent) => p = parent.to_path_buf(),
            None => break,
        }
    }
    fs4::available_space(&p).ok()
}

pub fn fmt_bytes(n: u64) -> String {
    if n == 0 {
        return "0 B".into();
    }
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let i = ((n as f64).ln() / 1024f64.ln()).floor().clamp(0.0, 4.0) as usize;
    let v = n as f64 / 1024f64.powi(i as i32);
    if i == 0 {
        format!("{} {}", n, U[0])
    } else {
        format!("{:.1} {}", v, U[i])
    }
}

/// Queue names: control characters and bidi overrides removed, trimmed, at most 40 characters.
pub fn clean_name(s: &str) -> String {
    let kept: String = s.chars().filter(|c| !is_control_or_bidi(*c)).collect();
    kept.trim().chars().take(40).collect()
}

fn is_control_or_bidi(c: char) -> bool {
    (c as u32) < 0x20 || ('\u{202a}'..='\u{202e}').contains(&c) || ('\u{2066}'..='\u{2069}').contains(&c)
}

/// The extension of a file name including the dot, with JavaScript's `path.extname` rules:
/// "a.tar.gz" -> ".gz", ".bashrc" -> "", "a." -> ".".
pub fn extname(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => &name[i..],
        _ => "",
    }
}

fn percent_decode_strict(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            if !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// File name offered by the server in a Content-Disposition header, if any.
fn name_from_disposition(cd: &str) -> Option<String> {
    let lower = cd.to_ascii_lowercase();
    // filename*=UTF-8''percent%20encoded
    if let Some(pos) = lower.find("filename*") {
        let rest = &cd[pos + "filename*".len()..];
        let rest = rest.trim_start();
        if let Some(rest) = rest.strip_prefix('=') {
            let rest = rest.trim_start();
            if rest.len() >= 7 && rest[..7].eq_ignore_ascii_case("utf-8''") {
                let raw = rest[7..].split(';').next().unwrap_or("").trim().trim_matches('"');
                if let Some(v) = percent_decode_strict(raw) {
                    if !v.is_empty() {
                        return Some(v);
                    }
                }
            }
        }
    }
    // filename="quoted" or filename=token
    let mut search_from = 0;
    while let Some(rel) = lower[search_from..].find("filename") {
        let at = search_from + rel;
        let after = &cd[at + "filename".len()..];
        search_from = at + "filename".len();
        if after.starts_with('*') {
            continue;
        }
        let after = after.trim_start();
        let Some(val) = after.strip_prefix('=') else { continue };
        let val = val.trim_start();
        if let Some(q) = val.strip_prefix('"') {
            if let Some(end) = q.find('"') {
                if end > 0 {
                    return Some(q[..end].trim().to_string());
                }
            }
            continue;
        }
        let tok = val.split(';').next().unwrap_or("").trim();
        if !tok.is_empty() {
            return Some(tok.to_string());
        }
    }
    None
}

/// Best guess at the file name: Content-Disposition first, then the last part of the URL path.
pub fn filename_from(content_disposition: Option<&str>, url: &str) -> String {
    if let Some(cd) = content_disposition {
        if let Some(n) = name_from_disposition(cd) {
            return n;
        }
    }
    if let Ok(u) = url::Url::parse(url) {
        if let Some(last) = u.path().split('/').filter(|s| !s.is_empty()).last() {
            if let Some(p) = percent_decode_strict(last) {
                if !p.is_empty() {
                    return p;
                }
            }
        }
    }
    "download".into()
}

/// Make a server-supplied name safe to use as a file name on Windows (and everywhere else).
pub fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !(('\u{202a}'..='\u{202e}').contains(c) || ('\u{2066}'..='\u{2069}').contains(c)))
        .map(|c| if (c as u32) < 0x20 || "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .collect();
    let mut n = cleaned.trim().trim_end_matches(|c| c == '.' || c == ' ').to_string();
    let stem = n.split('.').next().unwrap_or("").to_ascii_lowercase();
    let reserved = matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
        || (stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    if reserved {
        n = format!("_{n}");
    }
    if n.is_empty() {
        n = "download".into();
    }
    if n.chars().count() > 180 {
        let ext: String = extname(&n).chars().take(12).collect();
        let keep = 180 - ext.chars().count();
        n = n.chars().take(keep).collect::<String>() + &ext;
    }
    n
}

pub fn mime_ext(content_type: &str) -> Option<&'static str> {
    let t = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    Some(match t.as_str() {
        "video/mp4" => ".mp4",
        "video/webm" => ".webm",
        "video/x-matroska" => ".mkv",
        "video/quicktime" => ".mov",
        "audio/mpeg" => ".mp3",
        "audio/mp4" => ".m4a",
        "audio/ogg" => ".ogg",
        "audio/flac" => ".flac",
        "audio/wav" => ".wav",
        "application/pdf" => ".pdf",
        "application/zip" => ".zip",
        "image/jpeg" => ".jpg",
        "image/png" => ".png",
        _ => return None,
    })
}

/// Add an extension based on the content type when the name has none.
pub fn with_ext(name: &str, content_type: Option<&str>) -> String {
    if !extname(name).is_empty() {
        return name.to_string();
    }
    match content_type.and_then(mime_ext) {
        Some(ext) => format!("{name}{ext}"),
        None => name.to_string(),
    }
}

/// A name that does not collide with a finished file or a partial download in `dir`.
pub fn unique_name(dir: &Path, name: &str) -> String {
    let ext = extname(name);
    let base = &name[..name.len() - ext.len()];
    let taken = |n: &str| {
        dir.join(n).exists()
            || dir.join(format!("{n}{PART_EXT}")).exists()
            || dir.join(format!("{n}{LEGACY_PART_EXT}")).exists()
    };
    let mut n = name.to_string();
    let mut i = 0;
    while taken(&n) {
        i += 1;
        n = format!("{base} ({i}){ext}");
    }
    n
}

/// Name of a download without a " (2)" style suffix, for "is this the same file" checks.
pub fn same_name(a: &str, b: &str) -> bool {
    fn norm(s: &str) -> String {
        let s = s.to_lowercase();
        let ext = extname(&s).to_string();
        let base = &s[..s.len() - ext.len()];
        // strip one trailing " (n)" before the extension
        let stripped = match (base.rfind(" ("), base.ends_with(')')) {
            (Some(i), true) if base[i + 2..base.len() - 1].chars().all(|c| c.is_ascii_digit()) && base.len() - 1 > i + 2 => &base[..i],
            _ => base,
        };
        format!("{stripped}{ext}").trim().to_string()
    }
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_basics() {
        assert_eq!(sanitize_filename("a<b>c:d.txt"), "a_b_c_d.txt");
        assert_eq!(sanitize_filename("  name. . "), "name");
        assert_eq!(sanitize_filename("CON.txt"), "_CON.txt");
        assert_eq!(sanitize_filename("com5"), "_com5");
        assert_eq!(sanitize_filename("com10"), "com10");
        assert_eq!(sanitize_filename(""), "download");
        assert_eq!(sanitize_filename("a\u{202e}b.exe"), "ab.exe");
        let long = format!("{}.mp4", "x".repeat(300));
        let s = sanitize_filename(&long);
        assert_eq!(s.chars().count(), 180);
        assert!(s.ends_with(".mp4"));
    }

    #[test]
    fn names_from_headers_and_urls() {
        assert_eq!(filename_from(Some("attachment; filename=\"a b.zip\""), "http://x/y"), "a b.zip");
        assert_eq!(filename_from(Some("attachment; filename*=UTF-8''na%C3%AFve.txt"), "http://x/y"), "naïve.txt");
        assert_eq!(filename_from(Some("attachment; filename=plain.bin; size=3"), "http://x/y"), "plain.bin");
        assert_eq!(filename_from(None, "http://x/dir/file%20one.iso?x=1"), "file one.iso");
        assert_eq!(filename_from(None, "http://x/"), "download");
        // malformed percent-encoding in the URL falls back to a generic name
        assert_eq!(filename_from(None, "http://x/%E0%A4%A"), "download");
    }

    #[test]
    fn extension_rules() {
        assert_eq!(extname("a.tar.gz"), ".gz");
        assert_eq!(extname(".bashrc"), "");
        assert_eq!(extname("noext"), "");
        assert_eq!(with_ext("clip", Some("video/mp4; codecs=x")), "clip.mp4");
        assert_eq!(with_ext("clip.bin", Some("video/mp4")), "clip.bin");
    }

    #[test]
    fn unique_and_same_names() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(unique_name(d.path(), "f.txt"), "f.txt");
        std::fs::write(d.path().join("f.txt"), b"x").unwrap();
        assert_eq!(unique_name(d.path(), "f.txt"), "f (1).txt");
        std::fs::write(d.path().join("f (1).txt.danpart"), b"x").unwrap();
        assert_eq!(unique_name(d.path(), "f.txt"), "f (2).txt");
        assert!(same_name("Movie (2).mp4", "movie.mp4"));
        assert!(!same_name("a.mp4", "b.mp4"));
    }

    #[test]
    fn formatting_and_tokens() {
        assert_eq!(fmt_bytes(0), "0 B");
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(1536), "1.5 KB");
        assert_eq!(new_token().len(), 32);
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert_eq!(clean_name("  hi\u{202e}there \n"), "hithere");
    }
}
