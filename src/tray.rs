use crate::launcher;
use crate::model::{Asset, Protocol};
use crate::startup::{reg_query_dword, to_wide};
use std::ptr::{null, null_mut};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, AtomicU32, Ordering};

fn quick_remote_order(assets: &[Asset]) -> Vec<Asset> {
    let mut ordered = Vec::with_capacity(assets.len());
    for proto in Protocol::ALL {
        ordered.extend(assets.iter().filter(|a| a.protocol == proto).cloned());
    }
    ordered
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    let clean = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if clean.chars().count() > max_chars {
        let prefix: String = clean.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{prefix}…")
    } else {
        clean
    }
}

fn escape_win32_menu(s: &str) -> String {
    s.replace('&', "&&")
}

fn format_tray_item_label(asset: &Asset) -> String {
    let name = escape_win32_menu(&truncate_chars(&asset.name, 36));
    let addr = escape_win32_menu(&truncate_chars(&asset.display_address(), 32));
    format!("[{}] {}\t{}", asset.protocol.label(), name, addr)
}

type Hwnd = *mut core::ffi::c_void;
type Hicon = *mut core::ffi::c_void;
type Hmenu = *mut core::ffi::c_void;
type Hinstance = *mut core::ffi::c_void;

const WM_DESTROY: u32 = 0x0002;
const WM_SIZE: u32 = 0x0005;
const WM_CLOSE: u32 = 0x0010;
const WM_SETTINGCHANGE: u32 = 0x001A;
const WM_SETICON: u32 = 0x0080;
const WM_NULL: u32 = 0x0000;
const WM_CONTEXTMENU: u32 = 0x007B;
const WM_SYSCOMMAND: u32 = 0x0112;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_LBUTTONDBLCLK: u32 = 0x0203;
const WM_RBUTTONUP: u32 = 0x0205;
const WM_APP_TRAY: u32 = 0x8001;

const ICON_SMALL: usize = 0;
const ICON_BIG: usize = 1;

const SC_MINIMIZE: usize = 0xF020;
const SIZE_MINIMIZED: usize = 1;

const SW_HIDE: i32 = 0;
const SW_SHOW: i32 = 5;
const SW_RESTORE: i32 = 9;

const GW_OWNER: u32 = 4;
const IMAGE_ICON: u32 = 1;
const LR_DEFAULTCOLOR: u32 = 0x0000_0000;
const SM_CXICON: i32 = 11;
const SM_CYICON: i32 = 12;
const SM_CXSMICON: i32 = 49;
const SM_CYSMICON: i32 = 50;
const IDI_APPLICATION: *const u16 = core::ptr::without_provenance(32512);

const NIM_ADD: u32 = 0x0000_0000;
const NIM_MODIFY: u32 = 0x0000_0001;
const NIM_DELETE: u32 = 0x0000_0002;
const NIF_MESSAGE: u32 = 0x0000_0001;
const NIF_ICON: u32 = 0x0000_0002;
const NIF_TIP: u32 = 0x0000_0004;
const NIF_INFO: u32 = 0x0000_0010;
const NIIF_ERROR: u32 = 0x0000_0003;

const ICON_RES_APP: usize = 1;
const ICON_RES_TRAY_DARK: usize = 2;
const ICON_RES_TRAY_LIGHT: usize = 3;

const MF_STRING: u32 = 0x0000_0000;
const MF_GRAYED: u32 = 0x0000_0001;
const MF_SEPARATOR: u32 = 0x0000_0800;
const TPM_RIGHTBUTTON: u32 = 0x0002;
const TPM_NONOTIFY: u32 = 0x0080;
const TPM_RETURNCMD: u32 = 0x0100;

const CMD_OPEN: u32 = 1001;
const CMD_HIDE: u32 = 1002;
const CMD_EXIT: u32 = 1003;
const CMD_ASSET_BASE: u32 = 2000;
const MAX_QUICK_ITEMS: usize = 20;

const SUBCLASS_ID: usize = 0x4954_524C;
const TRAY_ICON_ID: u32 = 1;

static CLOSE_TO_TRAY: AtomicBool = AtomicBool::new(true);
static MINIMIZE_TO_TRAY: AtomicBool = AtomicBool::new(true);
static ALLOW_EXIT: AtomicBool = AtomicBool::new(false);
static INSTALLED: AtomicBool = AtomicBool::new(false);
static TRAY_ADDED: AtomicBool = AtomicBool::new(false);
static MENU_OPEN: AtomicBool = AtomicBool::new(false);
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
static TRAY_ICON: AtomicIsize = AtomicIsize::new(0);
static WIN_ICON_SMALL: AtomicIsize = AtomicIsize::new(0);
static WIN_ICON_BIG: AtomicIsize = AtomicIsize::new(0);
static TRAY_THEME_STATE: AtomicU8 = AtomicU8::new(0);
static WM_TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
static QUICK_ASSETS: Mutex<Vec<Asset>> = Mutex::new(Vec::new());

    #[repr(C)]
struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[repr(C)]
struct NotifyIconDataW {
    cb_size: u32,
    h_wnd: Hwnd,
    u_id: u32,
    u_flags: u32,
    u_callback_message: u32,
    h_icon: Hicon,
    sz_tip: [u16; 128],
    dw_state: u32,
    dw_state_mask: u32,
    sz_info: [u16; 256],
    u_timeout_or_version: u32,
    sz_info_title: [u16; 64],
    dw_info_flags: u32,
    guid_item: Guid,
    h_balloon_icon: Hicon,
}

type SubclassProc = unsafe extern "system" fn(
    hwnd: Hwnd,
    u_msg: u32,
    w_param: usize,
    l_param: isize,
    u_id_subclass: usize,
    dw_ref_data: usize,
) -> isize;

type EnumWindowsProc = unsafe extern "system" fn(hwnd: Hwnd, l_param: isize) -> i32;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentThreadId() -> u32;
    fn GetModuleHandleW(lp_module_name: *const u16) -> Hinstance;
}

#[link(name = "user32")]
unsafe extern "system" {
    fn EnumThreadWindows(dw_thread_id: u32, lpfn: EnumWindowsProc, l_param: isize) -> i32;
    fn GetWindow(hwnd: Hwnd, u_cmd: u32) -> Hwnd;
    fn GetWindowTextW(hwnd: Hwnd, lp_string: *mut u16, n_max_count: i32) -> i32;
    fn GetClassNameW(hwnd: Hwnd, lp_class_name: *mut u16, n_max_count: i32) -> i32;
    fn RegisterWindowMessageW(lp_string: *const u16) -> u32;
    fn GetSystemMetrics(n_index: i32) -> i32;
    fn LoadImageW(
        h_inst: Hinstance,
        name: *const u16,
        img_type: u32,
        cx: i32,
        cy: i32,
        fu_load: u32,
    ) -> Hicon;
    fn LoadIconW(h_instance: Hinstance, lp_icon_name: *const u16) -> Hicon;
    fn DestroyIcon(h_icon: Hicon) -> i32;
    fn ShowWindow(hwnd: Hwnd, n_cmd_show: i32) -> i32;
    fn IsWindowVisible(hwnd: Hwnd) -> i32;
    fn IsIconic(hwnd: Hwnd) -> i32;
    fn SetForegroundWindow(hwnd: Hwnd) -> i32;
    fn PostMessageW(hwnd: Hwnd, msg: u32, w_param: usize, l_param: isize) -> i32;
    fn SendMessageW(hwnd: Hwnd, msg: u32, w_param: usize, l_param: isize) -> isize;
    fn CreatePopupMenu() -> Hmenu;
    fn AppendMenuW(
        h_menu: Hmenu,
        u_flags: u32,
        u_id_new_item: usize,
        lp_new_item: *const u16,
    ) -> i32;
    fn SetMenuDefaultItem(h_menu: Hmenu, u_item: u32, f_by_pos: u32) -> i32;
    fn TrackPopupMenuEx(
        h_menu: Hmenu,
        u_flags: u32,
        x: i32,
        y: i32,
        hwnd: Hwnd,
        lptpm: *const core::ffi::c_void,
    ) -> i32;
    fn DestroyMenu(h_menu: Hmenu) -> i32;
    fn GetCursorPos(lp_point: *mut Point) -> i32;
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn Shell_NotifyIconW(dw_message: u32, lp_data: *const NotifyIconDataW) -> i32;
}

#[link(name = "comctl32")]
unsafe extern "system" {
    fn SetWindowSubclass(
        hwnd: Hwnd,
        pfn_subclass: SubclassProc,
        u_id_subclass: usize,
        dw_ref_data: usize,
    ) -> i32;
    fn RemoveWindowSubclass(hwnd: Hwnd, pfn_subclass: SubclassProc, u_id_subclass: usize) -> i32;
    fn DefSubclassProc(hwnd: Hwnd, u_msg: u32, w_param: usize, l_param: isize) -> isize;
}

fn write_wide_fixed<const N: usize>(buf: &mut [u16; N], text: &str) {
    buf.fill(0);
    let limit = N.saturating_sub(1);
    let mut pos = 0;
    for ch in text.chars() {
        let mut enc = [0u16; 2];
        let units = ch.encode_utf16(&mut enc);
        if pos + units.len() > limit {
            break;
        }
        buf[pos..pos + units.len()].copy_from_slice(units);
        pos += units.len();
    }
}

struct SearchCtx<'a> {
    title_hint: &'a str,
    matched: Hwnd,
    fallback: Hwnd,
}

unsafe extern "system" fn enum_thread_proc(hwnd: Hwnd, l_param: isize) -> i32 {
    let ctx = unsafe { &mut *(l_param as *mut SearchCtx<'_>) };
    let owner = unsafe { GetWindow(hwnd, GW_OWNER) };
    if !owner.is_null() {
        return 1;
    }

    let mut title_buf = [0u16; 256];
    let title_len = unsafe { GetWindowTextW(hwnd, title_buf.as_mut_ptr(), 256) };
    if title_len > 0 {
        let title = String::from_utf16_lossy(&title_buf[..title_len as usize]);
        if title.contains(ctx.title_hint) {
            ctx.matched = hwnd;
            return 0;
        }
    }

    let mut class_buf = [0u16; 128];
    let class_len = unsafe { GetClassNameW(hwnd, class_buf.as_mut_ptr(), 128) };
    if class_len > 0 {
        let class_name = String::from_utf16_lossy(&class_buf[..class_len as usize]);
        if class_name == "WinUIDesktopWin32WindowClass" || ctx.fallback.is_null() {
            ctx.fallback = hwnd;
        }
    }
    1
}

fn find_main_window_on_thread(title_hint: &str) -> Option<Hwnd> {
    let mut ctx = SearchCtx {
        title_hint,
        matched: null_mut(),
        fallback: null_mut(),
    };
    let ptr = (&raw mut ctx) as isize;
    unsafe {
        EnumThreadWindows(GetCurrentThreadId(), enum_thread_proc, ptr);
    }
    let found = if !ctx.matched.is_null() {
        ctx.matched
    } else {
        ctx.fallback
    };
    if found.is_null() { None } else { Some(found) }
}

fn is_taskbar_light() -> bool {
    reg_query_dword(
        r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
        "SystemUsesLightTheme",
    ) == Some(1)
}

fn load_icon_resource(res_id: usize, cx_metric: i32, cy_metric: i32) -> Hicon {
    unsafe {
        let hinst = GetModuleHandleW(null());
        let cx = GetSystemMetrics(cx_metric).max(16);
        let cy = GetSystemMetrics(cy_metric).max(16);
        LoadImageW(
            hinst,
            core::ptr::without_provenance(res_id),
            IMAGE_ICON,
            cx,
            cy,
            LR_DEFAULTCOLOR,
        )
    }
}

fn apply_window_icons(hwnd: Hwnd) {
    let small = load_icon_resource(ICON_RES_APP, SM_CXSMICON, SM_CYSMICON);
    if !small.is_null() {
        unsafe {
            SendMessageW(hwnd, WM_SETICON, ICON_SMALL, small as isize);
        }
        let old = WIN_ICON_SMALL.swap(small as isize, Ordering::AcqRel) as Hicon;
        if !old.is_null() && old != small {
            unsafe {
                DestroyIcon(old);
            }
        }
    }

    let big = load_icon_resource(ICON_RES_APP, SM_CXICON, SM_CYICON);
    if !big.is_null() {
        unsafe {
            SendMessageW(hwnd, WM_SETICON, ICON_BIG, big as isize);
        }
        let old = WIN_ICON_BIG.swap(big as isize, Ordering::AcqRel) as Hicon;
        if !old.is_null() && old != big {
            unsafe {
                DestroyIcon(old);
            }
        }
    }
}

fn desired_tray_icon_for_theme(light: bool) -> Hicon {
    let primary_res = if light {
        ICON_RES_TRAY_LIGHT
    } else {
        ICON_RES_TRAY_DARK
    };
    let mut icon = load_icon_resource(primary_res, SM_CXSMICON, SM_CYSMICON);
    if icon.is_null() {
        icon = load_icon_resource(ICON_RES_APP, SM_CXSMICON, SM_CYSMICON);
    }
    icon
}

fn load_tray_icon() -> Hicon {
    let light = is_taskbar_light();
    let desired_state = if light { 2 } else { 1 };
    let existing = TRAY_ICON.load(Ordering::Acquire) as Hicon;
    if !existing.is_null() && TRAY_THEME_STATE.load(Ordering::Acquire) == desired_state {
        return existing;
    }

    let icon = desired_tray_icon_for_theme(light);
    if icon.is_null() {
        if !existing.is_null() {
            return existing;
        }
        return unsafe { LoadIconW(null_mut(), IDI_APPLICATION) };
    }
    TRAY_THEME_STATE.store(desired_state, Ordering::Release);
    let old = TRAY_ICON.swap(icon as isize, Ordering::AcqRel) as Hicon;
    if !old.is_null() && old != icon {
        unsafe {
            DestroyIcon(old);
        }
    }
    icon
}

fn refresh_tray_icon(hwnd: Hwnd) {
    let light = is_taskbar_light();
    let next_state = if light { 2 } else { 1 };
    if TRAY_THEME_STATE.load(Ordering::Acquire) == next_state {
        return;
    }

    let new_icon = desired_tray_icon_for_theme(light);
    if new_icon.is_null() {
        return;
    }

    let mut nid = base_nid(hwnd);
    nid.u_flags = NIF_ICON;
    nid.h_icon = new_icon;
    let updated = unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) } != 0;
    if updated {
        TRAY_THEME_STATE.store(next_state, Ordering::Release);
        let old = TRAY_ICON.swap(new_icon as isize, Ordering::AcqRel) as Hicon;
        if !old.is_null() && old != new_icon {
            unsafe {
                DestroyIcon(old);
            }
        }
    } else {
        unsafe {
            DestroyIcon(new_icon);
        }
    }
}

fn base_nid(hwnd: Hwnd) -> NotifyIconDataW {
    NotifyIconDataW {
        cb_size: size_of::<NotifyIconDataW>() as u32,
        h_wnd: hwnd,
        u_id: TRAY_ICON_ID,
        u_flags: 0,
        u_callback_message: 0,
        h_icon: null_mut(),
        sz_tip: [0; 128],
        dw_state: 0,
        dw_state_mask: 0,
        sz_info: [0; 256],
        u_timeout_or_version: 0,
        sz_info_title: [0; 64],
        dw_info_flags: 0,
        guid_item: Guid {
            data1: 0,
            data2: 0,
            data3: 0,
            data4: [0; 8],
        },
        h_balloon_icon: null_mut(),
    }
}

fn add_tray_icon(hwnd: Hwnd) -> bool {
    let mut nid = base_nid(hwnd);
    nid.u_flags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.u_callback_message = WM_APP_TRAY;
    nid.h_icon = load_tray_icon();
    write_wide_fixed(&mut nid.sz_tip, "RDP Center");
    let added = unsafe { Shell_NotifyIconW(NIM_ADD, &nid) } != 0;
    TRAY_ADDED.store(added, Ordering::Release);
    added
}

fn remove_tray_icon(hwnd: Hwnd) {
    let nid = base_nid(hwnd);
    unsafe {
        Shell_NotifyIconW(NIM_DELETE, &nid);
    }
    TRAY_ADDED.store(false, Ordering::Release);
}

fn show_and_focus(hwnd: Hwnd) {
    unsafe {
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        } else {
            ShowWindow(hwnd, SW_SHOW);
        }
        SetForegroundWindow(hwnd);
    }
}

fn show_tray_error(hwnd: Hwnd, message: &str) {
    let mut nid = base_nid(hwnd);
    nid.u_flags = NIF_INFO;
    nid.dw_info_flags = NIIF_ERROR;
    write_wide_fixed(&mut nid.sz_info_title, "RDP Center");
    write_wide_fixed(&mut nid.sz_info, message);
    unsafe {
        Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

struct MenuGuard;
impl Drop for MenuGuard {
    fn drop(&mut self) {
        MENU_OPEN.store(false, Ordering::Release);
    }
}

fn show_context_menu(hwnd: Hwnd) {
    if MENU_OPEN
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let _guard = MenuGuard;

    let snapshot = QUICK_ASSETS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let ordered = quick_remote_order(&snapshot);

    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        let open_w = to_wide("Open RDP Center");
        let hide_w = to_wide("Hide to Tray");
        let exit_w = to_wide("Exit");

        AppendMenuW(menu, MF_STRING, CMD_OPEN as usize, open_w.as_ptr());
        SetMenuDefaultItem(menu, CMD_OPEN, 0);
        if IsWindowVisible(hwnd) != 0 {
            AppendMenuW(menu, MF_STRING, CMD_HIDE as usize, hide_w.as_ptr());
        }

        if !ordered.is_empty() {
            AppendMenuW(menu, MF_SEPARATOR, 0, null());
            let header_w = to_wide("Quick Remote");
            AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, header_w.as_ptr());

            for (i, asset) in ordered.iter().take(MAX_QUICK_ITEMS).enumerate() {
                let label_w = to_wide(&format_tray_item_label(asset));
                AppendMenuW(
                    menu,
                    MF_STRING,
                    (CMD_ASSET_BASE as usize) + i,
                    label_w.as_ptr(),
                );
            }

            if ordered.len() > MAX_QUICK_ITEMS {
                let more_w = to_wide(&format!(
                    "… +{} more (Open RDP Center)",
                    ordered.len() - MAX_QUICK_ITEMS
                ));
                AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, more_w.as_ptr());
            }
        }

        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        AppendMenuW(menu, MF_STRING, CMD_EXIT as usize, exit_w.as_ptr());

        let mut pt = Point { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenuEx(
            menu,
            TPM_RIGHTBUTTON | TPM_NONOTIFY | TPM_RETURNCMD,
            pt.x,
            pt.y,
            hwnd,
            null(),
        );
        DestroyMenu(menu);

        if MAIN_HWND.load(Ordering::Acquire) != hwnd as isize {
            return;
        }
        PostMessageW(hwnd, WM_NULL, 0, 0);

        match cmd as u32 {
            CMD_OPEN => show_and_focus(hwnd),
            CMD_HIDE => {
                ShowWindow(hwnd, SW_HIDE);
            }
            CMD_EXIT => {
                let _ = request_exit();
            }
            id if id >= CMD_ASSET_BASE => {
                if let Some(asset) = ordered.get((id - CMD_ASSET_BASE) as usize)
                    && let Err(err) = launcher::launch_asset(asset)
                {
                    show_tray_error(hwnd, &err);
                }
            }
            _ => {}
        }
    }
}

fn destroy_cached_icon(slot: &AtomicIsize) {
    let icon = slot.swap(0, Ordering::AcqRel) as Hicon;
    if !icon.is_null() {
        unsafe {
            DestroyIcon(icon);
        }
    }
}

unsafe extern "system" fn tray_subclass_proc(
    hwnd: Hwnd,
    u_msg: u32,
    w_param: usize,
    l_param: isize,
    _u_id_subclass: usize,
    _dw_ref_data: usize,
) -> isize {
    let taskbar_msg = WM_TASKBAR_CREATED.load(Ordering::Relaxed);
    if taskbar_msg != 0 && u_msg == taskbar_msg {
        add_tray_icon(hwnd);
    }

    match u_msg {
        WM_CLOSE => {
            if !ALLOW_EXIT.load(Ordering::Acquire)
                && CLOSE_TO_TRAY.load(Ordering::Acquire)
                && TRAY_ADDED.load(Ordering::Acquire)
            {
                unsafe {
                    ShowWindow(hwnd, SW_HIDE);
                }
                return 0;
            }
            remove_tray_icon(hwnd);
        }
        WM_SYSCOMMAND => {
            if TRAY_ADDED.load(Ordering::Acquire)
                && MINIMIZE_TO_TRAY.load(Ordering::Acquire)
                && (w_param & 0xFFF0) == SC_MINIMIZE
            {
                unsafe {
                    ShowWindow(hwnd, SW_HIDE);
                }
                return 0;
            }
        }
        WM_SIZE => {
            if TRAY_ADDED.load(Ordering::Acquire)
                && MINIMIZE_TO_TRAY.load(Ordering::Acquire)
                && w_param == SIZE_MINIMIZED
            {
                unsafe {
                    ShowWindow(hwnd, SW_HIDE);
                }
            }
        }
        WM_SETTINGCHANGE => refresh_tray_icon(hwnd),
        WM_APP_TRAY => {
            let event = (l_param as usize & 0xFFFF) as u32;
            match event {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => show_and_focus(hwnd),
                WM_RBUTTONUP | WM_CONTEXTMENU => show_context_menu(hwnd),
                _ => {}
            }
            return 0;
        }
        WM_DESTROY => {
            remove_tray_icon(hwnd);
            destroy_cached_icon(&TRAY_ICON);
            destroy_cached_icon(&WIN_ICON_SMALL);
            destroy_cached_icon(&WIN_ICON_BIG);
            TRAY_THEME_STATE.store(0, Ordering::Release);
            unsafe {
                RemoveWindowSubclass(hwnd, tray_subclass_proc, SUBCLASS_ID);
            }
            MAIN_HWND.store(0, Ordering::Release);
            INSTALLED.store(false, Ordering::Release);
        }
        _ => {}
    }

    unsafe { DefSubclassProc(hwnd, u_msg, w_param, l_param) }
}

pub fn install_for_current_thread(
    title_hint: &str,
    close_to_tray: bool,
    minimize_to_tray: bool,
    start_minimized: bool,
) -> bool {
    update_behavior(close_to_tray, minimize_to_tray);
    if INSTALLED.load(Ordering::Acquire) {
        return true;
    }

    let Some(hwnd) = find_main_window_on_thread(title_hint) else {
        return false;
    };

    apply_window_icons(hwnd);

    let msg_w = to_wide("TaskbarCreated");
    let taskbar_msg = unsafe { RegisterWindowMessageW(msg_w.as_ptr()) };
    WM_TASKBAR_CREATED.store(taskbar_msg, Ordering::Relaxed);

    let ok = unsafe { SetWindowSubclass(hwnd, tray_subclass_proc, SUBCLASS_ID, 0) };
    if ok == 0 {
        return false;
    }

    MAIN_HWND.store(hwnd as isize, Ordering::Release);
    INSTALLED.store(true, Ordering::Release);
    let tray_ok = add_tray_icon(hwnd);

    if start_minimized && tray_ok {
        unsafe {
            ShowWindow(hwnd, SW_HIDE);
        }
    }

    tray_ok
}

pub fn update_behavior(close_to_tray: bool, minimize_to_tray: bool) {
    CLOSE_TO_TRAY.store(close_to_tray, Ordering::Release);
    MINIMIZE_TO_TRAY.store(minimize_to_tray, Ordering::Release);
}

pub fn set_quick_assets(assets: Vec<Asset>) {
    *QUICK_ASSETS.lock().unwrap_or_else(|e| e.into_inner()) = assets;
}

pub fn is_window_visible() -> bool {
    let hwnd = MAIN_HWND.load(Ordering::Acquire) as Hwnd;
    if hwnd.is_null() {
        return true;
    }
    unsafe { IsWindowVisible(hwnd) != 0 }
}

pub fn hide_to_tray() -> bool {
    if !TRAY_ADDED.load(Ordering::Acquire) {
        return false;
    }
    let hwnd = MAIN_HWND.load(Ordering::Acquire) as Hwnd;
    if hwnd.is_null() {
        return false;
    }
    unsafe {
        ShowWindow(hwnd, SW_HIDE);
    }
    true
}

pub fn request_exit() -> bool {
    ALLOW_EXIT.store(true, Ordering::Release);
    let hwnd = MAIN_HWND.load(Ordering::Acquire) as Hwnd;
    if hwnd.is_null() {
        return false;
    }
    remove_tray_icon(hwnd);
    unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) != 0 }
}

#[cfg(all(test, target_arch = "x86_64"))]
mod abi_tests {
    use super::*;

    #[test]
    fn notify_icon_data_w_size_matches_win64_abi() {
        assert_eq!(size_of::<NotifyIconDataW>(), 976);
    }
}

#[cfg(test)]
mod helper_tests {
    use super::*;

    fn mk(name: &str, protocol: Protocol, host: &str, port: Option<u16>) -> Asset {
        Asset {
            name: name.into(),
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
    fn quick_remote_order_puts_rdp_first_preserving_order() {
        let items = vec![
            mk("NAS", Protocol::Web, "192.168.1.30", Some(5000)),
            mk("DC01", Protocol::Rdp, "192.168.1.10", None),
            mk("APP01", Protocol::Ssh, "192.168.1.20", None),
            mk("DC02", Protocol::Rdp, "192.168.1.11", None),
        ];

        let ordered = quick_remote_order(&items);
        let names: Vec<&str> = ordered.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["DC01", "DC02", "APP01", "NAS"]);
    }

    #[test]
    fn format_tray_item_label_sanitizes_escapes_and_truncates() {
        let asset = mk(
            "R&D\tVery Long Server Name That Exceeds Thirty Six Characters",
            Protocol::Rdp,
            "192.168.1.10",
            None,
        );
        let label = format_tray_item_label(&asset);
        assert!(label.starts_with("[RDP] R&&D "));
        assert!(label.contains('…'));
        assert_eq!(label.matches('\t').count(), 1);
        assert!(label.ends_with("\t192.168.1.10"));
    }

    #[test]
    fn write_wide_fixed_never_splits_surrogate_pair_and_null_terminates() {
        let mut buf = [0u16; 4]; // max 3 UTF-16 code units + trailing NUL
        write_wide_fixed(&mut buf, "a🚀b");
        // 'a' = 1 unit, '🚀' = 2 units (fits in 3), 'b' dropped, index 3 = 0
        assert_eq!(buf[3], 0);
        assert_eq!(String::from_utf16(&buf[..3]).unwrap(), "a🚀");

        let mut small = [0u16; 3]; // max 2 UTF-16 code units + trailing NUL
        write_wide_fixed(&mut small, "a🚀");
        // '🚀' needs 2 units so would overflow index 2 -> must stop cleanly after 'a'
        assert_eq!(small[0], b'a' as u16);
        assert_eq!(small[1], 0);
        assert_eq!(small[2], 0);
    }
}
