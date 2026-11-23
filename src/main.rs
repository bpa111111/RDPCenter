#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod fuzzy;
mod launcher;
mod model;
mod probe;
mod rdp;
mod startup;
mod tray;

use model::{AppSettings, Asset, Protocol, ThemeMode};
use probe::{OnlineStatus, ProbeKey};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;
use windows_reactor::*;

const AUTO_REFRESH_SECS: u64 = 30;

fn main() {
    if let Err(err) = App::run_component::<LauncherApp>(()) {
        eprintln!("Failed to run RDP Center: {err}");
    }
}

#[derive(Debug, Clone)]
struct AssetForm {
    edit_index: Option<usize>,
    name: String,
    host: String,
    protocol: Protocol,
    port: String,
    username: String,
    category: String,
    notes: String,
    rdp_file: String,
}

impl AssetForm {
    fn new_blank() -> Self {
        Self {
            edit_index: None,
            name: String::new(),
            host: String::new(),
            protocol: Protocol::Rdp,
            port: String::new(),
            username: String::new(),
            category: "Server".into(),
            notes: String::new(),
            rdp_file: String::new(),
        }
    }

    fn from_asset(index: usize, asset: &Asset) -> Self {
        Self {
            edit_index: Some(index),
            name: asset.name.clone(),
            host: asset.host.clone(),
            protocol: asset.protocol,
            port: asset.port.map_or_else(String::new, |p| p.to_string()),
            username: asset.username.clone().unwrap_or_default(),
            category: asset.category.clone(),
            notes: asset.notes.clone(),
            rdp_file: asset.rdp_file.clone().unwrap_or_default(),
        }
    }

    fn to_asset(&self) -> Result<Asset, String> {
        let port = if self.port.trim().is_empty() {
            None
        } else {
            let parsed = self
                .port
                .trim()
                .parse::<u16>()
                .map_err(|_| "Port must be a valid number (1-65535)".to_string())?;
            if parsed == 0 {
                return Err("Port cannot be 0".into());
            }
            Some(parsed)
        };

        let opt_str = |s: &str| {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        };

        let asset = Asset {
            name: self.name.trim().to_string(),
            host: self.host.trim().to_string(),
            protocol: self.protocol,
            port,
            username: opt_str(&self.username),
            category: self.category.trim().to_string(),
            notes: self.notes.trim().to_string(),
            rdp_file: if self.protocol == Protocol::Rdp {
                opt_str(&self.rdp_file)
            } else {
                None
            },
        };
        asset.validate()?;
        Ok(asset)
    }
}

#[derive(Debug, Clone)]
struct ImportForm {
    folder_path: String,
}

#[derive(Debug, Clone)]
enum ActiveDialog {
    None,
    Editor(AssetForm),
    Import(ImportForm),
    Settings,
}

#[derive(Clone)]
enum Msg {
    QueryChanged(String),
    ClearQuery,
    FilterProtocol(Option<Protocol>),
    SelectPos(Option<usize>),
    LaunchSelected,
    LaunchAsset(usize),
    CopyHost(String),
    OpenAdd,
    OpenEdit(usize),
    OpenImport,
    OpenSettings,
    EditName(String),
    EditProtocol(Protocol),
    EditHost(String),
    EditPort(String),
    EditUsername(String),
    EditCategory(String),
    EditNotes(String),
    EditRdpFile(String),
    ImportPathChanged(String),
    DialogClosed(ContentDialogResult),
    DeleteAsset(usize),
    RefreshStatus(bool),
    AutoRefreshTick,
    ProbeFinished(HashMap<ProbeKey, OnlineStatus>),
    ProbeRejected,
    ImportFinished(Vec<Asset>, rdp::ImportSummary),
    BackgroundBusy(&'static str),
    ToggleTheme,
    ColorSchemeChanged(ColorScheme),
    SetMinimizeToTray(bool),
    SetCloseToTray(bool),
    SetStartWithWindows(bool),
    SetStartMinimized(bool),
    DragEnter(DragKind),
    DragLeave,
    Dropped(DroppedData),
}

struct LauncherApp {
    config_path: PathBuf,
    assets: Vec<Asset>,
    asset_ids: Vec<u64>,
    next_asset_id: u64,
    assets_load_failed: bool,
    settings: AppSettings,
    start_minimized_on_boot: bool,
    effective_scheme: ColorScheme,
    query: String,
    protocol_filter: Option<Protocol>,
    selected_pos: usize,
    statuses: HashMap<ProbeKey, OnlineStatus>,
    is_checking: bool,
    probe_task: Option<ComponentTask>,
    timer_task: Option<ComponentTask>,
    import_task: Option<ComponentTask>,
    toast: Option<(String, InfoBarSeverity)>,
    drag_active: bool,
    dialog: ActiveDialog,
    pending_delete: Option<usize>,
    search_ref: ElementRef<TextBox>,
    focus_rev: u64,
}

impl LauncherApp {
    fn push_asset(&mut self, asset: Asset) {
        let id = self.next_asset_id;
        self.next_asset_id += 1;
        self.assets.push(asset);
        self.asset_ids.push(id);
    }

    fn probe_keys(&self) -> Vec<ProbeKey> {
        self.assets
            .iter()
            .filter_map(|asset| {
                asset
                    .probe_target()
                    .map(|(host, port)| ProbeKey { host, port })
            })
            .collect()
    }

    fn prune_statuses(&mut self) {
        let valid: HashSet<ProbeKey> = self.probe_keys().into_iter().collect();
        self.statuses.retain(|k, _| valid.contains(k));
    }

    fn asset_status_and_target(&self, asset: &Asset) -> (OnlineStatus, Option<(String, u16)>) {
        let Some((host, port)) = asset.probe_target() else {
            return (OnlineStatus::Unknown, None);
        };
        let key = ProbeKey {
            host: host.clone(),
            port,
        };
        let status = if let Some(&s) = self.statuses.get(&key) {
            s
        } else if self.is_checking {
            OnlineStatus::Checking
        } else {
            OnlineStatus::Unknown
        };
        (status, Some((host, port)))
    }

    fn start_probe(&mut self, show_checking: bool, context: &ComponentContext<Self>) {
        if let Some(prev) = self.probe_task.take() {
            prev.cancel();
        }
        let keys = self.probe_keys();
        if keys.is_empty() {
            self.is_checking = false;
            return;
        }
        if show_checking {
            for key in &keys {
                self.statuses.insert(key.clone(), OnlineStatus::Checking);
            }
        }
        self.is_checking = true;
        self.probe_task = Some(context.spawn_background_with_rejection(
            move |cancel| {
                let results = probe::probe_batch(keys, &|| cancel.is_cancelled());
                Msg::ProbeFinished(results)
            },
            Msg::ProbeRejected,
        ));
    }

    fn schedule_auto_refresh(&mut self, context: &ComponentContext<Self>) {
        if let Some(prev) = self.timer_task.take() {
            prev.cancel();
        }
        self.timer_task = Some(context.spawn_background_with_rejection(
            move |cancel| {
                let total_steps = AUTO_REFRESH_SECS * 2;
                for _ in 0..total_steps {
                    if cancel.is_cancelled() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
                Msg::AutoRefreshTick
            },
            Msg::BackgroundBusy("Auto-refresh paused (background queue busy)"),
        ));
    }

    fn set_toast(&mut self, msg: impl Into<String>, severity: InfoBarSeverity) {
        self.toast = Some((msg.into(), severity));
    }

    fn save_current_assets(&mut self) -> bool {
        if self.assets_load_failed {
            self.set_toast(
                "Read-only: fix or remove corrupt assets.json before saving",
                InfoBarSeverity::Error,
            );
            return false;
        }
        tray::set_quick_assets(self.assets.clone());
        match model::save_assets(&self.config_path, &self.assets) {
            Ok(()) => true,
            Err(err) => {
                self.set_toast(err, InfoBarSeverity::Error);
                false
            }
        }
    }

    fn launch_index(&mut self, asset_idx: usize) {
        let Some(asset) = self.assets.get(asset_idx) else {
            return;
        };
        match launcher::launch_asset(asset) {
            Ok(msg) => self.set_toast(msg, InfoBarSeverity::Success),
            Err(err) => self.set_toast(err, InfoBarSeverity::Error),
        }
    }

    fn start_import(&mut self, paths: Vec<PathBuf>, context: &ComponentContext<Self>) {
        if paths.is_empty() {
            return;
        }
        if let Some(prev) = self.import_task.take() {
            prev.cancel();
        }
        let mut snapshot = self.assets.clone();
        let before = snapshot.len();
        self.set_toast("Importing .rdp files...", InfoBarSeverity::Informational);
        self.import_task = Some(context.spawn_background_with_rejection(
            move |_cancel| {
                let summary = rdp::import_rdp_paths(&paths, &mut snapshot);
                let added = snapshot.split_off(before);
                Msg::ImportFinished(added, summary)
            },
            Msg::BackgroundBusy("Background queue busy, import skipped"),
        ));
    }

    fn filtered_assets(&self) -> Vec<(usize, fuzzy::MatchResult)> {
        fuzzy::filter_and_sort(&self.query, self.protocol_filter, &self.assets)
    }

    fn clamp_selection(&mut self, len: usize) {
        if len == 0 {
            self.selected_pos = 0;
        } else if self.selected_pos >= len {
            self.selected_pos = len - 1;
        }
    }

    fn focus_search(&mut self) {
        self.focus_rev = self.focus_rev.wrapping_add(1);
    }
}

impl Component for LauncherApp {
    type Input = ();
    type Message = Msg;

    fn create(_input: &(), context: &ComponentContext<Self>) -> Self {
        launcher::cleanup_stale_sessions();
        let config_path = model::resolve_config_path();
        let mut settings = model::load_settings(&config_path);
        settings.start_with_windows = startup::is_enabled();
        let start_minimized_on_boot = settings.start_minimized || startup::has_minimized_flag();
        let load_res = model::load_or_seed(&config_path);
        tray::set_quick_assets(load_res.assets.clone());
        let asset_ids: Vec<u64> = (1..=load_res.assets.len() as u64).collect();
        let next_asset_id = (load_res.assets.len() as u64) + 1;
        let toast = load_res.notice.map(|m| {
            let sev = if load_res.load_failed || m.starts_with("Failed") {
                InfoBarSeverity::Error
            } else {
                InfoBarSeverity::Informational
            };
            (m, sev)
        });

        let mut app = Self {
            config_path,
            assets: load_res.assets,
            asset_ids,
            next_asset_id,
            assets_load_failed: load_res.load_failed,
            settings,
            start_minimized_on_boot,
            effective_scheme: ColorScheme::default(),
            query: String::new(),
            protocol_filter: None,
            selected_pos: 0,
            statuses: HashMap::new(),
            is_checking: false,
            probe_task: None,
            timer_task: None,
            import_task: None,
            toast,
            drag_active: false,
            dialog: ActiveDialog::None,
            pending_delete: None,
            search_ref: ElementRef::new(),
            focus_rev: u64::from(!start_minimized_on_boot),
        };

        app.start_probe(false, context);
        app.schedule_auto_refresh(context);
        app
    }

    fn update(&mut self, msg: Msg, context: &ComponentContext<Self>) {
        match msg {
            Msg::QueryChanged(q) => {
                self.query = q;
                self.selected_pos = 0;
                self.pending_delete = None;
            }
            Msg::ClearQuery => {
                self.query.clear();
                self.selected_pos = 0;
                self.pending_delete = None;
                self.focus_search();
            }
            Msg::FilterProtocol(proto) => {
                self.protocol_filter = proto;
                self.selected_pos = 0;
                self.pending_delete = None;
            }
            Msg::SelectPos(pos) => {
                if let Some(p) = pos {
                    self.selected_pos = p;
                    self.pending_delete = None;
                }
            }
            Msg::LaunchSelected => {
                if matches!(self.dialog, ActiveDialog::None) {
                    let filtered = self.filtered_assets();
                    self.clamp_selection(filtered.len());
                    if let Some(&(asset_idx, _)) = filtered.get(self.selected_pos) {
                        self.launch_index(asset_idx);
                    }
                }
            }
            Msg::LaunchAsset(asset_idx) => {
                self.launch_index(asset_idx);
            }
            Msg::CopyHost(host) => match clipboard_win::set_clipboard_string(&host) {
                Ok(()) => self.set_toast(format!("Copied host -> {host}"), InfoBarSeverity::Success),
                Err(err) => self.set_toast(
                    format!("Failed to copy host: {err}"),
                    InfoBarSeverity::Error,
                ),
            },
            Msg::OpenAdd => {
                self.pending_delete = None;
                self.dialog = ActiveDialog::Editor(AssetForm::new_blank());
            }
            Msg::OpenEdit(idx) => {
                self.pending_delete = None;
                if let Some(asset) = self.assets.get(idx) {
                    self.dialog = ActiveDialog::Editor(AssetForm::from_asset(idx, asset));
                }
            }
            Msg::OpenImport => {
                self.pending_delete = None;
                self.dialog = ActiveDialog::Import(ImportForm {
                    folder_path: String::new(),
                });
            }
            Msg::OpenSettings => {
                self.pending_delete = None;
                self.dialog = ActiveDialog::Settings;
            }
            Msg::EditName(v) => {
                if let ActiveDialog::Editor(form) = &mut self.dialog {
                    form.name = v;
                }
            }
            Msg::EditProtocol(p) => {
                if let ActiveDialog::Editor(form) = &mut self.dialog {
                    form.protocol = p;
                }
            }
            Msg::EditHost(v) => {
                if let ActiveDialog::Editor(form) = &mut self.dialog {
                    form.host = v;
                    if form.protocol == Protocol::Rdp {
                        form.rdp_file.clear();
                    }
                }
            }
            Msg::EditPort(v) => {
                if let ActiveDialog::Editor(form) = &mut self.dialog {
                    form.port = v;
                    if form.protocol == Protocol::Rdp {
                        form.rdp_file.clear();
                    }
                }
            }
            Msg::EditUsername(v) => {
                if let ActiveDialog::Editor(form) = &mut self.dialog {
                    form.username = v;
                    if form.protocol == Protocol::Rdp {
                        form.rdp_file.clear();
                    }
                }
            }
            Msg::EditCategory(v) => {
                if let ActiveDialog::Editor(form) = &mut self.dialog {
                    form.category = v;
                }
            }
            Msg::EditNotes(v) => {
                if let ActiveDialog::Editor(form) = &mut self.dialog {
                    form.notes = v;
                }
            }
            Msg::EditRdpFile(v) => {
                if let ActiveDialog::Editor(form) = &mut self.dialog {
                    form.rdp_file = v;
                }
            }
            Msg::ImportPathChanged(v) => {
                if let ActiveDialog::Import(form) = &mut self.dialog {
                    form.folder_path = v;
                }
            }
            Msg::DialogClosed(result) => {
                let active = std::mem::replace(&mut self.dialog, ActiveDialog::None);
                match active {
                    ActiveDialog::Editor(form) if result == ContentDialogResult::Primary => {
                        match form.to_asset() {
                            Ok(asset) => {
                                let name = asset.name.clone();
                                if let Some(idx) = form.edit_index {
                                    if idx < self.assets.len() {
                                        self.assets[idx] = asset;
                                    }
                                } else {
                                    self.push_asset(asset);
                                }
                                let len = self.filtered_assets().len();
                                self.clamp_selection(len);
                                self.prune_statuses();
                                if self.save_current_assets() {
                                    self.set_toast(format!("Saved {name}"), InfoBarSeverity::Success);
                                }
                                self.start_probe(false, context);
                            }
                            Err(err) => self.set_toast(err, InfoBarSeverity::Error),
                        }
                    }
                    ActiveDialog::Import(form) if result == ContentDialogResult::Primary => {
                        let trimmed = form.folder_path.trim();
                        if !trimmed.is_empty() {
                            self.start_import(vec![PathBuf::from(trimmed)], context);
                        }
                    }
                    ActiveDialog::Settings => match result {
                        ContentDialogResult::Primary => {
                            if !tray::hide_to_tray() {
                                self.set_toast(
                                    "System tray icon is not active",
                                    InfoBarSeverity::Error,
                                );
                            }
                            return;
                        }
                        ContentDialogResult::Secondary => {
                            if !tray::request_exit() {
                                let _ = context.window().request_close();
                            }
                            return;
                        }
                        _ => {}
                    },
                    _ => {}
                }
                self.focus_search();
            }
            Msg::DeleteAsset(idx) => {
                if self.pending_delete == Some(idx) {
                    self.pending_delete = None;
                    if idx < self.assets.len() {
                        let removed = self.assets.remove(idx);
                        self.asset_ids.remove(idx);
                        let len = self.filtered_assets().len();
                        self.clamp_selection(len);
                        self.prune_statuses();
                        if self.save_current_assets() {
                            self.set_toast(
                                format!("Deleted {}", removed.name),
                                InfoBarSeverity::Informational,
                            );
                        }
                    }
                } else {
                    self.pending_delete = Some(idx);
                }
            }
            Msg::RefreshStatus(show_checking) => {
                if !self.is_checking {
                    self.start_probe(show_checking, context);
                }
            }
            Msg::AutoRefreshTick => {
                if !self.is_checking && tray::is_window_visible() {
                    self.start_probe(false, context);
                }
                self.schedule_auto_refresh(context);
            }
            Msg::ProbeFinished(results) => {
                for (k, v) in results {
                    self.statuses.insert(k, v);
                }
                self.is_checking = false;
                self.probe_task = None;
            }
            Msg::ProbeRejected => {
                self.is_checking = false;
                self.probe_task = None;
            }
            Msg::ImportFinished(added, summary) => {
                self.import_task = None;
                let mut imported_count = 0usize;
                let mut dup_count = summary.skipped_duplicates;
                if !added.is_empty() {
                    let mut seen: HashSet<(Protocol, String, u16)> = self
                        .assets
                        .iter()
                        .filter_map(|a| {
                            a.probe_target()
                                .map(|(h, p)| (a.protocol, h.to_ascii_lowercase(), p))
                        })
                        .collect();
                    for asset in added {
                        let key = asset
                            .probe_target()
                            .map(|(h, p)| (asset.protocol, h.to_ascii_lowercase(), p));
                        if let Some(k) = key
                            && !seen.insert(k)
                        {
                            dup_count += 1;
                            continue;
                        }
                        self.push_asset(asset);
                        imported_count += 1;
                    }
                    if imported_count > 0 {
                        let saved = self.save_current_assets();
                        self.start_probe(false, context);
                        if !saved {
                            return;
                        }
                    }
                }
                if imported_count == 0 && dup_count == 0 && summary.failed == 0 {
                    self.set_toast("No .rdp files found", InfoBarSeverity::Informational);
                } else {
                    let msg = format!(
                        "RDP import: {} added, {} duplicates skipped, {} failed",
                        imported_count, dup_count, summary.failed
                    );
                    let severity = if imported_count == 0 && summary.failed > 0 {
                        InfoBarSeverity::Error
                    } else {
                        InfoBarSeverity::Success
                    };
                    self.set_toast(msg, severity);
                }
            }
            Msg::BackgroundBusy(msg) => {
                self.import_task = None;
                self.set_toast(msg, InfoBarSeverity::Informational);
            }
            Msg::ToggleTheme => {
                self.settings.theme = self.settings.theme.toggle();
                match model::save_settings(&self.config_path, &self.settings) {
                    Ok(()) => self.set_toast(
                        format!("Switched to {} theme", self.settings.theme.label()),
                        InfoBarSeverity::Informational,
                    ),
                    Err(err) => self.set_toast(err, InfoBarSeverity::Error),
                }
            }
            Msg::ColorSchemeChanged(scheme) => {
                self.effective_scheme = scheme;
            }
            Msg::SetMinimizeToTray(v) => {
                if v == self.settings.minimize_to_tray {
                    return;
                }
                self.settings.minimize_to_tray = v;
                tray::update_behavior(self.settings.close_to_tray, self.settings.minimize_to_tray);
                if let Err(err) = model::save_settings(&self.config_path, &self.settings) {
                    self.set_toast(err, InfoBarSeverity::Error);
                }
            }
            Msg::SetCloseToTray(v) => {
                if v == self.settings.close_to_tray {
                    return;
                }
                self.settings.close_to_tray = v;
                tray::update_behavior(self.settings.close_to_tray, self.settings.minimize_to_tray);
                if let Err(err) = model::save_settings(&self.config_path, &self.settings) {
                    self.set_toast(err, InfoBarSeverity::Error);
                }
            }
            Msg::SetStartWithWindows(enabled) => {
                if enabled == self.settings.start_with_windows {
                    return;
                }
                match startup::set_enabled(enabled, self.settings.start_minimized) {
                    Ok(()) => {
                        self.settings.start_with_windows = enabled;
                        if let Err(err) = model::save_settings(&self.config_path, &self.settings) {
                            self.set_toast(err, InfoBarSeverity::Error);
                        } else {
                            let label = if enabled {
                                "Enabled Start with Windows"
                            } else {
                                "Disabled Start with Windows"
                            };
                            self.set_toast(label, InfoBarSeverity::Success);
                        }
                    }
                    Err(err) => self.set_toast(err, InfoBarSeverity::Error),
                }
            }
            Msg::SetStartMinimized(enabled) => {
                if enabled == self.settings.start_minimized {
                    return;
                }
                if self.settings.start_with_windows
                    && let Err(err) = startup::set_enabled(true, enabled)
                {
                    self.set_toast(err, InfoBarSeverity::Error);
                    return;
                }
                self.settings.start_minimized = enabled;
                if let Err(err) = model::save_settings(&self.config_path, &self.settings) {
                    self.set_toast(err, InfoBarSeverity::Error);
                }
            }
            Msg::DragEnter(kind) => {
                if kind == DragKind::StorageItems {
                    self.drag_active = true;
                }
            }
            Msg::DragLeave => {
                self.drag_active = false;
            }
            Msg::Dropped(data) => {
                self.drag_active = false;
                if let DroppedData::StorageItems(items) = data {
                    let paths: Vec<PathBuf> = items.into_iter().map(|i| PathBuf::from(i.path)).collect();
                    self.start_import(paths, context);
                }
            }
        }
    }

    fn view(&self, _input: &(), context: &mut ViewContext<Self>) -> View {
        let win_theme = match self.settings.theme {
            ThemeMode::System => WindowTheme::System,
            ThemeMode::Light => WindowTheme::Light,
            ThemeMode::Dark => WindowTheme::Dark,
        };
        context.on_color_scheme(context.callback(Msg::ColorSchemeChanged));
        context.window_title("RDP Center");
        let visuals = WindowVisuals::new()
            .backdrop(WindowBackdrop::Mica)
            .client_size(960.0, 650.0)
            .constraints(WindowConstraints {
                min_width: Some(680.0),
                min_height: Some(540.0),
                ..Default::default()
            })
            .theme(win_theme);
        context.window_visuals(visuals);

        let close_to_tray = self.settings.close_to_tray;
        let minimize_to_tray = self.settings.minimize_to_tray;
        let start_minimized = self.start_minimized_on_boot;
        context.use_effect("init_tray", (), move || {
            tray::install_for_current_thread(
                "RDP Center",
                close_to_tray,
                minimize_to_tray,
                start_minimized,
            );
            None
        });

        let focus_rev = self.focus_rev;
        let search_ref = self.search_ref.clone();
        context.use_effect("focus_search", focus_rev, move || {
            if focus_rev > 0 {
                let _ = search_ref.request_focus();
            }
            None
        });

        let filtered = self.filtered_assets();
        let selected_index = if filtered.is_empty() {
            None
        } else {
            Some(self.selected_pos.min(filtered.len() - 1))
        };

        let header = self.build_header(context);
        let body = self.build_body(&filtered, selected_index, context);
        let footer = self.build_footer();
        let dialog = self.build_dialog(context);

        let root_grid = Grid::new()
            .rows([GridLength::Auto, GridLength::STAR, GridLength::Auto])
            .row_spacing(10.0)
            .key_accelerators(KeyAccelerators::new([KeyAccelerator::new(
                AcceleratorKey::R,
                AcceleratorModifiers::Control,
                context.message(Msg::RefreshStatus(true)),
            )]))
            .children((header, body, footer, dialog));

        Border::new()
            .padding(Thickness::xy(20.0, 16.0))
            .background(Color::transparent())
            .drop_policy(
                DragDropPolicy::new().storage_items(
                    DragDropAction::new(DragDropOperation::Copy).caption("Import .rdp files"),
                ),
            )
            .on_drag_enter(context.callback(Msg::DragEnter))
            .on_drag_leave(context.message(Msg::DragLeave))
            .on_drop(context.callback(Msg::Dropped))
            .content(root_grid)
    }
}

impl LauncherApp {
    fn is_dark(&self) -> bool {
        match self.settings.theme {
            ThemeMode::Dark => true,
            ThemeMode::Light => false,
            ThemeMode::System => self.effective_scheme == ColorScheme::Dark,
        }
    }

    fn muted_color(&self) -> Color {
        if self.is_dark() {
            Color::rgb(148, 163, 184)
        } else {
            Color::rgb(100, 116, 139)
        }
    }

    fn online_color(&self) -> Color {
        if self.is_dark() {
            Color::rgb(74, 222, 128)
        } else {
            Color::rgb(21, 128, 61)
        }
    }

    fn offline_color(&self) -> Color {
        if self.is_dark() {
            Color::rgb(248, 113, 113)
        } else {
            Color::rgb(220, 38, 38)
        }
    }

    fn notice_color(&self, severity: InfoBarSeverity) -> Color {
        match severity {
            InfoBarSeverity::Success => self.online_color(),
            InfoBarSeverity::Error => self.offline_color(),
            _ => {
                if self.is_dark() {
                    Color::rgb(96, 165, 250)
                } else {
                    Color::rgb(37, 99, 235)
                }
            }
        }
    }

    fn build_header(&self, context: &ViewContext<Self>) -> View {
        let mut online_count = 0usize;
        let mut offline_count = 0usize;
        let mut proto_counts = [0usize; 3];
        for asset in &self.assets {
            let p_idx = match asset.protocol {
                Protocol::Rdp => 0,
                Protocol::Ssh => 1,
                Protocol::Web => 2,
            };
            proto_counts[p_idx] += 1;
            let (status, _) = self.asset_status_and_target(asset);
            match status {
                OnlineStatus::Online { .. } => online_count += 1,
                OnlineStatus::Offline => offline_count += 1,
                OnlineStatus::Unknown | OnlineStatus::Checking => {}
            }
        }

        let title_col = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(10.0)
            .grid_column(0)
            .children((
                TextBlock::new()
                    .text("RDP Center")
                    .font_size(20.0)
                    .font_weight(FontWeight::SEMI_BOLD)
                    .vertical_alignment(VerticalAlignment::Center),
                TextBlock::new()
                    .text("RDP · SSH · Web Admin")
                    .font_size(12.0)
                    .foreground(self.muted_color())
                    .vertical_alignment(VerticalAlignment::Center),
            ));

        let refresh_btn = if self.is_checking {
            Button::new()
                .is_enabled(false)
                .content(
                    StackPanel::new()
                        .orientation(Orientation::Horizontal)
                        .spacing(6.0)
                        .children((
                            ProgressRing::new()
                                .width(14.0)
                                .height(14.0)
                                .is_active(true)
                                .is_indeterminate(true),
                            TextBlock::new().text("Checking..."),
                        )),
                )
        } else {
            Button::new()
                .on_click(context.message(Msg::RefreshStatus(true)))
                .content("Refresh")
                .tooltip("Check online status (Ctrl+R)")
        };

        let actions_row = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .horizontal_alignment(HorizontalAlignment::Right)
            .grid_column(1)
            .children((
                Button::new()
                    .style(ButtonStyle::Accent)
                    .on_click(context.message(Msg::OpenAdd))
                    .content("+ Add"),
                Button::new()
                    .on_click(context.message(Msg::OpenImport))
                    .content("Import .rdp"),
                refresh_btn,
                Button::new()
                    .on_click(context.message(Msg::ToggleTheme))
                    .content(format!("Theme: {}", self.settings.theme.label()))
                    .tooltip("Cycle theme: System / Light / Dark"),
                Button::new()
                    .on_click(context.message(Msg::OpenSettings))
                    .content("Settings")
                    .tooltip("Tray & Windows Startup settings"),
            ));

        let top_row = Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .children((title_col, actions_row));

        let search_accelerators = if matches!(self.dialog, ActiveDialog::None) {
            Some(KeyAccelerators::new([KeyAccelerator::new(
                AcceleratorKey::Enter,
                AcceleratorModifiers::None,
                context.message(Msg::LaunchSelected),
            )]))
        } else {
            None
        };
        let mut search_wrap = Grid::new().grid_column(0);
        if let Some(accels) = search_accelerators {
            search_wrap = search_wrap.key_accelerators(accels);
        }
        let search_box = search_wrap.children((TextBox::new()
            .element_ref(&self.search_ref)
            .placeholder_text(
                "Fuzzy search name, IP, tag, Thai/English (e.g. nas backup, .10, บัญชี)...",
            )
            .text(&self.query)
            .on_text_changed(context.callback(Msg::QueryChanged)),));

        let search_row = if self.query.is_empty() {
            Grid::new()
                .columns([GridLength::STAR])
                .children((search_box,))
        } else {
            Grid::new()
                .columns([GridLength::STAR, GridLength::Auto])
                .column_spacing(8.0)
                .children((
                    search_box,
                    Button::new()
                        .on_click(context.message(Msg::ClearQuery))
                        .grid_column(1)
                        .content("Clear"),
                ))
        };

        let filter_btn = |label: String, active: bool, target: Option<Protocol>| {
            Button::new()
                .style(if active {
                    ButtonStyle::Accent
                } else {
                    ButtonStyle::Subtle
                })
                .on_click(context.message(Msg::FilterProtocol(target)))
                .content(label)
        };

        let mut pills = vec![(
            "all",
            filter_btn(
                format!("All ({})", self.assets.len()),
                self.protocol_filter.is_none(),
                None,
            ),
        )];
        for (p_idx, proto) in Protocol::ALL.into_iter().enumerate() {
            let count = proto_counts[p_idx];
            pills.push((
                proto.label(),
                filter_btn(
                    format!("{} ({count})", proto.label()),
                    self.protocol_filter == Some(proto),
                    Some(proto),
                ),
            ));
        }

        let filter_pills = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(6.0)
            .grid_column(0)
            .keyed_children(pills);

        let status_summary = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(12.0)
            .vertical_alignment(VerticalAlignment::Center)
            .grid_column(1)
            .children((
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(6.0)
                    .children((
                        Ellipse::new()
                            .width(8.0)
                            .height(8.0)
                            .fill(Color::rgb(34, 197, 94))
                            .vertical_alignment(VerticalAlignment::Center),
                        TextBlock::new()
                            .text(format!("{online_count} Online"))
                            .font_size(12.0)
                            .foreground(self.online_color()),
                    )),
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(6.0)
                    .children((
                        Ellipse::new()
                            .width(8.0)
                            .height(8.0)
                            .fill(Color::rgb(239, 68, 68))
                            .vertical_alignment(VerticalAlignment::Center),
                        TextBlock::new()
                            .text(format!("{offline_count} Offline"))
                            .font_size(12.0)
                            .foreground(self.offline_color()),
                    )),
            ));

        let filter_row = Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .children((filter_pills, status_summary));

        StackPanel::new()
            .spacing(10.0)
            .grid_row(0)
            .children((top_row, search_row, filter_row))
    }

    fn build_body(
        &self,
        filtered: &[(usize, fuzzy::MatchResult)],
        selected_index: Option<usize>,
        context: &ViewContext<Self>,
    ) -> View {
        if filtered.is_empty() {
            let empty_msg = if self.assets.is_empty() {
                "No assets yet. Click '+ Add' or drag & drop .rdp files onto this window."
            } else {
                "No matching machines found. Try a different keyword or clear the protocol filter."
            };
            return Border::new()
                .background(ThemeBrush::CardBackground)
                .border_brush(ThemeBrush::CardStroke)
                .border_thickness(Thickness::uniform(1.0))
                .corner_radius(CornerRadius::uniform(8.0))
                .padding(Thickness::uniform(24.0))
                .grid_row(1)
                .content(
                    StackPanel::new()
                        .spacing(12.0)
                        .horizontal_alignment(HorizontalAlignment::Center)
                        .vertical_alignment(VerticalAlignment::Center)
                        .children((
                            TextBlock::new()
                                .text(empty_msg)
                                .font_size(14.0)
                                .foreground(self.muted_color()),
                            Button::new()
                                .style(ButtonStyle::Accent)
                                .horizontal_alignment(HorizontalAlignment::Center)
                                .on_click(context.message(Msg::OpenAdd))
                                .content("+ Add IT Asset"),
                        )),
                );
        }

        let items: Vec<KeyedView> = filtered
            .iter()
            .enumerate()
            .map(|(pos, &(asset_idx, ref match_res))| {
                let is_selected = selected_index == Some(pos);
                let card = self.build_asset_card(
                    asset_idx,
                    &self.assets[asset_idx],
                    match_res,
                    is_selected,
                    context,
                );
                let key = self
                    .asset_ids
                    .get(asset_idx)
                    .copied()
                    .unwrap_or(asset_idx as u64);
                KeyedView::new(
                    key,
                    ListViewItem::new()
                        .tag(asset_idx.to_string())
                        .horizontal_alignment(HorizontalAlignment::Stretch)
                        .content(card),
                )
            })
            .collect();

        let list_view = ListView::new()
            .selection_mode(ListViewSelectionMode::Single)
            .selected_index(selected_index)
            .on_selection_changed(context.callback(Msg::SelectPos))
            .slots([SlotView::collection(ListViewSlot::Items, items)]);

        let mut list_wrap = Grid::new().grid_row(1);
        if matches!(self.dialog, ActiveDialog::None) {
            list_wrap = list_wrap.key_accelerators(KeyAccelerators::new([KeyAccelerator::new(
                AcceleratorKey::Enter,
                AcceleratorModifiers::None,
                context.message(Msg::LaunchSelected),
            )]));
        }
        list_wrap.children((list_view,))
    }

    fn build_asset_card(
        &self,
        asset_idx: usize,
        asset: &Asset,
        match_res: &fuzzy::MatchResult,
        is_selected: bool,
        context: &ViewContext<Self>,
    ) -> View {
        let (status, target_opt) = self.asset_status_and_target(asset);
        let dot_color = match status {
            OnlineStatus::Online { .. } => Color::rgb(34, 197, 94),
            OnlineStatus::Offline => Color::rgb(239, 68, 68),
            OnlineStatus::Checking => Color::rgb(245, 158, 11),
            OnlineStatus::Unknown => Color::rgb(107, 114, 128),
        };

        let target_label = target_opt
            .map_or_else(|| "invalid target".into(), |(h, p)| format!("TCP {h}:{p}"));
        let status_tooltip = match status {
            OnlineStatus::Online { latency_ms } => {
                format!("Online · {latency_ms} ms · {target_label}")
            }
            OnlineStatus::Offline => format!("Offline · {target_label}"),
            OnlineStatus::Checking => format!("Checking {target_label}..."),
            OnlineStatus::Unknown => format!("Pending check · {target_label}"),
        };

        let status_dot = Ellipse::new()
            .width(10.0)
            .height(10.0)
            .fill(dot_color)
            .vertical_alignment(VerticalAlignment::Center)
            .grid_column(0)
            .tooltip(status_tooltip);

        let badge_bg = match asset.protocol {
            Protocol::Rdp => Color::rgb(37, 99, 235),
            Protocol::Ssh => Color::rgb(5, 150, 105),
            Protocol::Web => Color::rgb(217, 119, 6),
        };

        let proto_badge = Border::new()
            .width(46.0)
            .height(22.0)
            .background(badge_bg)
            .corner_radius(CornerRadius::uniform(5.0))
            .vertical_alignment(VerticalAlignment::Center)
            .grid_column(1)
            .content(
                TextBlock::new()
                    .text(asset.protocol.label())
                    .font_size(11.0)
                    .font_weight(FontWeight::BOLD)
                    .foreground(Color::rgb(255, 255, 255))
                    .horizontal_alignment(HorizontalAlignment::Center)
                    .vertical_alignment(VerticalAlignment::Center),
            );

        let name_block = RichTextBlock::new()
            .font_size(14.5)
            .paragraphs(RichText::single_paragraph(highlight_runs(
                &asset.name,
                &match_res.name_indices,
            )));

        let latency_view: View = if let OnlineStatus::Online { latency_ms } = status {
            TextBlock::new()
                .text(format!("{latency_ms} ms"))
                .font_size(11.5)
                .foreground(self.online_color())
                .vertical_alignment(VerticalAlignment::Center)
                .into()
        } else {
            View::empty()
        };

        let category_view: View = if !asset.category.trim().is_empty() {
            let chip_bg = if self.is_dark() {
                Color::rgb(38, 49, 71)
            } else {
                Color::rgb(226, 232, 240)
            };
            Border::new()
                .background(chip_bg)
                .corner_radius(CornerRadius::uniform(4.0))
                .padding(Thickness::xy(7.0, 2.0))
                .vertical_alignment(VerticalAlignment::Center)
                .content(
                    TextBlock::new()
                        .text(&asset.category)
                        .font_size(11.0)
                        .foreground(self.muted_color()),
                )
        } else {
            View::empty()
        };

        let title_line = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .children((name_block, category_view, latency_view));

        let addr = asset.display_address();
        let subtitle = if asset.notes.trim().is_empty() {
            addr
        } else {
            format!("{addr}  ·  {}", asset.notes)
        };
        let subtitle_line = TextBlock::new()
            .text(subtitle)
            .font_size(12.0)
            .foreground(self.muted_color())
            .text_trimming(TextTrimming::CharacterEllipsis);

        let info_col = StackPanel::new()
            .spacing(2.0)
            .vertical_alignment(VerticalAlignment::Center)
            .grid_column(2)
            .children((title_line, subtitle_line));

        let is_confirm_del = self.pending_delete == Some(asset_idx);
        let del_label: View = if is_confirm_del {
            TextBlock::new()
                .text("Confirm?")
                .foreground(self.offline_color())
                .into()
        } else {
            TextBlock::new().text("Del").into()
        };

        let buttons = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(6.0)
            .vertical_alignment(VerticalAlignment::Center)
            .grid_column(3)
            .children((
                Button::new()
                    .style(ButtonStyle::Accent)
                    .on_click(context.message(Msg::LaunchAsset(asset_idx)))
                    .content("Launch"),
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .on_click(context.message(Msg::CopyHost(asset.host.clone())))
                    .content("Copy")
                    .tooltip("Copy host/IP"),
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .on_click(context.message(Msg::OpenEdit(asset_idx)))
                    .content("Edit"),
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .on_click(context.message(Msg::DeleteAsset(asset_idx)))
                    .content(del_label),
            ));

        let row_grid = Grid::new()
            .columns([
                GridLength::Auto,
                GridLength::Auto,
                GridLength::STAR,
                GridLength::Auto,
            ])
            .column_spacing(10.0)
            .children((status_dot, proto_badge, info_col, buttons));

        let border_brush = if is_selected {
            ThemeBrush::Accent
        } else {
            ThemeBrush::CardStroke
        };

        Border::new()
            .margin(Thickness::xy(0.0, 3.0))
            .padding(Thickness::xy(14.0, 10.0))
            .background(ThemeBrush::CardBackground)
            .border_brush(border_brush)
            .border_thickness(Thickness::uniform(if is_selected { 1.5 } else { 1.0 }))
            .corner_radius(CornerRadius::uniform(8.0))
            .content(row_grid)
    }

    fn build_footer(&self) -> View {
        let (notice_text, notice_severity) = if self.drag_active {
            (
                "Drop .rdp files or folder here to import",
                InfoBarSeverity::Informational,
            )
        } else if let Some((msg, sev)) = &self.toast {
            (msg.as_str(), *sev)
        } else {
            ("", InfoBarSeverity::Informational)
        };

        let notice_block = TextBlock::new()
            .text(notice_text)
            .font_size(11.5)
            .font_weight(FontWeight::SEMI_BOLD)
            .foreground(self.notice_color(notice_severity))
            .horizontal_alignment(HorizontalAlignment::Center)
            .vertical_alignment(VerticalAlignment::Center)
            .max_width(340.0)
            .text_trimming(TextTrimming::CharacterEllipsis)
            .grid_column(1);
        let notice_view: View = if notice_text.is_empty() {
            notice_block.into()
        } else {
            notice_block.tooltip(notice_text)
        };

        Grid::new()
            .columns([GridLength::STAR, GridLength::Auto, GridLength::STAR])
            .column_spacing(12.0)
            .grid_row(2)
            .children((
                TextBlock::new()
                    .text(format!("Config: {}", self.config_path.display()))
                    .font_size(11.5)
                    .foreground(self.muted_color())
                    .text_trimming(TextTrimming::CharacterEllipsis)
                    .vertical_alignment(VerticalAlignment::Center)
                    .grid_column(0),
                notice_view,
                TextBlock::new()
                    .text("Enter Launch · Ctrl+R Refresh · Drop .rdp to import")
                    .font_size(11.5)
                    .foreground(self.muted_color())
                    .horizontal_alignment(HorizontalAlignment::Right)
                    .vertical_alignment(VerticalAlignment::Center)
                    .text_trimming(TextTrimming::CharacterEllipsis)
                    .grid_column(2),
            ))
    }

    fn build_dialog(&self, context: &ViewContext<Self>) -> View {
        match &self.dialog {
            ActiveDialog::None => ContentDialog::new().is_open(false).into(),
            ActiveDialog::Editor(form) => {
                let title = if form.edit_index.is_some() {
                    "Edit IT Asset"
                } else {
                    "Add IT Asset"
                };
                let validation = form.to_asset();
                let is_valid = validation.is_ok();

                let proto_btn = |proto: Protocol| {
                    Button::new()
                        .style(if form.protocol == proto {
                            ButtonStyle::Accent
                        } else {
                            ButtonStyle::Default
                        })
                        .on_click(context.message(Msg::EditProtocol(proto)))
                        .content(proto.label())
                };

                let proto_row = StackPanel::new()
                    .spacing(3.0)
                    .grid_column(1)
                    .children((
                        TextBlock::new().text("Protocol *").font_size(12.5),
                        StackPanel::new()
                            .orientation(Orientation::Horizontal)
                            .spacing(6.0)
                            .keyed_children(
                                Protocol::ALL
                                    .into_iter()
                                    .map(|proto| (proto.label(), proto_btn(proto))),
                            ),
                    ));

                let rdp_file_row: View = if form.protocol == Protocol::Rdp {
                    labeled_input(
                        ".rdp File (optional)",
                        r"C:\path\to\server.rdp",
                        &form.rdp_file,
                        context.callback(Msg::EditRdpFile),
                    )
                } else {
                    View::empty()
                };

                let status_line: View = match validation {
                    Ok(_) => View::empty(),
                    Err(_)
                        if form.name.trim().is_empty() && form.host.trim().is_empty() =>
                    {
                        TextBlock::new()
                            .text("* Name and Host are required")
                            .font_size(12.0)
                            .foreground(self.muted_color())
                            .into()
                    }
                    Err(err) => TextBlock::new()
                        .text(err)
                        .font_size(12.0)
                        .foreground(ThemeBrush::SystemCritical)
                        .text_wrapping(TextWrapping::Wrap)
                        .into(),
                };

                let name_proto_row = Grid::new()
                    .columns([GridLength::STAR, GridLength::Auto])
                    .column_spacing(12.0)
                    .children((
                        labeled_input_col(
                            "Name *",
                            "e.g. DC01 - AD Server",
                            &form.name,
                            context.callback(Msg::EditName),
                            Some(0),
                        ),
                        proto_row,
                    ));

                let host_port_row = Grid::new()
                    .columns([GridLength::STAR, GridLength::Pixel(140.0)])
                    .column_spacing(12.0)
                    .children((
                        labeled_input_col(
                            "Host / IP / URL *",
                            "192.168.1.10 or https://192.168.1.5",
                            &form.host,
                            context.callback(Msg::EditHost),
                            Some(0),
                        ),
                        labeled_input_col(
                            "Port (optional)",
                            &format!("Default: {}", form.protocol.default_port()),
                            &form.port,
                            context.callback(Msg::EditPort),
                            Some(1),
                        ),
                    ));

                let user_cat_row = Grid::new()
                    .columns([GridLength::STAR, GridLength::STAR])
                    .column_spacing(12.0)
                    .children((
                        labeled_input_col(
                            "Username (optional)",
                            "Optional (RDP / SSH)",
                            &form.username,
                            context.callback(Msg::EditUsername),
                            Some(0),
                        ),
                        labeled_input_col(
                            "Category",
                            "Server / NAS / Printer",
                            &form.category,
                            context.callback(Msg::EditCategory),
                            Some(1),
                        ),
                    ));

                let form_stack = StackPanel::new().width(448.0).spacing(8.0).children((
                    name_proto_row,
                    host_port_row,
                    user_cat_row,
                    labeled_input(
                        "Notes",
                        "Optional remarks or location",
                        &form.notes,
                        context.callback(Msg::EditNotes),
                    ),
                    rdp_file_row,
                    status_line,
                ));

                ContentDialog::new()
                    .is_open(true)
                    .title(title)
                    .primary_button_text("Save")
                    .close_button_text("Cancel")
                    .is_primary_button_enabled(is_valid)
                    .on_closed(context.callback(Msg::DialogClosed))
                    .content(form_stack)
            }
            ActiveDialog::Import(form) => {
                let ready = !form.folder_path.trim().is_empty();
                let hint_line = TextBlock::new()
                    .text("Enter a .rdp file or directory path, or drag & drop directly onto the main window.")
                    .font_size(12.0)
                    .foreground(self.muted_color())
                    .text_wrapping(TextWrapping::Wrap);

                ContentDialog::new()
                    .is_open(true)
                    .title("Import .rdp Files or Folder")
                    .primary_button_text("Import")
                    .close_button_text("Cancel")
                    .is_primary_button_enabled(ready)
                    .on_closed(context.callback(Msg::DialogClosed))
                    .content(StackPanel::new().width(420.0).spacing(10.0).children((
                        labeled_input(
                            "Path *",
                            r"C:\Users\IT\Desktop\rdp",
                            &form.folder_path,
                            context.callback(Msg::ImportPathChanged),
                        ),
                        hint_line,
                    )))
            }
            ActiveDialog::Settings => {
                let muted = self.muted_color();
                let body = StackPanel::new().width(440.0).spacing(12.0).children((
                    setting_row(
                        "Minimize to tray icon",
                        "Hide window to the system tray instead of keeping it on the taskbar when minimized.",
                        self.settings.minimize_to_tray,
                        muted,
                        context.callback(Msg::SetMinimizeToTray),
                    ),
                    setting_row(
                        "Close to tray icon",
                        "Clicking the window X button hides the app to tray (use tray menu -> Exit to quit).",
                        self.settings.close_to_tray,
                        muted,
                        context.callback(Msg::SetCloseToTray),
                    ),
                    setting_row(
                        "Start with Windows",
                        "Launch automatically when you sign in to Windows (HKCU Run).",
                        self.settings.start_with_windows,
                        muted,
                        context.callback(Msg::SetStartWithWindows),
                    ),
                    setting_row(
                        "Start minimized to tray",
                        "Start hidden in the system tray at launch and Windows startup.",
                        self.settings.start_minimized,
                        muted,
                        context.callback(Msg::SetStartMinimized),
                    ),
                ));

                ContentDialog::new()
                    .is_open(true)
                    .title("Settings")
                    .primary_button_text("Hide to Tray")
                    .secondary_button_text("Exit App")
                    .close_button_text("Close")
                    .on_closed(context.callback(Msg::DialogClosed))
                    .content(body)
            }
        }
    }
}

fn setting_row(
    title: &str,
    subtitle: &str,
    is_on: bool,
    muted: Color,
    on_toggled: Callback<bool>,
) -> View {
    Grid::new()
        .columns([GridLength::STAR, GridLength::Auto])
        .column_spacing(16.0)
        .children((
            StackPanel::new()
                .spacing(2.0)
                .vertical_alignment(VerticalAlignment::Center)
                .grid_column(0)
                .children((
                    TextBlock::new().text(title).font_size(14.0),
                    TextBlock::new()
                        .text(subtitle)
                        .font_size(11.5)
                        .foreground(muted)
                        .text_wrapping(TextWrapping::Wrap),
                )),
            ToggleSwitch::new()
                .is_on(is_on)
                .on_toggled(on_toggled)
                .vertical_alignment(VerticalAlignment::Center)
                .grid_column(1),
        ))
}

fn labeled_input(
    label: &str,
    placeholder: &str,
    value: &str,
    on_changed: Callback<String>,
) -> View {
    labeled_input_col(label, placeholder, value, on_changed, None)
}

fn labeled_input_col(
    label: &str,
    placeholder: &str,
    value: &str,
    on_changed: Callback<String>,
    grid_col: Option<i32>,
) -> View {
    let mut panel = StackPanel::new().spacing(3.0);
    if let Some(col) = grid_col {
        panel = panel.grid_column(col);
    }
    panel.children((
        TextBlock::new().text(label).font_size(12.5),
        TextBox::new()
            .placeholder_text(placeholder)
            .text(value)
            .on_text_changed(on_changed),
    ))
}

fn highlight_runs(name: &str, matched_indices: &[usize]) -> Vec<RichTextInline> {
    if matched_indices.is_empty() {
        return vec![RichTextInline::Run(RichTextRun::plain(name))];
    }

    let mut runs = Vec::new();
    let mut buf = String::new();
    let mut current_bold: Option<bool> = None;

    for (idx, ch) in name.chars().enumerate() {
        let is_bold = matched_indices.binary_search(&idx).is_ok();
        if let Some(prev) = current_bold
            && prev != is_bold
            && !buf.is_empty()
        {
            runs.push(RichTextInline::Run(RichTextRun {
                text: std::mem::take(&mut buf),
                is_bold: prev,
                is_italic: false,
            }));
        }
        current_bold = Some(is_bold);
        buf.push(ch);
    }

    if let Some(bold) = current_bold
        && !buf.is_empty()
    {
        runs.push(RichTextInline::Run(RichTextRun {
            text: buf,
            is_bold: bold,
            is_italic: false,
        }));
    }

    if runs.is_empty() {
        runs.push(RichTextInline::Run(RichTextRun::plain(name)));
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_form_validates_and_strips_rdp_file_when_non_rdp() {
        let mut form = AssetForm::new_blank();
        form.name = "  Linux Node  ".into();
        form.host = "192.168.1.20".into();
        form.protocol = Protocol::Ssh;
        form.port = "2222".into();
        form.rdp_file = r"C:\ignored.rdp".into();

        let asset = form.to_asset().unwrap();
        assert_eq!(asset.name, "Linux Node");
        assert_eq!(asset.port, Some(2222));
        assert_eq!(asset.rdp_file, None);

        form.port = "0".into();
        assert!(form.to_asset().is_err());

        form.port = "abc".into();
        assert!(form.to_asset().is_err());
    }

    #[test]
    fn highlight_runs_splits_bold_and_plain_segments() {
        let plain = highlight_runs("DC01", &[]);
        assert_eq!(plain.len(), 1);
        assert!(matches!(
            &plain[0],
            RichTextInline::Run(r) if r.text == "DC01" && !r.is_bold
        ));

        let mixed = highlight_runs("NAS01", &[0, 2]);
        assert_eq!(mixed.len(), 4);
        assert!(matches!(&mixed[0], RichTextInline::Run(r) if r.text == "N" && r.is_bold));
        assert!(matches!(&mixed[1], RichTextInline::Run(r) if r.text == "A" && !r.is_bold));
        assert!(matches!(&mixed[2], RichTextInline::Run(r) if r.text == "S" && r.is_bold));
        assert!(matches!(&mixed[3], RichTextInline::Run(r) if r.text == "01" && !r.is_bold));
    }
}
