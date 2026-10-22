//! Reading the Remote API settings from a TRACE32 config.t32 file.
//!
//! config.t32 consists of sections separated by blank lines; comments start
//! with `;`. The Remote API section looks like:
//!
//! ```text
//! RCL=NETTCP
//! PORT=20000
//! REMOTEHOSTDENY
//! ```

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RclSettings {
    pub protocol: String,
    pub port: Option<u16>,
    /// `REMOTEHOSTDENY`: PowerView accepts Remote API clients from localhost
    /// only. Without it (or with `REMOTEHOSTALLOW`, the default) it listens on
    /// every network interface.
    pub remote_host_deny: bool,
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
    let mut remote_host_deny = false;
    for line in lines {
        if line.is_empty() {
            break;
        }
        if line.starts_with(';') {
            continue;
        }
        match line.split_once('=') {
            Some((key, value)) if key.trim().eq_ignore_ascii_case("PORT") => {
                port = value.trim().parse().ok();
            }
            Some(_) => {}
            None if line.eq_ignore_ascii_case("REMOTEHOSTDENY") => remote_host_deny = true,
            None if line.eq_ignore_ascii_case("REMOTEHOSTALLOW") => remote_host_deny = false,
            None => {}
        }
    }
    Some(RclSettings {
        protocol,
        port,
        remote_host_deny,
    })
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
                port: Some(20000),
                remote_host_deny: false,
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
                port: None,
                remote_host_deny: false,
            })
        );
    }

    #[test]
    fn remote_host_keys_inside_the_section_only() {
        let deny = parse_rcl_settings("RCL=NETTCP\nPORT=20000\nREMOTEHOSTDENY\n").unwrap();
        assert!(deny.remote_host_deny);
        let lower = parse_rcl_settings("RCL=NETTCP\nremotehostdeny\n").unwrap();
        assert!(lower.remote_host_deny);
        let allow = parse_rcl_settings("RCL=NETTCP\nREMOTEHOSTDENY\nREMOTEHOSTALLOW\n").unwrap();
        assert!(!allow.remote_host_deny);
        let other_section =
            parse_rcl_settings("RCL=NETTCP\nPORT=1\n\nIC=NETASSIST\nREMOTEHOSTDENY\n");
        assert!(!other_section.unwrap().remote_host_deny);
    }
}
