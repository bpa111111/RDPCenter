use crate::model::{Asset, Protocol, default_data_dir};
use crate::startup::to_wide;
use std::fs;
use std::io::{ErrorKind, Write};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
const FILE_SHARE_READ: u32 = 1;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
const SW_SHOWNORMAL: i32 = 1;

static RDP_SESSION_SEQ: AtomicU64 = AtomicU64::new(0);

#[link(name = "shell32")]
unsafe extern "system" {
    fn ShellExecuteW(
        hwnd: *mut core::ffi::c_void,
        lp_operation: *const u16,
        lp_file: *const u16,
        lp_parameters: *const u16,
        lp_directory: *const u16,
        n_show_cmd: i32,
    ) -> isize;
}

pub fn ssh_args(asset: &Asset) -> Result<Vec<String>, String> {
    asset.validate()?;
    if asset.protocol != Protocol::Ssh {
        return Err("Asset is not configured for SSH".into());
    }

    let (host, port) = asset
        .probe_target()
        .ok_or_else(|| "Invalid SSH host".to_string())?;

    let mut args = Vec::new();
    if port != 22 {
        args.push("-p".into());
        args.push(port.to_string());
    }

    let dest = match asset
        .username
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
    {
        Some(user) => format!("{user}@{host}"),
        None => host,
    };
    args.push("--".into());
    args.push(dest);

    Ok(args)
}

pub fn sanitize_wt_title(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| if c == ';' { '-' } else { c })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "SSH".into()
    } else {
        trimmed.to_string()
    }
}

pub fn launch_asset(asset: &Asset) -> Result<String, String> {
    asset.validate()?;
    match asset.protocol {
        Protocol::Rdp => launch_rdp(asset),
        Protocol::Ssh => launch_ssh(asset),
        Protocol::Web => launch_web(asset),
    }
}

fn rdp_target(asset: &Asset) -> Result<String, String> {
    let (host, port) = asset
        .probe_target()
        .ok_or_else(|| "Invalid RDP target".to_string())?;
    let port_opt = (port != 3389).then_some(port);
    Ok(Asset::format_host_port(&host, port_opt))
}

fn session_dir() -> PathBuf {
    if let Some(local) = std::env::var_os("LOCALAPPDATA")
        && !local.to_string_lossy().trim().is_empty()
    {
        return PathBuf::from(local).join("RDPCenter").join("sessions");
    }
    default_data_dir().join("sessions")
}

pub fn cleanup_stale_sessions() {
    let dir = session_dir();
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("rdp-") || !name.ends_with(".rdp") {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink()
            || (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0
            || !meta.file_type().is_file()
        {
            continue;
        }
        let expired = meta
            .modified()
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > Duration::from_secs(120));
        if expired {
            let _ = fs::remove_file(&path);
        }
    }
}

fn write_temp_rdp(target: &str, username: &str) -> Result<(PathBuf, fs::File), String> {
    let dir = session_dir();
    fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create RDP session directory {}: {e}", dir.display()))?;

    let content = format!("full address:s:{target}\r\nusername:s:{username}\r\n");
    let mut bytes = vec![0xFF, 0xFE];
    for unit in content.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }

    let pid = std::process::id();
    for _ in 0..8 {
        let seq = RDP_SESSION_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("rdp-{pid}-{seq}.rdp"));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut writer) => {
                if let Err(err) = writer.write_all(&bytes).and_then(|()| writer.flush()) {
                    drop(writer);
                    let _ = fs::remove_file(&path);
                    return Err(format!("Failed to write temporary .rdp file: {err}"));
                }
                drop(writer);

                let read_lock = fs::OpenOptions::new()
                    .read(true)
                    .share_mode(FILE_SHARE_READ)
                    .open(&path)
                    .map_err(|e| {
                        let _ = fs::remove_file(&path);
                        format!("Failed to lock temporary .rdp file: {e}")
                    })?;
                return Ok((path, read_lock));
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("Failed to create temporary .rdp file: {e}")),
        }
    }

    Err("Failed to allocate unique temporary .rdp file".into())
}

fn launch_rdp(asset: &Asset) -> Result<String, String> {
    let target = rdp_target(asset)?;

    if let Some(rdp_path) = asset
        .rdp_file
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        let p = Path::new(rdp_path);
        let meta = fs::symlink_metadata(p)
            .map_err(|e| format!("Cannot access .rdp file {}: {e}", p.display()))?;
        if meta.file_type().is_symlink()
            || (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0
            || !meta.file_type().is_file()
        {
            return Err("Refusing symlink or invalid .rdp profile path".into());
        }
        Command::new("mstsc.exe")
            .arg(rdp_path)
            .spawn()
            .map_err(|e| format!("Failed to launch mstsc.exe: {e}"))?;
        let file_label = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("profile.rdp");
        return Ok(format!("Launched RDP ({file_label}) → {target}"));
    }

    if let Some(user) = asset
        .username
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
    {
        let (tmp_rdp, lock_file) = write_temp_rdp(&target, user)?;
        if let Err(err) = Command::new("mstsc.exe").arg(&tmp_rdp).spawn() {
            drop(lock_file);
            let _ = fs::remove_file(&tmp_rdp);
            return Err(format!("Failed to launch mstsc.exe: {err}"));
        }
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(5));
            drop(lock_file);
            let _ = fs::remove_file(&tmp_rdp);
        });
        return Ok(format!("Launched RDP → {user}@{target}"));
    }

    Command::new("mstsc.exe")
        .arg(format!("/v:{target}"))
        .spawn()
        .map_err(|e| format!("Failed to launch mstsc.exe: {e}"))?;
    Ok(format!("Launched RDP → {target}"))
}

fn launch_ssh(asset: &Asset) -> Result<String, String> {
    let args = ssh_args(asset)?;
    if args.iter().any(|a| a.contains(';')) {
        return Err("SSH arguments cannot contain ';'".into());
    }
    let title = sanitize_wt_title(&asset.name);
    let dest_label = args.last().cloned().unwrap_or_else(|| asset.host.clone());

    match Command::new("wt.exe")
        .args(["new-tab", "--title", &title, "--", "ssh.exe"])
        .args(&args)
        .spawn()
    {
        Ok(_) => return Ok(format!("Launched SSH (Windows Terminal) → {dest_label}")),
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(format!("Failed to launch wt.exe: {e}")),
    }

    Command::new("ssh.exe")
        .args(&args)
        .creation_flags(CREATE_NEW_CONSOLE)
        .spawn()
        .map_err(|e| format!("Failed to launch ssh.exe: {e}"))?;

    Ok(format!("Launched SSH → {dest_label}"))
}

fn launch_web(asset: &Asset) -> Result<String, String> {
    let url = asset.web_url()?;
    let op_w = to_wide("open");
    let url_w = to_wide(&url);
    let rc = unsafe {
        ShellExecuteW(
            null_mut(),
            op_w.as_ptr(),
            url_w.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        )
    };
    if rc <= 32 {
        return Err(format!("Failed to open browser (ShellExecuteW code {rc})"));
    }
    Ok(format!("Opened Web → {url}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_args_formats_user_and_port() {
        let mut asset = Asset {
            name: "App".into(),
            host: "192.168.1.20".into(),
            protocol: Protocol::Ssh,
            port: None,
            username: Some("devops".into()),
            category: "Server".into(),
            notes: String::new(),
            rdp_file: None,
        };
        assert_eq!(ssh_args(&asset).unwrap(), vec!["--", "devops@192.168.1.20"]);

        asset.port = Some(2222);
        assert_eq!(
            ssh_args(&asset).unwrap(),
            vec!["-p", "2222", "--", "devops@192.168.1.20"]
        );

        asset.username = None;
        assert_eq!(
            ssh_args(&asset).unwrap(),
            vec!["-p", "2222", "--", "192.168.1.20"]
        );
    }

    #[test]
    fn ssh_args_rejects_flag_and_wt_injection() {
        let mk_ssh = |host: &str| Asset {
            name: "Evil".into(),
            host: host.into(),
            protocol: Protocol::Ssh,
            port: None,
            username: None,
            category: String::new(),
            notes: String::new(),
            rdp_file: None,
        };
        assert!(ssh_args(&mk_ssh("-oProxyCommand=calc")).is_err());
        assert!(ssh_args(&mk_ssh("[-oProxyCommand=calc]")).is_err());
        assert!(ssh_args(&mk_ssh("192.168.1.20;calc.exe")).is_err());
        assert_eq!(sanitize_wt_title("DB01; calc.exe"), "DB01- calc.exe");
    }
}
