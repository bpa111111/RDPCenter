use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::net::{IpAddr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn strip_http_scheme(raw: &str) -> Option<(&str, &str, bool)> {
    if raw.get(..8).is_some_and(|s| s.eq_ignore_ascii_case("https://")) {
        Some((&raw[..8], &raw[8..], true))
    } else if raw.get(..7).is_some_and(|s| s.eq_ignore_ascii_case("http://")) {
        Some((&raw[..7], &raw[7..], false))
    } else {
        None
    }
}

fn is_valid_host_chars(host: &str) -> bool {
    host.parse::<IpAddr>().is_ok()
        || (!host.is_empty()
            && !host.starts_with('-')
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')))
}

pub fn validate_rdp_file_path(raw: &str) -> Result<(), String> {
    let trimmed = raw.trim();
    if trimmed.chars().any(char::is_control) {
        return Err(".rdp file path cannot contain control characters".into());
    }
    if trimmed.starts_with(['-', '/']) {
        return Err(".rdp file path cannot start with '-' or '/'".into());
    }
    if trimmed.starts_with(r"\\") || trimmed.starts_with("//") {
        return Err("Network/device .rdp paths (UNC) are not allowed".into());
    }
    let is_rdp = Path::new(trimmed)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("rdp"));
    if !is_rdp {
        return Err(".rdp file must have a .rdp extension".into());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Rdp,
    Ssh,
    Web,
}

impl Protocol {
    pub const ALL: [Protocol; 3] = [Protocol::Rdp, Protocol::Ssh, Protocol::Web];

    pub fn label(self) -> &'static str {
        match self {
            Protocol::Rdp => "RDP",
            Protocol::Ssh => "SSH",
            Protocol::Web => "WEB",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Protocol::Rdp => 3389,
            Protocol::Ssh => 22,
            Protocol::Web => 80,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeMode {
    pub fn toggle(self) -> Self {
        match self {
            ThemeMode::System => ThemeMode::Light,
            ThemeMode::Light => ThemeMode::Dark,
            ThemeMode::Dark => ThemeMode::System,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ThemeMode::System => "System",
            ThemeMode::Light => "Light",
            ThemeMode::Dark => "Dark",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    pub host: String,
    pub protocol: Protocol,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rdp_file: Option<String>,
}

impl Asset {
    /// Splits `host`, `host:port`, `[ipv6]`, or `[ipv6]:port` into `(host, Option<port>)`.
    pub fn split_host_port(raw: &str) -> Result<(String, Option<u16>), String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err("Host cannot be empty".into());
        }

        if let Some(after_open) = trimmed.strip_prefix('[') {
            let Some(end) = after_open.find(']') else {
                return Err("Unclosed '[' in IPv6 host".into());
            };
            let host = &after_open[..end];
            if host.parse::<Ipv6Addr>().is_err() {
                return Err("Bracketed host must be a valid IPv6 address".into());
            }
            let rest = &after_open[end + 1..];
            if rest.is_empty() {
                return Ok((host.to_string(), None));
            }
            if let Some(port_str) = rest.strip_prefix(':') {
                let p = port_str
                    .parse::<u16>()
                    .map_err(|_| "Invalid port number".to_string())?;
                if p == 0 {
                    return Err("Port cannot be 0".into());
                }
                return Ok((host.to_string(), Some(p)));
            }
            return Err("Unexpected characters after IPv6 address".into());
        }

        if trimmed.parse::<Ipv6Addr>().is_ok() {
            return Ok((trimmed.to_string(), None));
        }

        if let Some((host, port_str)) = trimmed.rsplit_once(':') {
            if host.contains(':') {
                return Err("Use [ipv6]:port for IPv6 addresses with a port".into());
            }
            if host.is_empty() {
                return Err("Host cannot be empty".into());
            }
            let p = port_str
                .parse::<u16>()
                .map_err(|_| "Invalid port number".to_string())?;
            if p == 0 {
                return Err("Port cannot be 0".into());
            }
            return Ok((host.to_string(), Some(p)));
        }

        Ok((trimmed.to_string(), None))
    }

    /// Formats a host and port, bracketing IPv6 literals when appending a port.
    pub fn format_host_port(host: &str, port: Option<u16>) -> String {
        match port {
            Some(p) if host.contains(':') && !host.starts_with('[') => format!("[{host}]:{p}"),
            Some(p) => format!("{host}:{p}"),
            None => host.to_string(),
        }
    }

    fn parse_web_parts(&self) -> Result<(String, String, u16), String> {
        let raw = self.host.trim();
        if raw.is_empty() {
            return Err("Host/URL cannot be empty".into());
        }
        if raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err("Host/URL cannot contain whitespace or control characters".into());
        }

        if let Some((scheme, rest, is_https)) = strip_http_scheme(raw) {
            let split_idx = rest.find(['/', '?', '#']).unwrap_or(rest.len());
            let (authority, suffix) = rest.split_at(split_idx);
            let host_port = authority.rsplit_once('@').map_or(authority, |(_, hp)| hp);
            let (host, inline_port) = Self::split_host_port(host_port)?;
            let default_port = if is_https { 443 } else { 80 };
            let port = inline_port.or(self.port).unwrap_or(default_port);
            let url = if let Some(p) = self.port
                && inline_port.is_none()
                && p != default_port
            {
                format!("{scheme}{authority}:{p}{suffix}")
            } else {
                raw.to_string()
            };
            return Ok((url, host, port));
        }

        if raw.contains("://") {
            return Err("Only http:// and https:// URLs are supported".into());
        }

        let split_idx = raw.find(['/', '?', '#']).unwrap_or(raw.len());
        let (authority, suffix) = raw.split_at(split_idx);
        let (host, inline_port) = Self::split_host_port(authority)?;
        let effective_port = inline_port.or(self.port);
        let is_https = effective_port == Some(443);
        let scheme = if is_https { "https" } else { "http" };
        let default_port = if is_https { 443 } else { 80 };
        let port = effective_port.unwrap_or(default_port);
        let url = if let Some(p) = self.port
            && inline_port.is_none()
            && p != default_port
        {
            format!("{scheme}://{authority}:{p}{suffix}")
        } else {
            format!("{scheme}://{raw}")
        };
        Ok((url, host, port))
    }

    pub fn probe_target(&self) -> Option<(String, u16)> {
        let raw = self.host.trim();
        if raw.is_empty() {
            return None;
        }

        match self.protocol {
            Protocol::Web => self
                .parse_web_parts()
                .ok()
                .map(|(_, host, port)| (host, port)),
            Protocol::Rdp | Protocol::Ssh => {
                let (host, inline_port) = Self::split_host_port(raw).ok()?;
                let port = self
                    .port
                    .or(inline_port)
                    .unwrap_or_else(|| self.protocol.default_port());
                Some((host, port))
            }
        }
    }

    pub fn web_url(&self) -> Result<String, String> {
        self.parse_web_parts().map(|(url, _, _)| url)
    }

    pub fn display_address(&self) -> String {
        match self.protocol {
            Protocol::Rdp | Protocol::Ssh => {
                let (host, inline_port) = Self::split_host_port(self.host.trim())
                    .unwrap_or_else(|_| (self.host.trim().to_string(), None));
                let port = self
                    .port
                    .or(inline_port)
                    .filter(|&p| p != self.protocol.default_port());
                let addr = Self::format_host_port(&host, port);
                match self.username.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
                    Some(user) => format!("{user}@{addr}"),
                    None => addr,
                }
            }
            Protocol::Web => self.web_url().unwrap_or_else(|_| self.host.trim().to_string()),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Name cannot be empty".into());
        }
        if self.name.chars().any(char::is_control) {
            return Err("Name cannot contain control characters".into());
        }
        let host = self.host.trim();
        if host.is_empty() {
            return Err("Host cannot be empty".into());
        }
        if host.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err("Host cannot contain whitespace or control characters".into());
        }
        if self.port == Some(0) {
            return Err("Port cannot be 0".into());
        }

        if let Some(user) = self.username.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
            if user.chars().any(char::is_control) {
                return Err("Username cannot contain control characters".into());
            }
            if self.protocol == Protocol::Ssh {
                if user.chars().any(char::is_whitespace) {
                    return Err("SSH username cannot contain whitespace".into());
                }
                if user.starts_with('-') {
                    return Err("SSH username cannot start with '-'".into());
                }
                if user.contains(';') {
                    return Err("SSH username cannot contain ';'".into());
                }
            }
        }

        match self.protocol {
            Protocol::Rdp | Protocol::Ssh => {
                let (parsed_host, _) = Self::split_host_port(host)?;
                if !is_valid_host_chars(&parsed_host) {
                    return Err("Host contains invalid characters or starts with '-'".into());
                }
                if self.protocol == Protocol::Rdp
                    && let Some(rdp_path) = self
                        .rdp_file
                        .as_deref()
                        .map(str::trim)
                        .filter(|p| !p.is_empty())
                {
                    validate_rdp_file_path(rdp_path)?;
                }
            }
            Protocol::Web => {
                self.parse_web_parts()?;
            }
        }

        Ok(())
    }
}

fn non_empty_env(name: &str) -> Option<PathBuf> {
    let val = std::env::var(name).ok()?;
    let trimmed = val.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

pub fn default_data_dir() -> PathBuf {
    if let Some(base) = non_empty_env("APPDATA").or_else(|| non_empty_env("LOCALAPPDATA")) {
        return base.join("RDPCenter");
    }
    if let Some(profile) = non_empty_env("USERPROFILE") {
        return profile.join(".rdp-center");
    }
    std::env::temp_dir().join("RDPCenter")
}

pub fn resolve_config_path() -> PathBuf {
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--config"
            && let Some(path) = args.next()
        {
            return PathBuf::from(path);
        }
    }

    if let Some(env_path) = non_empty_env("RDP_CENTER_CONFIG") {
        return env_path;
    }

    default_data_dir().join("assets.json")
}

pub fn sample_assets() -> Vec<Asset> {
    vec![
        Asset {
            name: "DC01 - Windows AD Server".into(),
            host: "192.168.1.10".into(),
            protocol: Protocol::Rdp,
            port: None,
            username: Some("Administrator".into()),
            category: "Server".into(),
            notes: "Active Directory & DNS".into(),
            rdp_file: None,
        },
        Asset {
            name: "APP01 - Linux App Server".into(),
            host: "192.168.1.20".into(),
            protocol: Protocol::Ssh,
            port: None,
            username: Some("devops".into()),
            category: "Server".into(),
            notes: "Docker & API Services".into(),
            rdp_file: None,
        },
        Asset {
            name: "NAS01 - Synology Backup".into(),
            host: "192.168.1.30".into(),
            protocol: Protocol::Web,
            port: Some(5000),
            username: None,
            category: "NAS".into(),
            notes: "File Share & Daily Snapshots".into(),
            rdp_file: None,
        },
        Asset {
            name: "PRN-ACC - HP LaserJet ฝ่ายบัญชี".into(),
            host: "192.168.1.50".into(),
            protocol: Protocol::Web,
            port: Some(80),
            username: None,
            category: "Printer".into(),
            notes: "Web Admin & Toner Status".into(),
            rdp_file: None,
        },
        Asset {
            name: "PVE01 - Proxmox Hypervisor".into(),
            host: "https://192.168.1.5".into(),
            protocol: Protocol::Web,
            port: Some(8006),
            username: None,
            category: "Virtualization".into(),
            notes: "VM Cluster Management".into(),
            rdp_file: None,
        },
    ]
}

#[derive(Debug, Clone)]
pub struct LoadAssetsResult {
    pub assets: Vec<Asset>,
    pub notice: Option<String>,
    pub load_failed: bool,
}

pub fn load_or_seed(path: &Path) -> LoadAssetsResult {
    if path.exists() {
        match fs::read_to_string(path) {
            Ok(content) => match serde_json::from_str::<Vec<Asset>>(&content) {
                Ok(assets) => LoadAssetsResult {
                    assets,
                    notice: None,
                    load_failed: false,
                },
                Err(err) => LoadAssetsResult {
                    assets: Vec::new(),
                    notice: Some(format!(
                        "Failed to parse {} (file preserved): {err}",
                        path.display()
                    )),
                    load_failed: true,
                },
            },
            Err(err) => LoadAssetsResult {
                assets: Vec::new(),
                notice: Some(format!("Failed to read {}: {err}", path.display())),
                load_failed: true,
            },
        }
    } else {
        let samples = sample_assets();
        let notice = match save_assets(path, &samples) {
            Ok(()) => Some(format!(
                "Created starter {} (edit or drag & drop .rdp files)",
                path.display()
            )),
            Err(err) => Some(format!("Loaded defaults (could not save file: {err})")),
        };
        LoadAssetsResult {
            assets: samples,
            notice,
            load_failed: false,
        }
    }
}

fn atomic_write(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create directory {}: {e}", parent.display()))?;
    }

    let base_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("config.json");
    let pid = std::process::id();

    let mut tmp_path = None;
    let mut last_err = None;
    for _ in 0..8 {
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let candidate = path.with_file_name(format!("{base_name}.{pid}.{seq}.tmp"));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                if let Err(err) = file
                    .write_all(content.as_bytes())
                    .and_then(|()| file.sync_all())
                {
                    drop(file);
                    let _ = fs::remove_file(&candidate);
                    return Err(format!("Failed to write {}: {err}", candidate.display()));
                }
                tmp_path = Some(candidate);
                break;
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                last_err = Some(err);
            }
            Err(err) => {
                return Err(format!("Failed to create {}: {err}", candidate.display()));
            }
        }
    }

    let Some(tmp_path) = tmp_path else {
        return Err(format!(
            "Failed to allocate temp file for {}: {}",
            path.display(),
            last_err.map_or_else(|| "AlreadyExists".into(), |e| e.to_string())
        ));
    };

    let mut rename_err = None;
    for attempt in 0..5 {
        match fs::rename(&tmp_path, path) {
            Ok(()) => return Ok(()),
            Err(err) => {
                rename_err = Some(err);
                if attempt < 4 {
                    std::thread::sleep(std::time::Duration::from_millis(15));
                }
            }
        }
    }

    let _ = fs::remove_file(&tmp_path);
    Err(format!(
        "Failed to replace {}: {}",
        path.display(),
        rename_err.map_or_else(|| "unknown error".into(), |e| e.to_string())
    ))
}

pub fn save_assets(path: &Path, assets: &[Asset]) -> Result<(), String> {
    let json = serde_json::to_string_pretty(assets)
        .map_err(|e| format!("Failed to serialize assets: {e}"))?;
    atomic_write(path, &format!("{json}\n"))
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub theme: ThemeMode,
    #[serde(default = "default_true")]
    pub minimize_to_tray: bool,
    #[serde(default = "default_true")]
    pub close_to_tray: bool,
    #[serde(default)]
    pub start_with_windows: bool,
    #[serde(default)]
    pub start_minimized: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: ThemeMode::System,
            minimize_to_tray: true,
            close_to_tray: true,
            start_with_windows: false,
            start_minimized: false,
        }
    }
}

pub fn settings_path_for(config_path: &Path) -> PathBuf {
    config_path.with_file_name("settings.json")
}

pub fn load_settings(config_path: &Path) -> AppSettings {
    let path = settings_path_for(config_path);
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<AppSettings>(&raw).ok())
        .unwrap_or_default()
}

pub fn save_settings(config_path: &Path, settings: &AppSettings) -> Result<(), String> {
    let path = settings_path_for(config_path);
    let json = serde_json::to_string_pretty(settings)
        .map_err(|e| format!("Failed to serialize settings: {e}"))?;
    atomic_write(&path, &format!("{json}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_asset(protocol: Protocol, host: &str, port: Option<u16>) -> Asset {
        Asset {
            name: "Test Node".into(),
            host: host.into(),
            protocol,
            port,
            username: None,
            category: "Server".into(),
            notes: String::new(),
            rdp_file: None,
        }
    }

    #[test]
    fn serde_round_trip() {
        let list = sample_assets();
        let json = serde_json::to_string_pretty(&list).unwrap();
        let decoded: Vec<Asset> = serde_json::from_str(&json).unwrap();
        assert_eq!(list, decoded);
    }

    #[test]
    fn probe_target_parsing() {
        assert_eq!(
            mk_asset(Protocol::Rdp, "192.168.1.10", None).probe_target(),
            Some(("192.168.1.10".into(), 3389))
        );
        assert_eq!(
            mk_asset(Protocol::Rdp, "192.168.1.10:3390", None).probe_target(),
            Some(("192.168.1.10".into(), 3390))
        );
        assert_eq!(
            mk_asset(Protocol::Ssh, "[::1]:2222", None).probe_target(),
            Some(("::1".into(), 2222))
        );
        assert_eq!(
            mk_asset(Protocol::Web, "https://pve.local/ui", Some(8006)).probe_target(),
            Some(("pve.local".into(), 8006))
        );
        assert_eq!(
            mk_asset(Protocol::Web, "https://router.local", None).probe_target(),
            Some(("router.local".into(), 443))
        );
        assert_eq!(
            mk_asset(Protocol::Web, "192.168.1.50", None).probe_target(),
            Some(("192.168.1.50".into(), 80))
        );
    }

    #[test]
    fn web_url_allows_http_https_and_rejects_dangerous_schemes() {
        let nas = mk_asset(Protocol::Web, "192.168.1.30", Some(5000));
        assert_eq!(nas.web_url().unwrap(), "http://192.168.1.30:5000");

        let pve = mk_asset(Protocol::Web, "https://192.168.1.5", Some(8006));
        assert_eq!(pve.web_url().unwrap(), "https://192.168.1.5:8006");

        let https_443 = mk_asset(Protocol::Web, "192.168.1.30:443", None);
        assert_eq!(https_443.web_url().unwrap(), "https://192.168.1.30:443");
        assert_eq!(
            https_443.probe_target(),
            Some(("192.168.1.30".into(), 443))
        );

        let override_port = mk_asset(Protocol::Web, "https://192.168.1.5:8443/ui", Some(8006));
        assert_eq!(
            override_port.web_url().unwrap(),
            "https://192.168.1.5:8443/ui"
        );
        assert_eq!(
            override_port.probe_target(),
            Some(("192.168.1.5".into(), 8443))
        );

        let thai = mk_asset(Protocol::Web, "ไทย", None);
        assert_eq!(thai.web_url().unwrap(), "http://ไทย");
        assert_eq!(thai.probe_target(), Some(("ไทย".into(), 80)));

        let aa_thai = mk_asset(Protocol::Web, "aaไทย", None);
        assert_eq!(aa_thai.web_url().unwrap(), "http://aaไทย");

        let file_url = mk_asset(Protocol::Web, "file:///etc/passwd", None);
        assert!(file_url.web_url().is_err());

        let js_url = mk_asset(Protocol::Web, "javascript:alert(1)", None);
        assert!(js_url.web_url().is_err());
    }

    #[test]
    fn validate_rejects_flag_injection_and_zero_port() {
        let bad_host = mk_asset(Protocol::Ssh, "-oProxyCommand=calc", None);
        assert!(bad_host.validate().is_err());

        let bracketed_flag = mk_asset(Protocol::Ssh, "[-oProxyCommand=calc]", None);
        assert!(bracketed_flag.validate().is_err());

        let semi_host = mk_asset(Protocol::Ssh, "192.168.1.20;calc.exe", None);
        assert!(semi_host.validate().is_err());

        let mut bad_user = mk_asset(Protocol::Ssh, "192.168.1.20", None);
        bad_user.username = Some("-evil".into());
        assert!(bad_user.validate().is_err());

        bad_user.username = Some("dev;calc".into());
        assert!(bad_user.validate().is_err());

        let zero_port = mk_asset(Protocol::Rdp, "192.168.1.10", Some(0));
        assert!(zero_port.validate().is_err());

        let inline_zero = mk_asset(Protocol::Rdp, "192.168.1.10:0", None);
        assert!(inline_zero.validate().is_err());

        let inline_overflow = mk_asset(Protocol::Rdp, "192.168.1.10:99999", None);
        assert!(inline_overflow.validate().is_err());

        let mut unc_rdp = mk_asset(Protocol::Rdp, "192.168.1.10", None);
        unc_rdp.rdp_file = Some(r"\\attacker\share\evil.rdp".into());
        assert!(unc_rdp.validate().is_err());

        let mut exe_rdp = mk_asset(Protocol::Rdp, "192.168.1.10", None);
        exe_rdp.rdp_file = Some(r"C:\tools\evil.exe".into());
        assert!(exe_rdp.validate().is_err());
    }

    #[test]
    fn load_or_seed_preserves_corrupt_file_and_saves_cleanly() {
        let dir =
            std::env::temp_dir().join(format!("it-launcher-load-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("assets.json");

        let seeded = load_or_seed(&config_path);
        assert!(!seeded.load_failed);
        assert!(!seeded.assets.is_empty());

        let subset = vec![mk_asset(Protocol::Rdp, "10.20.30.40", Some(3390))];
        save_assets(&config_path, &subset).unwrap();
        let reloaded = load_or_seed(&config_path);
        assert!(!reloaded.load_failed);
        assert_eq!(reloaded.assets, subset);

        fs::write(&config_path, "{ not valid json").unwrap();
        let failed = load_or_seed(&config_path);
        assert!(failed.load_failed);
        assert!(failed.assets.is_empty());
        assert_eq!(fs::read_to_string(&config_path).unwrap(), "{ not valid json");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_defaults_and_legacy_json_upgrade() {
        assert_eq!(ThemeMode::System.toggle(), ThemeMode::Light);
        assert_eq!(ThemeMode::Light.toggle(), ThemeMode::Dark);
        assert_eq!(ThemeMode::Dark.toggle(), ThemeMode::System);

        let dir =
            std::env::temp_dir().join(format!("it-launcher-settings-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("assets.json");

        assert_eq!(load_settings(&config_path), AppSettings::default());

        fs::write(settings_path_for(&config_path), r#"{"theme": "light"}"#).unwrap();
        let upgraded = load_settings(&config_path);
        assert_eq!(upgraded.theme, ThemeMode::Light);
        assert!(upgraded.minimize_to_tray);
        assert!(upgraded.close_to_tray);
        assert!(!upgraded.start_with_windows);
        assert!(!upgraded.start_minimized);

        let custom = AppSettings {
            theme: ThemeMode::Dark,
            minimize_to_tray: false,
            close_to_tray: true,
            start_with_windows: true,
            start_minimized: true,
        };
        save_settings(&config_path, &custom).unwrap();
        assert_eq!(load_settings(&config_path), custom);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_data_dir_points_to_app_data_not_exe_dir() {
        let dir = default_data_dir();
        let leaf = dir.file_name().and_then(|n| n.to_str());
        if cfg!(target_os = "windows") {
            assert!(matches!(leaf, Some("RDPCenter" | ".rdp-center")));
        } else {
            assert_eq!(leaf, Some("rdp-center"));
        }

        if let Ok(exe) = std::env::current_exe()
            && let Some(exe_dir) = exe.parent()
        {
            assert_ne!(dir, exe_dir);
        }
    }
}
