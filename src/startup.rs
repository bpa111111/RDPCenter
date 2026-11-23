use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{null, null_mut};

const HKEY_CURRENT_USER: isize = -2147483647;
const KEY_QUERY_VALUE: u32 = 0x0001;
const KEY_SET_VALUE: u32 = 0x0002;
const REG_SZ: u32 = 1;
const REG_DWORD: u32 = 4;
const ERROR_SUCCESS: i32 = 0;
const ERROR_FILE_NOT_FOUND: i32 = 2;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE_NAME: &str = "RDPCenter";
const LEGACY_RUN_VALUE_NAME: &str = "ITRemoteLauncher";

#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegOpenKeyExW(
        hkey: isize,
        sub_key: *const u16,
        options: u32,
        sam_desired: u32,
        phk_result: *mut isize,
    ) -> i32;
    fn RegCreateKeyExW(
        hkey: isize,
        sub_key: *const u16,
        reserved: u32,
        class: *mut u16,
        options: u32,
        sam_desired: u32,
        security_attributes: *const core::ffi::c_void,
        phk_result: *mut isize,
        disposition: *mut u32,
    ) -> i32;
    fn RegQueryValueExW(
        hkey: isize,
        value_name: *const u16,
        reserved: *mut u32,
        reg_type: *mut u32,
        data: *mut u8,
        cb_data: *mut u32,
    ) -> i32;
    fn RegSetValueExW(
        hkey: isize,
        value_name: *const u16,
        reserved: u32,
        reg_type: u32,
        data: *const u8,
        cb_data: u32,
    ) -> i32;
    fn RegDeleteValueW(hkey: isize, value_name: *const u16) -> i32;
    fn RegCloseKey(hkey: isize) -> i32;
}

pub(crate) fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

pub(crate) fn reg_query_dword(sub_key: &str, value_name: &str) -> Option<u32> {
    let key_w = to_wide(sub_key);
    let val_w = to_wide(value_name);
    let mut hkey: isize = 0;
    let open_res =
        unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, key_w.as_ptr(), 0, KEY_QUERY_VALUE, &mut hkey) };
    if open_res != ERROR_SUCCESS {
        return None;
    }

    let mut reg_type: u32 = 0;
    let mut value: u32 = 0;
    let mut cb_data: u32 = size_of::<u32>() as u32;
    let query_res = unsafe {
        RegQueryValueExW(
            hkey,
            val_w.as_ptr(),
            null_mut(),
            &mut reg_type,
            (&raw mut value).cast::<u8>(),
            &mut cb_data,
        )
    };
    unsafe {
        RegCloseKey(hkey);
    }
    (query_res == ERROR_SUCCESS && reg_type == REG_DWORD).then_some(value)
}

pub fn has_minimized_flag() -> bool {
    std::env::args().any(|arg| arg == "--minimized" || arg == "-m")
}

pub fn format_startup_command(exe_path: &Path, start_minimized: bool) -> String {
    let raw = exe_path.to_string_lossy();
    let unc_owned;
    let clean = if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        unc_owned = format!(r"\\{rest}");
        &unc_owned
    } else {
        raw.strip_prefix(r"\\?\").unwrap_or(&raw)
    };
    if start_minimized {
        format!("\"{clean}\" --minimized")
    } else {
        format!("\"{clean}\"")
    }
}

pub fn is_enabled() -> bool {
    let key_w = to_wide(RUN_KEY);
    let val_w = to_wide(RUN_VALUE_NAME);
    let mut hkey: isize = 0;
    let open_res =
        unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, key_w.as_ptr(), 0, KEY_QUERY_VALUE, &mut hkey) };
    if open_res != ERROR_SUCCESS {
        return false;
    }
    let query_res = unsafe {
        RegQueryValueExW(
            hkey,
            val_w.as_ptr(),
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
        )
    };
    unsafe {
        RegCloseKey(hkey);
    }
    query_res == ERROR_SUCCESS
}

pub fn set_enabled(enabled: bool, start_minimized: bool) -> Result<(), String> {
    let key_w = to_wide(RUN_KEY);
    let val_w = to_wide(RUN_VALUE_NAME);
    let legacy_w = to_wide(LEGACY_RUN_VALUE_NAME);
    let mut hkey: isize = 0;
    let create_res = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key_w.as_ptr(),
            0,
            null_mut(),
            0,
            KEY_SET_VALUE,
            null(),
            &mut hkey,
            null_mut(),
        )
    };
    if create_res != ERROR_SUCCESS {
        return Err(format!("Failed to open Windows Run key (code {create_res})"));
    }

    unsafe {
        let _ = RegDeleteValueW(hkey, legacy_w.as_ptr());
    }

    let op_res = if enabled {
        match std::env::current_exe() {
            Ok(exe) => {
                let cmd = format_startup_command(&exe, start_minimized);
                let cmd_w = to_wide(&cmd);
                let byte_len = u32::try_from(cmd_w.len() * 2).unwrap_or(u32::MAX);
                unsafe {
                    RegSetValueExW(
                        hkey,
                        val_w.as_ptr(),
                        0,
                        REG_SZ,
                        cmd_w.as_ptr().cast::<u8>(),
                        byte_len,
                    )
                }
            }
            Err(err) => {
                unsafe {
                    RegCloseKey(hkey);
                }
                return Err(format!("Failed to resolve executable path: {err}"));
            }
        }
    } else {
        let del = unsafe { RegDeleteValueW(hkey, val_w.as_ptr()) };
        if del == ERROR_FILE_NOT_FOUND {
            ERROR_SUCCESS
        } else {
            del
        }
    };

    unsafe {
        RegCloseKey(hkey);
    }

    if op_res == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!("Failed to update Windows startup entry (code {op_res})"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn format_startup_command_quotes_spaces_and_strips_verbatim_prefix() {
        let p = PathBuf::from(r"\\?\C:\Program Files\IT Tools\RDPCenter.exe");
        assert_eq!(
            format_startup_command(&p, false),
            r#""C:\Program Files\IT Tools\RDPCenter.exe""#
        );
        assert_eq!(
            format_startup_command(&p, true),
            r#""C:\Program Files\IT Tools\RDPCenter.exe" --minimized"#
        );

        let unc = PathBuf::from(r"\\?\UNC\fileserver\tools\RDPCenter.exe");
        assert_eq!(
            format_startup_command(&unc, false),
            r#""\\fileserver\tools\RDPCenter.exe""#
        );
    }
}
