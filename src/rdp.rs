use crate::model::{Asset, Protocol};
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};

const MAX_RDP_FILE_BYTES: u64 = 64 * 1024;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;

fn is_reparse_or_symlink(meta: &fs::Metadata) -> bool {
    meta.file_type().is_symlink() || (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ImportSummary {
    pub imported: usize,
    pub skipped_duplicates: usize,
    pub failed: usize,
}

pub fn decode_rdp_bytes(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .copied()
            .map(u16::from_le_bytes)
            .collect();
        String::from_utf16_lossy(&units)
    } else if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .copied()
            .map(u16::from_be_bytes)
            .collect();
        String::from_utf16_lossy(&units)
    } else if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(rest).into_owned()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

fn parse_rdp_u16(raw: &str) -> Option<u16> {
    let trimmed = raw.trim();
    let val = if let Some(hex) = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")) {
        u16::from_str_radix(hex, 16).ok()?
    } else {
        trimmed.parse::<u16>().ok()?
    };
    (val > 0).then_some(val)
}

pub fn parse_rdp_bytes(bytes: &[u8], fallback_name: &str) -> Option<Asset> {
    let text = decode_rdp_bytes(bytes);
    let mut full_address: Option<String> = None;
    let mut server_port: Option<u16> = None;
    let mut username: Option<String> = None;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower
            .strip_prefix("full address:s:")
            .map(|_| &line["full address:s:".len()..])
        {
            let val = rest.trim();
            if !val.is_empty() {
                full_address = Some(val.to_string());
            }
        } else if let Some(rest) = lower
            .strip_prefix("server port:i:")
            .map(|_| &line["server port:i:".len()..])
        {
            if let Some(p) = parse_rdp_u16(rest) {
                server_port = Some(p);
            }
        } else if let Some(rest) = lower
            .strip_prefix("username:s:")
            .map(|_| &line["username:s:".len()..])
        {
            let val = rest.trim();
            if !val.is_empty() {
                username = Some(val.to_string());
            }
        }
    }

    let addr = full_address?;
    let (host, parsed_port) = Asset::split_host_port(&addr).ok()?;

    let name = if fallback_name.trim().is_empty() {
        host.clone()
    } else {
        fallback_name.trim().to_string()
    };

    let asset = Asset {
        name,
        host,
        protocol: Protocol::Rdp,
        port: parsed_port.or(server_port).filter(|&p| p != 3389),
        username,
        category: "Server".into(),
        notes: "Imported from .rdp".into(),
        rdp_file: None,
    };

    asset.validate().ok()?;
    Some(asset)
}

pub fn parse_rdp_file(path: &Path) -> Result<Asset, String> {
    let meta = fs::symlink_metadata(path)
        .map_err(|e| format!("Failed to inspect {}: {e}", path.display()))?;
    if is_reparse_or_symlink(&meta) || !meta.file_type().is_file() {
        return Err(format!("Refusing non-regular or linked file: {}", path.display()));
    }

    let file =
        fs::File::open(path).map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_RDP_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    if bytes.len() as u64 > MAX_RDP_FILE_BYTES {
        return Err(format!(".rdp file too large (> 64 KiB): {}", path.display()));
    }

    let fallback_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("RDP Host");

    parse_rdp_bytes(&bytes, fallback_name)
        .ok_or_else(|| format!("Invalid or incomplete .rdp file: {}", path.display()))
}

fn is_rdp_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("rdp"))
}

fn collect_rdp_files(path: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return;
    };
    if is_reparse_or_symlink(&meta) {
        return;
    }

    if meta.file_type().is_file() {
        if is_rdp_file(path) {
            out.push(path.to_path_buf());
        }
        return;
    }

    if meta.file_type().is_dir() && depth < 3
        && let Ok(entries) = fs::read_dir(path)
    {
        for entry in entries.flatten() {
            collect_rdp_files(&entry.path(), depth + 1, out);
        }
    }
}

fn dedup_key(asset: &Asset) -> Option<(Protocol, String, u16)> {
    asset
        .probe_target()
        .map(|(h, p)| (asset.protocol, h.to_ascii_lowercase(), p))
}

pub fn import_rdp_paths(paths: &[PathBuf], existing: &mut Vec<Asset>) -> ImportSummary {
    let mut files = Vec::new();
    for path in paths {
        collect_rdp_files(path, 0, &mut files);
    }
    files.sort();
    files.dedup();

    let mut seen: HashSet<(Protocol, String, u16)> =
        existing.iter().filter_map(dedup_key).collect();

    let mut summary = ImportSummary::default();
    for file in files {
        match parse_rdp_file(&file) {
            Ok(asset) => {
                if let Some(key) = dedup_key(&asset)
                    && !seen.insert(key)
                {
                    summary.skipped_duplicates += 1;
                    continue;
                }
                existing.push(asset);
                summary.imported += 1;
            }
            Err(_) => {
                summary.failed += 1;
            }
        }
    }

    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_utf16_le_bom(s: &str) -> Vec<u8> {
        let mut out = vec![0xFF, 0xFE];
        for unit in s.encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out
    }

    #[test]
    fn parses_utf16_le_and_utf8_rdp() {
        let content = "screen mode id:i:2\r\nfull address:s:10.0.0.55:3390\r\nusername:s:CORP\\admin\r\n";
        let utf16 = encode_utf16_le_bom(content);

        let asset = parse_rdp_bytes(&utf16, "ERP-Server").unwrap();
        assert_eq!(asset.name, "ERP-Server");
        assert_eq!(asset.host, "10.0.0.55");
        assert_eq!(asset.port, Some(3390));
        assert_eq!(asset.username.as_deref(), Some("CORP\\admin"));
        assert_eq!(asset.protocol, Protocol::Rdp);

        let utf8_asset = parse_rdp_bytes(content.as_bytes(), "ERP-UTF8").unwrap();
        assert_eq!(utf8_asset.host, "10.0.0.55");
        assert_eq!(utf8_asset.port, Some(3390));
    }

    #[test]
    fn rejects_rdp_without_full_address() {
        let bad = "username:s:admin\r\n";
        assert!(parse_rdp_bytes(bad.as_bytes(), "Missing").is_none());
    }

    #[test]
    fn import_skips_duplicates() {
        let dir = std::env::temp_dir().join(format!("it-launcher-rdp-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let f1 = dir.join("node1.rdp");
        let f2 = dir.join("node1_copy.rdp");
        let f3 = dir.join("broken.rdp");
        fs::write(&f1, "full address:s:10.1.1.10\r\n").unwrap();
        fs::write(&f2, encode_utf16_le_bom("full address:s:10.1.1.10:3389\r\n")).unwrap();
        fs::write(&f3, "username:s:nobody\r\n").unwrap();

        let mut assets = Vec::new();
        let summary = import_rdp_paths(std::slice::from_ref(&dir), &mut assets);
        assert_eq!(summary.imported, 1);
        assert_eq!(summary.skipped_duplicates, 1);
        assert_eq!(summary.failed, 1);
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].rdp_file, None);

        let f4 = dir.join("nat_hex.rdp");
        let f5 = dir.join("oversized.rdp");
        fs::write(&f4, "full address:s:10.1.1.10\r\nserver port:i:0xd3e\r\n").unwrap();
        fs::write(&f5, vec![b'a'; (64 * 1024) + 16]).unwrap();

        let nat_summary = import_rdp_paths(std::slice::from_ref(&dir), &mut assets);
        assert_eq!(nat_summary.imported, 1);
        assert_eq!(assets.len(), 2);
        assert_eq!(assets[1].port, Some(3390));

        let dec = parse_rdp_bytes(
            b"full address:s:gw.corp.local\r\nserver port:i:3391\r\n",
            "GW-NAT",
        )
        .unwrap();
        assert_eq!(dec.port, Some(3391));

        let _ = fs::remove_dir_all(&dir);
    }
}
