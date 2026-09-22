//! Reading the Remote API settings from a TRACE32 config.t32 file.
//!
//! config.t32 consists of sections separated by blank lines; comments start
//! with `;`. The Remote API section looks like:
//!
//! ```text
//! RCL=NETTCP
//! PORT=20000
//! ```

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RclSettings {
    pub protocol: String,
    pub port: Option<u16>,
}

pub fn rcl_settings(path: &Path) -> Option<RclSettings> {
    parse_rcl_settings(&std::fs::read_to_string(path).ok()?)
}

pub fn parse_rcl_settings(source: &str) -> Option<RclSettings> {
    let mut lines = source.lines().map(str::trim);
    let protocol = lines.by_ref().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case("RCL")
            .then(|| value.trim().to_string())
    })?;
    let mut port = None;
    for line in lines {
        if line.is_empty() {
            break;
        }
        if line.starts_with(';') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim().eq_ignore_ascii_case("PORT") {
                port = value.trim().parse().ok();
            }
        }
    }
    Some(RclSettings { protocol, port })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_rcl_section() {
        let source = "OS=\nSYS=/t32\n\nPBI=\nUSB\n\n;API\nRCL=NETTCP\nPORT=20000\n\nSCREEN=\n";
        assert_eq!(
            parse_rcl_settings(source),
            Some(RclSettings {
                protocol: "NETTCP".into(),
                port: Some(20000)
            })
        );
    }

    #[test]
    fn missing_section_and_port() {
        assert_eq!(parse_rcl_settings("OS=\nSYS=/t32\n"), None);
        assert_eq!(
            parse_rcl_settings("RCL=NETASSIST\n\nPORT=1\n"),
            Some(RclSettings {
                protocol: "NETASSIST".into(),
                port: None
            })
        );
    }
}
