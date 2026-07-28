//! Shared helpers for the doom-proto integration tests.

use std::path::PathBuf;

pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
}

/// Extract a `"key": "value"` string field from a single-line JSON object.
pub fn json_str<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\": \"");
    let start = line.find(&pat)? + pat.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// Extract a `"key": N` numeric field from a single-line JSON object.
pub fn json_num(line: &str, key: &str) -> Option<u64> {
    let pat = format!("\"{key}\": ");
    let start = line.find(&pat)? + pat.len();
    let rest = &line[start..];
    let end = rest.find([',', '}', ' '])?;
    rest[..end].parse().ok()
}

/// Extract a `"key": true|false` boolean field.
pub fn json_bool(line: &str, key: &str) -> Option<bool> {
    let pat = format!("\"{key}\": ");
    let start = line.find(&pat)? + pat.len();
    match line[start..].chars().next()? {
        't' => Some(true),
        'f' => Some(false),
        _ => None,
    }
}

// Each integration target reads a different subset of these fields.
#[allow(dead_code)]
pub struct PacketEntry {
    pub file: String,
    pub dir: String,
    pub length: u64,
    pub sha256: String,
    pub ptype: u64,
    pub reliable: bool,
}

pub fn manifest_entries(manifest: &str) -> Vec<PacketEntry> {
    let mut entries = Vec::new();
    for line in manifest.lines() {
        let line = line.trim();
        if !line.starts_with("{\"i\":") {
            continue;
        }
        entries.push(PacketEntry {
            file: json_str(line, "file").expect("packet file").to_string(),
            dir: json_str(line, "dir").expect("packet dir").to_string(),
            length: json_num(line, "length").expect("packet length"),
            sha256: json_str(line, "sha256").expect("packet sha256").to_string(),
            ptype: json_num(line, "type").expect("packet type"),
            reliable: json_bool(line, "reliable").expect("packet reliable"),
        });
    }
    entries
}
