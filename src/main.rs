use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone, Utc};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::env;
use std::ffi::{CString, c_char, c_int, c_void};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr;
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;
use walkdir::WalkDir;

const REFRESH_SECONDS: u32 = 30;
const PRIMARY_WINDOW_SECONDS: i64 = 5 * 60 * 60;
const PACE_ALERT_AHEAD_PERCENT: f64 = 10.0;
const PACE_ALERT_CLEAR_PERCENT: f64 = 5.0;

#[derive(Clone, Copy)]
struct Price {
    input: f64,
    cached_input: f64,
    output: f64,
}

#[derive(Clone, Default)]
struct Usage {
    input_tokens: i64,
    cached_input_tokens: i64,
    output_tokens: i64,
    reasoning_output_tokens: i64,
    total_tokens: i64,
}

impl Usage {
    fn add_json(&mut self, value: &Value) {
        self.input_tokens += value_i64(value, "input_tokens");
        self.cached_input_tokens += value_i64(value, "cached_input_tokens");
        self.output_tokens += value_i64(value, "output_tokens");
        self.reasoning_output_tokens += value_i64(value, "reasoning_output_tokens");
        self.total_tokens += value_i64(value, "total_tokens");
    }

    fn merge(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.cached_input_tokens += other.cached_input_tokens;
        self.output_tokens += other.output_tokens;
        self.reasoning_output_tokens += other.reasoning_output_tokens;
        self.total_tokens += other.total_tokens;
    }
}

#[derive(Clone, Default)]
struct WindowLimit {
    used_percent: f64,
    resets_at: Option<i64>,
}

#[derive(Clone, Default)]
struct RateLimits {
    plan_type: String,
    primary: WindowLimit,
    secondary: WindowLimit,
}

#[derive(Clone, Default)]
struct Stats {
    total: Usage,
    today: Usage,
    month: Usage,
    by_model: HashMap<String, Usage>,
    today_by_model: HashMap<String, Usage>,
    month_by_model: HashMap<String, Usage>,
    rate_limits: Option<RateLimits>,
    latest_rate_ts: Option<DateTime<Local>>,
    files_seen: usize,
    events_seen: usize,
}

#[derive(Clone, Default)]
struct FileStats {
    total: Usage,
    by_model: HashMap<String, Usage>,
    by_day: HashMap<NaiveDate, Usage>,
    by_day_model: HashMap<NaiveDate, HashMap<String, Usage>>,
    by_month: HashMap<(i32, u32), Usage>,
    by_month_model: HashMap<(i32, u32), HashMap<String, Usage>>,
    rate_limits: Option<RateLimits>,
    latest_rate_ts: Option<DateTime<Local>>,
    events_seen: usize,
}

struct CachedFile {
    len: u64,
    modified_ns: u128,
    stats: FileStats,
}

#[derive(Default)]
struct StatsCache {
    files: HashMap<PathBuf, CachedFile>,
    last_stats: Option<Stats>,
    last_day: Option<NaiveDate>,
    last_month: Option<(i32, u32)>,
}

#[repr(C)]
struct GtkWidget(c_void);
#[repr(C)]
struct GtkMenu(c_void);
#[repr(C)]
struct AppIndicator(c_void);
#[repr(C)]
struct GtkCssProvider(c_void);
#[repr(C)]
struct GdkScreen(c_void);
#[repr(C)]
struct GdkVisual(c_void);
#[repr(C)]
struct Cairo(c_void);

#[link(name = "gtk-3")]
unsafe extern "C" {
    fn gtk_init(argc: *mut c_int, argv: *mut *mut *mut c_char);
    fn gtk_main();
    fn gtk_main_quit();
    fn gtk_menu_new() -> *mut GtkWidget;
    fn gtk_menu_item_new() -> *mut GtkWidget;
    fn gtk_menu_item_new_with_label(label: *const c_char) -> *mut GtkWidget;
    fn gtk_separator_menu_item_new() -> *mut GtkWidget;
    fn gtk_menu_shell_append(menu_shell: *mut GtkWidget, child: *mut GtkWidget);
    fn gtk_widget_show_all(widget: *mut GtkWidget);
    fn gtk_widget_set_sensitive(widget: *mut GtkWidget, sensitive: c_int);
    fn gtk_widget_destroy(widget: *mut GtkWidget);
    fn gtk_widget_queue_draw(widget: *mut GtkWidget);
    fn gtk_widget_set_size_request(widget: *mut GtkWidget, width: c_int, height: c_int);
    fn gtk_widget_set_app_paintable(widget: *mut GtkWidget, app_paintable: c_int);
    fn gtk_widget_set_opacity(widget: *mut GtkWidget, opacity: f64);
    fn gtk_widget_set_halign(widget: *mut GtkWidget, align: c_int);
    fn gtk_widget_set_valign(widget: *mut GtkWidget, align: c_int);
    fn gtk_widget_get_screen(widget: *mut GtkWidget) -> *mut GdkScreen;
    fn gtk_widget_set_visual(widget: *mut GtkWidget, visual: *mut GdkVisual);
    fn gtk_widget_get_allocated_width(widget: *mut GtkWidget) -> c_int;
    fn gtk_widget_get_allocated_height(widget: *mut GtkWidget) -> c_int;
    fn gtk_label_new(str: *const c_char) -> *mut GtkWidget;
    fn gtk_label_set_markup(label: *mut GtkWidget, str: *const c_char);
    fn gtk_label_set_xalign(label: *mut GtkWidget, xalign: f32);
    fn gtk_container_add(container: *mut GtkWidget, widget: *mut GtkWidget);
    fn gtk_overlay_new() -> *mut GtkWidget;
    fn gtk_overlay_add_overlay(overlay: *mut GtkWidget, widget: *mut GtkWidget);
    fn gtk_drawing_area_new() -> *mut GtkWidget;
    fn gtk_window_new(window_type: c_int) -> *mut GtkWidget;
    fn gtk_window_set_title(window: *mut GtkWidget, title: *const c_char);
    fn gtk_window_set_default_size(window: *mut GtkWidget, width: c_int, height: c_int);
    fn gtk_window_set_decorated(window: *mut GtkWidget, setting: c_int);
    fn gtk_window_set_keep_above(window: *mut GtkWidget, setting: c_int);
    fn gtk_css_provider_new() -> *mut GtkCssProvider;
    fn gtk_css_provider_load_from_data(
        css_provider: *mut GtkCssProvider,
        data: *const c_char,
        length: isize,
        error: *mut *mut c_void,
    ) -> c_int;
    fn gtk_style_context_add_provider_for_screen(
        screen: *mut GdkScreen,
        provider: *mut GtkCssProvider,
        priority: u32,
    );
}

#[link(name = "gdk-3")]
unsafe extern "C" {
    fn gdk_screen_get_rgba_visual(screen: *mut GdkScreen) -> *mut GdkVisual;
}

#[link(name = "gobject-2.0")]
unsafe extern "C" {
    fn g_signal_connect_data(
        instance: *mut c_void,
        detailed_signal: *const c_char,
        c_handler: *mut c_void,
        data: *mut c_void,
        destroy_data: *mut c_void,
        connect_flags: c_int,
    ) -> u64;
}

#[link(name = "glib-2.0")]
unsafe extern "C" {
    fn g_timeout_add_seconds(
        interval: u32,
        function: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
        data: *mut c_void,
    ) -> u32;
    fn g_timeout_add(
        interval: u32,
        function: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
        data: *mut c_void,
    ) -> u32;
}

#[link(name = "ayatana-appindicator3")]
unsafe extern "C" {
    fn app_indicator_new(
        id: *const c_char,
        icon_name: *const c_char,
        category: c_int,
    ) -> *mut AppIndicator;
    fn app_indicator_set_status(self_: *mut AppIndicator, status: c_int);
    fn app_indicator_set_menu(self_: *mut AppIndicator, menu: *mut GtkMenu);
    fn app_indicator_set_label(
        self_: *mut AppIndicator,
        label: *const c_char,
        guide: *const c_char,
    );
    fn app_indicator_set_icon_full(
        self_: *mut AppIndicator,
        icon_name: *const c_char,
        icon_desc: *const c_char,
    );
    fn app_indicator_set_icon_theme_path(self_: *mut AppIndicator, icon_theme_path: *const c_char);
    fn app_indicator_set_title(self_: *mut AppIndicator, title: *const c_char);
}

#[link(name = "gtk-layer-shell")]
unsafe extern "C" {
    fn gtk_layer_init_for_window(window: *mut GtkWidget);
    fn gtk_layer_set_namespace(window: *mut GtkWidget, name_space: *const c_char);
    fn gtk_layer_set_layer(window: *mut GtkWidget, layer: c_int);
    fn gtk_layer_set_anchor(window: *mut GtkWidget, edge: c_int, anchor_to_edge: c_int);
    fn gtk_layer_set_margin(window: *mut GtkWidget, edge: c_int, margin_size: c_int);
    fn gtk_layer_set_exclusive_zone(window: *mut GtkWidget, exclusive_zone: c_int);
    fn gtk_layer_set_keyboard_mode(window: *mut GtkWidget, mode: c_int);
}

#[link(name = "cairo")]
unsafe extern "C" {
    fn cairo_save(cr: *mut Cairo);
    fn cairo_restore(cr: *mut Cairo);
    fn cairo_set_source_rgba(cr: *mut Cairo, r: f64, g: f64, b: f64, a: f64);
    fn cairo_set_operator(cr: *mut Cairo, op: c_int);
    fn cairo_paint(cr: *mut Cairo);
    fn cairo_rectangle(cr: *mut Cairo, x: f64, y: f64, width: f64, height: f64);
    fn cairo_arc(cr: *mut Cairo, xc: f64, yc: f64, radius: f64, angle1: f64, angle2: f64);
    fn cairo_fill(cr: *mut Cairo);
    fn cairo_translate(cr: *mut Cairo, tx: f64, ty: f64);
    fn cairo_rotate(cr: *mut Cairo, angle: f64);
}

struct AppState {
    indicator: *mut AppIndicator,
    limit_label: *mut GtkWidget,
    weekly_label: *mut GtkWidget,
    pace_label: *mut GtkWidget,
    pace_expected_label: *mut GtkWidget,
    cost_today_label: *mut GtkWidget,
    cost_month_label: *mut GtkWidget,
    cost_total_label: *mut GtkWidget,
    tokens_today_label: *mut GtkWidget,
    tokens_total_label: *mut GtkWidget,
    party_mode_label: *mut GtkWidget,
    last_render: Option<RenderSnapshot>,
    seen_primary_window: bool,
    last_primary_resets_at: Option<i64>,
    pace_alert_active: bool,
    pace_alert_window: Option<i64>,
    seen_secondary_window: bool,
    last_secondary_resets_at: Option<i64>,
}

#[derive(Clone, PartialEq)]
struct RenderSnapshot {
    limit_markup: String,
    weekly_markup: String,
    pace_markup: String,
    pace_expected_markup: String,
    cost_today_markup: String,
    cost_month_markup: String,
    cost_total_markup: String,
    tokens_today_markup: String,
    tokens_total_markup: String,
    party_mode_markup: String,
    tray_label: String,
    title: String,
    icon_name: String,
}

unsafe impl Send for AppState {}

static STATE: OnceLock<Mutex<AppState>> = OnceLock::new();
static STATS_CACHE: OnceLock<Mutex<StatsCache>> = OnceLock::new();

fn price(model: &str) -> Option<Price> {
    match normalize_model(model).as_str() {
        "gpt-5.5" => Some(Price {
            input: 5.00,
            cached_input: 0.50,
            output: 30.00,
        }),
        "gpt-5.4" => Some(Price {
            input: 2.50,
            cached_input: 0.25,
            output: 15.00,
        }),
        "gpt-5.4-mini" => Some(Price {
            input: 0.75,
            cached_input: 0.075,
            output: 4.50,
        }),
        "gpt-5.4-nano" => Some(Price {
            input: 0.20,
            cached_input: 0.02,
            output: 1.25,
        }),
        "gpt-5.2" | "gpt-5.2-codex" => Some(Price {
            input: 1.75,
            cached_input: 0.175,
            output: 14.00,
        }),
        "gpt-5.1" | "gpt-5.1-codex" | "gpt-5.1-codex-max" | "gpt-5" | "gpt-5-codex" => {
            Some(Price {
                input: 1.25,
                cached_input: 0.125,
                output: 10.00,
            })
        }
        "gpt-5-mini" => Some(Price {
            input: 0.25,
            cached_input: 0.025,
            output: 2.00,
        }),
        "gpt-5-nano" => Some(Price {
            input: 0.05,
            cached_input: 0.005,
            output: 0.40,
        }),
        _ => None,
    }
}

fn normalize_model(model: &str) -> String {
    if model.starts_with("gpt-5.5") {
        "gpt-5.5".into()
    } else if model.starts_with("gpt-5.4-mini") {
        "gpt-5.4-mini".into()
    } else if model.starts_with("gpt-5.4-nano") {
        "gpt-5.4-nano".into()
    } else if model.starts_with("gpt-5.4") {
        "gpt-5.4".into()
    } else if model.starts_with("gpt-5.2-codex") {
        "gpt-5.2-codex".into()
    } else if model.starts_with("gpt-5.2") {
        "gpt-5.2".into()
    } else if model.starts_with("gpt-5.1-codex-max") {
        "gpt-5.1-codex-max".into()
    } else if model.starts_with("gpt-5.1-codex") {
        "gpt-5.1-codex".into()
    } else if model.starts_with("gpt-5.1") {
        "gpt-5.1".into()
    } else if model.starts_with("gpt-5-mini") {
        "gpt-5-mini".into()
    } else if model.starts_with("gpt-5-nano") {
        "gpt-5-nano".into()
    } else if model.starts_with("gpt-5-codex") {
        "gpt-5-codex".into()
    } else if model.starts_with("gpt-5") {
        "gpt-5".into()
    } else if model.is_empty() {
        "unknown".into()
    } else {
        model.into()
    }
}

fn value_i64(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn cost_for_usage(model: &str, usage: &Usage) -> Option<f64> {
    let p = price(model)?;
    let cached = usage.cached_input_tokens.max(0) as f64;
    let uncached = (usage.input_tokens - usage.cached_input_tokens).max(0) as f64;
    Some(
        (uncached * p.input + cached * p.cached_input + usage.output_tokens as f64 * p.output)
            / 1_000_000.0,
    )
}

fn sum_cost(models: &HashMap<String, Usage>) -> Option<f64> {
    let mut total = 0.0;
    let mut any = false;
    for (model, usage) in models {
        if let Some(cost) = cost_for_usage(model, usage) {
            total += cost;
            any = true;
        }
    }
    any.then_some(total)
}

fn unpriced_tokens(stats: &Stats) -> i64 {
    stats
        .by_model
        .iter()
        .filter(|(model, _)| price(model).is_none())
        .map(|(_, usage)| usage.total_tokens)
        .sum()
}

fn collect_stats() -> Stats {
    let cache = STATS_CACHE.get_or_init(|| Mutex::new(StatsCache::default()));
    let mut cache = cache.lock().unwrap();
    collect_stats_cached(&mut cache)
}

fn collect_stats_cached(cache: &mut StatsCache) -> Stats {
    let now = Local::now();
    let today = now.date_naive();
    let month = (now.year(), now.month());
    let mut changed = cache.last_day != Some(today) || cache.last_month != Some(month);
    let mut seen = HashSet::new();
    let codex_home = codex_home();
    for root in [
        codex_home.join("sessions"),
        codex_home.join("archived_sessions"),
    ] {
        if !root.exists() {
            continue;
        }
        for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
            if !entry.file_type().is_file()
                || entry.path().extension().and_then(|s| s.to_str()) != Some("jsonl")
            {
                continue;
            }
            let path = entry.path().to_path_buf();
            seen.insert(path.clone());
            let Some((len, modified_ns)) = file_key(&path) else {
                continue;
            };
            let stale = cache
                .files
                .get(&path)
                .is_none_or(|cached| cached.len != len || cached.modified_ns != modified_ns);
            if stale {
                changed = true;
                cache.files.insert(
                    path,
                    CachedFile {
                        len,
                        modified_ns,
                        stats: parse_file_stats(entry.path()),
                    },
                );
            }
        }
    }
    let before_retain = cache.files.len();
    cache.files.retain(|path, _| seen.contains(path));
    if cache.files.len() != before_retain {
        changed = true;
    }
    if !changed && let Some(stats) = cache.last_stats.clone() {
        return stats;
    }
    let stats = aggregate_cached_stats(cache, today, month);
    cache.last_day = Some(today);
    cache.last_month = Some(month);
    cache.last_stats = Some(stats.clone());
    stats
}

fn codex_home() -> PathBuf {
    if let Some(path) = env::var_os("CODEX_HOME") {
        return PathBuf::from(path);
    }
    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home).join(".codex");
    }
    PathBuf::from(".codex")
}

fn details_html_path() -> PathBuf {
    env::temp_dir().join("codex-usage-tray-details.html")
}

fn icon_dir() -> PathBuf {
    env::temp_dir().join("codex-usage-tray-icons")
}

fn config_path() -> PathBuf {
    if let Some(config_home) = env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(config_home).join("codex-usage-tray/config.json");
    }
    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home).join(".config/codex-usage-tray/config.json");
    }
    PathBuf::from("codex-usage-tray-config.json")
}

fn party_mode_enabled() -> bool {
    let Ok(content) = fs::read_to_string(config_path()) else {
        return true;
    };
    serde_json::from_str::<Value>(&content)
        .ok()
        .and_then(|value| value.get("party_mode").and_then(Value::as_bool))
        .unwrap_or(true)
}

fn set_party_mode(enabled: bool) {
    let path = config_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let body = format!(
        "{{\n  \"party_mode\": {}\n}}\n",
        if enabled { "true" } else { "false" }
    );
    let _ = fs::write(path, body);
}

fn party_mode_markup() -> String {
    if party_mode_enabled() {
        "🎉  Party mode:  <b>On</b>".into()
    } else {
        "🎉  Party mode:  <b>Off</b>".into()
    }
}

fn file_key(path: &Path) -> Option<(u64, u128)> {
    let metadata = fs::metadata(path).ok()?;
    let modified_ns = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((metadata.len(), modified_ns))
}

fn aggregate_cached_stats(cache: &StatsCache, today: NaiveDate, month: (i32, u32)) -> Stats {
    let mut stats = Stats::default();
    for cached in cache.files.values() {
        let file = &cached.stats;
        stats.files_seen += 1;
        stats.events_seen += file.events_seen;
        stats.total.merge(&file.total);
        merge_usage_map(&mut stats.by_model, &file.by_model);
        if let Some(day) = file.by_day.get(&today) {
            stats.today.merge(day);
        }
        if let Some(day_models) = file.by_day_model.get(&today) {
            merge_usage_map(&mut stats.today_by_model, day_models);
        }
        if let Some(month_usage) = file.by_month.get(&month) {
            stats.month.merge(month_usage);
        }
        if let Some(month_models) = file.by_month_model.get(&month) {
            merge_usage_map(&mut stats.month_by_model, month_models);
        }
        if let (Some(ts), Some(rate_limits)) = (file.latest_rate_ts, file.rate_limits.clone())
            && stats.latest_rate_ts.is_none_or(|latest| ts > latest)
        {
            stats.latest_rate_ts = Some(ts);
            stats.rate_limits = Some(rate_limits);
        }
    }
    stats
}

fn merge_usage_map(target: &mut HashMap<String, Usage>, source: &HashMap<String, Usage>) {
    for (model, usage) in source {
        target.entry(model.clone()).or_default().merge(usage);
    }
}

fn parse_file_stats(path: &Path) -> FileStats {
    let mut stats = FileStats::default();
    let Ok(content) = fs::read_to_string(path) else {
        return stats;
    };
    let mut model = "unknown".to_string();
    for line in content.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(obj) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let typ = obj.get("type").and_then(Value::as_str).unwrap_or("");
        let payload = obj.get("payload").unwrap_or(&Value::Null);
        if typ == "session_meta" || typ == "turn_context" {
            if let Some(new_model) = payload.get("model").and_then(Value::as_str) {
                model = normalize_model(new_model);
            }
            continue;
        }
        if typ != "event_msg" || payload.get("type").and_then(Value::as_str) != Some("token_count")
        {
            continue;
        }
        let Some(usage_json) = payload.pointer("/info/last_token_usage") else {
            continue;
        };
        let ts = obj
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_timestamp)
            .unwrap_or_else(Local::now);
        let mut usage = Usage::default();
        usage.add_json(usage_json);
        stats.events_seen += 1;
        stats.total.merge(&usage);
        stats
            .by_model
            .entry(model.clone())
            .or_default()
            .merge(&usage);
        let day = ts.date_naive();
        stats.by_day.entry(day).or_default().merge(&usage);
        stats
            .by_day_model
            .entry(day)
            .or_default()
            .entry(model.clone())
            .or_default()
            .merge(&usage);
        let month = (ts.year(), ts.month());
        stats.by_month.entry(month).or_default().merge(&usage);
        stats
            .by_month_model
            .entry(month)
            .or_default()
            .entry(model.clone())
            .or_default()
            .merge(&usage);
        if let Some(rate_limits) = parse_rate_limits(payload)
            && stats.latest_rate_ts.is_none_or(|latest| ts > latest)
        {
            stats.latest_rate_ts = Some(ts);
            stats.rate_limits = Some(rate_limits);
        }
    }
    stats
}

fn parse_timestamp(raw: &str) -> Option<DateTime<Local>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Local))
}

fn parse_rate_limits(payload: &Value) -> Option<RateLimits> {
    let raw = payload.get("rate_limits")?;
    Some(RateLimits {
        plan_type: raw
            .get("plan_type")
            .and_then(Value::as_str)
            .unwrap_or("n/a")
            .to_string(),
        primary: parse_window(raw.get("primary")),
        secondary: parse_window(raw.get("secondary")),
    })
}

fn parse_window(value: Option<&Value>) -> WindowLimit {
    let Some(value) = value else {
        return WindowLimit::default();
    };
    WindowLimit {
        used_percent: value
            .get("used_percent")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        resets_at: value.get("resets_at").and_then(Value::as_i64),
    }
}

fn dollars(value: Option<f64>) -> String {
    match value {
        None => "n/a".into(),
        Some(v) if v >= 1000.0 => format!("${}", comma_int(v.round() as i64)),
        Some(v) if v >= 100.0 => format!("${}", comma_decimal(v, 1)),
        Some(v) => format!("${v:.2}"),
    }
}

fn full_tokens(value: i64) -> String {
    comma_int(value)
}

fn compact_tokens(value: i64) -> String {
    let abs = value.abs() as f64;
    if abs >= 1_000_000_000.0 {
        format!("{:.2}B", value as f64 / 1_000_000_000.0)
    } else if abs >= 1_000_000.0 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else {
        comma_int(value)
    }
}

fn comma_int(value: i64) -> String {
    let negative = value < 0;
    let digits = value.abs().to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    let mut result: String = out.chars().rev().collect();
    if negative {
        result.insert(0, '-');
    }
    result
}

fn comma_decimal(value: f64, decimals: usize) -> String {
    let rounded = format!("{value:.decimals$}");
    let Some((whole, frac)) = rounded.split_once('.') else {
        return comma_int(value.round() as i64);
    };
    let whole = whole.parse::<i64>().unwrap_or(0);
    format!("{}.{}", comma_int(whole), frac)
}

fn display_plan(plan: &str) -> &str {
    if plan == "prolite" {
        "$100 Pro (Pro Lite)"
    } else {
        plan
    }
}

fn reset_text(resets_at: Option<i64>) -> String {
    let Some(reset) = resets_at else {
        return "n/a".into();
    };
    let Some(reset_dt) = Utc.timestamp_opt(reset, 0).single() else {
        return "n/a".into();
    };
    let seconds = reset_dt.signed_duration_since(Utc::now()).num_seconds();
    if seconds <= 0 {
        "now".into()
    } else {
        let days = seconds / 86_400;
        let hours = (seconds % 86_400) / 3_600;
        let minutes = (seconds % 3_600) / 60;
        if days > 0 {
            format!("{days}d {hours}h")
        } else if hours > 0 {
            format!("{hours}h {minutes}m")
        } else {
            format!("{minutes}m")
        }
    }
}

fn reset_clock_text(resets_at: Option<i64>) -> String {
    let Some(reset) = resets_at else {
        return "n/a".into();
    };
    let Some(reset_dt) = Local.timestamp_opt(reset, 0).single() else {
        return "n/a".into();
    };
    reset_dt.format("%H:%M").to_string()
}

#[derive(Clone, Copy)]
struct Pace {
    expected_percent: f64,
    ahead_percent: f64,
}

fn primary_pace(limit: &WindowLimit) -> Option<Pace> {
    let reset = limit.resets_at?;
    let now = Utc::now().timestamp();
    let seconds_left = (reset - now).clamp(0, PRIMARY_WINDOW_SECONDS);
    let elapsed = PRIMARY_WINDOW_SECONDS - seconds_left;
    let expected_percent = elapsed as f64 * 100.0 / PRIMARY_WINDOW_SECONDS as f64;
    Some(Pace {
        expected_percent,
        ahead_percent: limit.used_percent - expected_percent,
    })
}

fn pace_text(limit: &WindowLimit) -> String {
    let Some(pace) = primary_pace(limit) else {
        return "n/a".into();
    };
    let diff = pace.ahead_percent.abs();
    if pace.ahead_percent >= PACE_ALERT_AHEAD_PERCENT {
        format!("fast by {:.1}%", diff)
    } else if pace.ahead_percent > 1.0 {
        format!("ahead by {:.1}%", diff)
    } else if pace.ahead_percent < -1.0 {
        format!("slow by {:.1}%", diff)
    } else {
        "on pace".into()
    }
}

fn pace_dot(limit: &WindowLimit) -> &'static str {
    let Some(pace) = primary_pace(limit) else {
        return "⚪";
    };
    if pace.ahead_percent >= PACE_ALERT_AHEAD_PERCENT {
        "🔴"
    } else if pace.ahead_percent > 5.0 {
        "🟠"
    } else if pace.ahead_percent > 1.0 {
        "🟡"
    } else {
        "🟢"
    }
}

fn current_month_range_text() -> String {
    let now = Local::now();
    let year = now.year();
    let month = now.month();
    let Some(start) = chrono::NaiveDate::from_ymd_opt(year, month, 1) else {
        return "Current month".into();
    };
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let Some(next_start) = chrono::NaiveDate::from_ymd_opt(next_year, next_month, 1) else {
        return "Current month".into();
    };
    let end = next_start - chrono::Duration::days(1);
    format!("{} - {}", start.format("%b %-d"), end.format("%b %-d, %Y"))
}

fn make_details_text(stats: &Stats) -> String {
    let total_cost = sum_cost(&stats.by_model);
    let today_cost = sum_cost(&stats.today_by_model);
    let month_cost = sum_cost(&stats.month_by_model);
    let rate = stats.rate_limits.clone().unwrap_or_default();
    let mut top: Vec<_> = stats.by_model.iter().collect();
    top.sort_by_key(|(_, usage)| -usage.total_tokens);
    let model_lines = top
        .into_iter()
        .take(8)
        .map(|(model, usage)| {
            format!(
                "{model}: {} tokens | input {} | cached {} | output {} | reasoning {} | cost {}",
                full_tokens(usage.total_tokens),
                full_tokens(usage.input_tokens),
                full_tokens(usage.cached_input_tokens),
                full_tokens(usage.output_tokens),
                full_tokens(usage.reasoning_output_tokens),
                dollars(cost_for_usage(model, usage))
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Codex Usage\n\nPlan: {}\nParty mode: {}\n\nRate limits\n5h: {:.0}% | reset in {} | pace: {} | expected {:.1}%\nWeekly: {:.0}% | reset in {}\n\nCosts\nToday: {} tokens | {}\nThis month: {} tokens | {}\nAll-time: {} tokens | {}\n\nToken breakdown\nAll-time input: {}\nAll-time cached input: {}\nAll-time output: {}\nAll-time reasoning: {}\nSkipped with no public API price: {}\n\nModels\n{}\n\nSource: {} token_count events from {} JSONL files\n",
        display_plan(&rate.plan_type),
        if party_mode_enabled() { "on" } else { "off" },
        rate.primary.used_percent,
        reset_text(rate.primary.resets_at),
        pace_text(&rate.primary),
        primary_pace(&rate.primary)
            .map(|pace| pace.expected_percent)
            .unwrap_or(0.0),
        rate.secondary.used_percent,
        reset_text(rate.secondary.resets_at),
        full_tokens(stats.today.total_tokens),
        dollars(today_cost),
        full_tokens(stats.month.total_tokens),
        dollars(month_cost),
        full_tokens(stats.total.total_tokens),
        dollars(total_cost),
        full_tokens(stats.total.input_tokens),
        full_tokens(stats.total.cached_input_tokens),
        full_tokens(stats.total.output_tokens),
        full_tokens(stats.total.reasoning_output_tokens),
        full_tokens(unpriced_tokens(stats)),
        model_lines,
        stats.events_seen,
        stats.files_seen
    )
}

fn make_details_html(stats: &Stats) -> String {
    let total_cost = sum_cost(&stats.by_model);
    let today_cost = sum_cost(&stats.today_by_model);
    let month_cost = sum_cost(&stats.month_by_model);
    let month_range = current_month_range_text();
    let rate = stats.rate_limits.clone().unwrap_or_default();
    let party_mode = party_mode_enabled();
    let mut top: Vec<_> = stats.by_model.iter().collect();
    top.sort_by_key(|(_, usage)| -usage.total_tokens);
    let model_rows = top
        .into_iter()
        .take(8)
        .map(|(model, usage)| {
            format!(
                "<tr>\
                    <td><span class=\"model-name\">{}</span></td>\
                    <td title=\"{}\">{}</td>\
                    <td title=\"{}\">{}</td>\
                    <td title=\"{}\">{}</td>\
                    <td title=\"{}\">{}</td>\
                    <td title=\"{}\">{}</td>\
                    <td class=\"cost\">{}</td>\
                </tr>",
                html_escape(model),
                full_tokens(usage.total_tokens),
                compact_tokens(usage.total_tokens),
                full_tokens(usage.input_tokens),
                compact_tokens(usage.input_tokens),
                full_tokens(usage.cached_input_tokens),
                compact_tokens(usage.cached_input_tokens),
                full_tokens(usage.output_tokens),
                compact_tokens(usage.output_tokens),
                full_tokens(usage.reasoning_output_tokens),
                compact_tokens(usage.reasoning_output_tokens),
                dollars(cost_for_usage(model, usage))
            )
        })
        .collect::<Vec<_>>()
        .join("");
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Codex Usage</title>
<style>
:root {{
  color-scheme: dark;
  --bg: #090d12;
  --panel: #111820;
  --panel-soft: #0d131a;
  --line: #202b36;
  --line-soft: #18222d;
  --text: #eef4fb;
  --muted: #8ea0b2;
  --quiet: #627284;
  --green: #35c46a;
  --amber: #e5b454;
  --red: #e35d5d;
  --blue: #6aa8ff;
  --violet: #aa7cff;
}}
* {{ box-sizing: border-box; }}
body {{
  margin: 0;
  min-height: 100dvh;
  background:
    radial-gradient(circle at 15% 0%, rgba(106, 168, 255, 0.13), transparent 34rem),
    radial-gradient(circle at 85% 8%, rgba(170, 124, 255, 0.11), transparent 30rem),
    linear-gradient(180deg, #0b1016 0%, var(--bg) 48%, #070a0f 100%);
  color: var(--text);
  font: 14px/1.5 "SF Pro Text", -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
}}
.shell {{
  width: min(1180px, calc(100vw - 32px));
  margin: 0 auto;
  padding: 34px 0 42px;
}}
.topbar {{
  display: flex;
  align-items: end;
  justify-content: space-between;
  gap: 24px;
  margin-bottom: 22px;
}}
h1 {{
  margin: 0;
  font-size: clamp(30px, 3.7vw, 46px);
  line-height: 1.04;
  letter-spacing: 0;
  font-weight: 650;
}}
.subtitle {{
  margin: 10px 0 0;
  max-width: 58ch;
  color: var(--muted);
}}
.plan {{
  border: 1px solid var(--line);
  border-radius: 14px;
  padding: 10px 14px;
  background: rgba(17, 24, 32, 0.72);
  color: #d9e6f2;
  white-space: nowrap;
}}
.grid {{
  display: grid;
  grid-template-columns: repeat(12, 1fr);
  gap: 12px;
}}
.card {{
  border: 1px solid var(--line);
  border-radius: 16px;
  background:
    linear-gradient(180deg, rgba(255, 255, 255, 0.035), transparent),
    rgba(17, 24, 32, 0.84);
  box-shadow: 0 18px 70px rgba(0, 0, 0, 0.22);
  overflow: hidden;
}}
.card.pad {{ padding: 18px; }}
.span-3 {{ grid-column: span 3; }}
.span-4 {{ grid-column: span 4; }}
.span-6 {{ grid-column: span 6; }}
.span-8 {{ grid-column: span 8; }}
.span-12 {{ grid-column: span 12; }}
.label {{
  color: var(--quiet);
  font-size: 12px;
  font-weight: 650;
  margin-bottom: 8px;
}}
.metric {{
  font-size: clamp(23px, 2.8vw, 32px);
  line-height: 1;
  font-weight: 650;
  letter-spacing: 0;
}}
.muted {{ color: var(--muted); }}
.small {{ font-size: 12px; color: var(--quiet); }}
.rate-head {{
  display: flex;
  justify-content: space-between;
  gap: 18px;
  align-items: start;
  margin-bottom: 14px;
}}
.rate-title {{
  display: flex;
  align-items: center;
  gap: 8px;
  font-weight: 740;
}}
.dot {{
  width: 9px;
  height: 9px;
  border-radius: 999px;
  background: var(--dot);
  box-shadow: 0 0 18px var(--dot);
}}
.progress {{
  height: 8px;
  border-radius: 999px;
  background: #0a0f15;
  border: 1px solid var(--line-soft);
  overflow: hidden;
}}
.bar {{
  height: 100%;
  width: var(--value);
  background: linear-gradient(90deg, var(--dot), color-mix(in srgb, var(--dot), #ffffff 18%));
  border-radius: inherit;
}}
.rate-meta {{
  display: flex;
  justify-content: space-between;
  gap: 16px;
  margin-top: 12px;
}}
.pace {{
  margin-top: 10px;
  color: var(--muted);
  font-size: 12px;
}}
.breakdown {{
  display: grid;
  gap: 10px;
}}
.breakdown-row {{
  display: grid;
  grid-template-columns: 1fr auto;
  gap: 16px;
  padding: 10px 0;
  border-bottom: 1px solid var(--line-soft);
}}
.breakdown-row:last-child {{ border-bottom: 0; }}
.table-wrap {{ overflow: hidden; }}
table {{
  width: 100%;
  table-layout: fixed;
  border-collapse: collapse;
}}
th, td {{
  padding: 12px 10px;
  text-align: right;
  border-bottom: 1px solid var(--line-soft);
  color: #d8e4ef;
  white-space: nowrap;
}}
th {{
  color: var(--quiet);
  font-size: 11px;
  font-weight: 700;
}}
th:first-child, td:first-child {{
  width: 24%;
  text-align: left;
}}
th:not(:first-child), td:not(:first-child) {{
  width: 12.666%;
}}
tr:last-child td {{ border-bottom: 0; }}
.model-name {{
  display: inline-flex;
  align-items: center;
  max-width: 100%;
  border: 1px solid var(--line);
  border-radius: 999px;
  padding: 3px 8px;
  background: rgba(255, 255, 255, 0.025);
  font-weight: 650;
  overflow: hidden;
  text-overflow: ellipsis;
  vertical-align: middle;
}}
.cost {{ color: #cfe0ff; font-weight: 720; }}
.footer {{
  margin-top: 12px;
  color: var(--quiet);
  display: flex;
  justify-content: space-between;
  gap: 16px;
  flex-wrap: wrap;
}}
@media (max-width: 860px) {{
  .shell {{ width: min(100vw - 22px, 1180px); padding-top: 22px; }}
  .topbar {{ display: block; }}
  .plan {{ display: inline-flex; margin-top: 16px; }}
  .span-3, .span-4, .span-6, .span-8 {{ grid-column: span 12; }}
  th, td {{ padding: 10px 7px; font-size: 12px; }}
  .model-name {{ max-width: 92px; }}
}}
</style>
</head>
<body>
<main class="shell">
  <header class="topbar">
    <div>
      <h1>Codex Usage</h1>
      <p class="subtitle">Subscription usage, API-equivalent cost, cached-token pricing and reset windows from local Codex session events.</p>
    </div>
    <div class="plan">{}</div>
  </header>

  <section class="grid">
    <article class="card pad span-6" style="--dot:{}">
      <div class="rate-head">
        <div class="rate-title"><span class="dot"></span><span>5h rate limit</span></div>
        <div class="metric">{:.0}%</div>
      </div>
      <div class="progress"><div class="bar" style="--value:{:.4}%"></div></div>
      <div class="rate-meta">
        <span class="small">Resets in <strong class="muted">{}</strong></span>
        <span class="small">At <strong class="muted">{}</strong></span>
      </div>
    </article>

    <article class="card pad span-6" style="--dot:{}">
      <div class="rate-head">
        <div class="rate-title"><span class="dot"></span><span>Weekly rate limit</span></div>
        <div class="metric">{:.0}%</div>
      </div>
      <div class="progress"><div class="bar" style="--value:{:.4}%"></div></div>
      <div class="rate-meta">
        <span class="small">Resets in <strong class="muted">{}</strong></span>
        <span class="small">At <strong class="muted">{}</strong></span>
      </div>
    </article>

    <article class="card pad span-12" style="--dot:{}">
      <div class="rate-head">
        <div class="rate-title"><span class="dot"></span><span>Usage pace</span></div>
        <div class="metric">{}</div>
      </div>
      <div class="small">Expected <strong class="muted">{:.1}%</strong> of the 5h window by now.</div>
    </article>

    <article class="card pad span-12">
      <div class="rate-head">
        <div>
          <div class="label">Party mode</div>
          <div class="metric">{}</div>
        </div>
        <div class="plan">{}</div>
      </div>
      <div class="small">Reset notifications always stay enabled. Party mode controls the fullscreen confetti overlay and can be toggled from the tray menu.</div>
    </article>

    <article class="card pad span-4">
      <div class="label">Today's cost</div>
      <div class="metric">{}</div>
      <div class="small">{} tokens</div>
    </article>
    <article class="card pad span-4">
      <div class="label">Monthly cost</div>
      <div class="metric">{}</div>
      <div class="small">{} tokens</div>
      <div class="small">{}</div>
    </article>
    <article class="card pad span-4">
      <div class="label">All-time estimate</div>
      <div class="metric">{}</div>
      <div class="small">{} tokens</div>
    </article>

    <article class="card pad span-4">
      <div class="label">Token breakdown</div>
      <div class="breakdown">
        <div class="breakdown-row"><span>Input</span><strong>{}</strong></div>
        <div class="breakdown-row"><span>Cached input</span><strong>{}</strong></div>
        <div class="breakdown-row"><span>Output</span><strong>{}</strong></div>
        <div class="breakdown-row"><span>Reasoning</span><strong>{}</strong></div>
        <div class="breakdown-row"><span>Unpriced</span><strong>{}</strong></div>
      </div>
    </article>

    <article class="card span-8">
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>Model</th>
              <th>Total</th>
              <th>Input</th>
              <th>Cached</th>
              <th>Output</th>
              <th>Reasoning</th>
              <th>Cost</th>
            </tr>
          </thead>
          <tbody>{}</tbody>
        </table>
      </div>
    </article>
  </section>

  <footer class="footer">
    <span>Source: {} token_count events from {} JSONL files</span>
    <span>Costs include cached input pricing when token data is present.</span>
  </footer>
</main>
</body>
</html>"#,
        html_escape(display_plan(&rate.plan_type)),
        rate_color(rate.primary.used_percent),
        rate.primary.used_percent,
        rate.primary.used_percent.clamp(0.0, 100.0),
        html_escape(&reset_text(rate.primary.resets_at)),
        html_escape(&reset_clock_text(rate.primary.resets_at)),
        rate_color(rate.secondary.used_percent),
        rate.secondary.used_percent,
        rate.secondary.used_percent.clamp(0.0, 100.0),
        html_escape(&reset_text(rate.secondary.resets_at)),
        html_escape(&reset_clock_text(rate.secondary.resets_at)),
        pace_color(&rate.primary),
        html_escape(&pace_text(&rate.primary)),
        primary_pace(&rate.primary)
            .map(|pace| pace.expected_percent)
            .unwrap_or(0.0),
        if party_mode { "On" } else { "Off" },
        if party_mode {
            "Confetti enabled"
        } else {
            "Notifications only"
        },
        dollars(today_cost),
        full_tokens(stats.today.total_tokens),
        dollars(month_cost),
        full_tokens(stats.month.total_tokens),
        html_escape(&month_range),
        dollars(total_cost),
        full_tokens(stats.total.total_tokens),
        full_tokens(stats.total.input_tokens),
        full_tokens(stats.total.cached_input_tokens),
        full_tokens(stats.total.output_tokens),
        full_tokens(stats.total.reasoning_output_tokens),
        full_tokens(unpriced_tokens(stats)),
        model_rows,
        stats.events_seen,
        stats.files_seen
    )
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn rate_color(percent: f64) -> &'static str {
    if percent >= 85.0 {
        "#e35d5d"
    } else if percent >= 60.0 {
        "#e5b454"
    } else {
        "#35c46a"
    }
}

fn pace_color(limit: &WindowLimit) -> &'static str {
    let Some(pace) = primary_pace(limit) else {
        return "#627284";
    };
    if pace.ahead_percent >= PACE_ALERT_AHEAD_PERCENT {
        "#e35d5d"
    } else if pace.ahead_percent > 5.0 {
        "#e5b454"
    } else {
        "#35c46a"
    }
}

fn c_string(value: &str) -> CString {
    CString::new(value.replace('\0', "")).unwrap()
}

fn limit_dot(percent: f64) -> &'static str {
    if percent >= 90.0 {
        "🔴"
    } else if percent >= 70.0 {
        "🟠"
    } else if percent >= 50.0 {
        "🟡"
    } else {
        "🟢"
    }
}

fn icon_color(percent: f64) -> String {
    smooth_limit_color(percent)
}

fn smooth_limit_color(used_percent: f64) -> String {
    let remaining = (100.0 - used_percent).clamp(0.0, 100.0);
    let (from, to, t) = if remaining >= 50.0 {
        let t = (100.0 - remaining) / 50.0;
        ((0x35, 0xc4, 0x6a), (0xe5, 0xb4, 0x54), t)
    } else {
        let t = (50.0 - remaining) / 50.0;
        ((0xe5, 0xb4, 0x54), (0xe3, 0x5d, 0x5d), t)
    };
    let r = lerp_u8(from.0, to.0, t);
    let g = lerp_u8(from.1, to.1, t);
    let b = lerp_u8(from.2, to.2, t);
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn lerp_u8(from: u8, to: u8, t: f64) -> u8 {
    (from as f64 + (to as f64 - from as f64) * t).round() as u8
}

fn ensure_icon(percent: f64) -> String {
    let pct = percent.clamp(0.0, 99.0).round() as i64;
    let name = format!("codex-usage-{pct}");
    let dir = icon_dir();
    let _ = fs::create_dir_all(dir);
    let path = icon_dir().join(format!("{name}.svg"));
    if !path.exists() {
        let color = icon_color(percent);
        let font_size = if pct >= 100 {
            30
        } else if pct >= 10 {
            40
        } else {
            42
        };
        let svg = format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64">
  <text x="32" y="45" text-anchor="middle" font-family="SF Pro Text, SF Pro Display, Inter, Arial, sans-serif" font-size="{font_size}" font-weight="650" fill="{color}">{pct}</text>
</svg>
"##
        );
        let _ = fs::write(path, svg);
    }
    name
}

unsafe fn set_markup(label: *mut GtkWidget, markup: &str) {
    let markup = c_string(markup);
    unsafe { gtk_label_set_markup(label, markup.as_ptr()) };
}

unsafe extern "C" fn on_refresh(_widget: *mut GtkWidget, _data: *mut c_void) {
    update_state();
}

unsafe extern "C" fn on_details(_widget: *mut GtkWidget, _data: *mut c_void) {
    let stats = collect_stats();
    let details = details_html_path();
    let _ = fs::write(&details, make_details_html(&stats));
    let _ = Command::new("xdg-open").arg(details).spawn();
}

unsafe extern "C" fn on_toggle_party_mode(_widget: *mut GtkWidget, _data: *mut c_void) {
    set_party_mode(!party_mode_enabled());
    update_state();
}

unsafe extern "C" fn on_quit(_widget: *mut GtkWidget, _data: *mut c_void) {
    unsafe { gtk_main_quit() };
}

unsafe extern "C" fn on_timer(_data: *mut c_void) -> c_int {
    update_state();
    1
}

unsafe extern "C" fn quit_timer(_data: *mut c_void) -> c_int {
    unsafe { gtk_main_quit() };
    0
}

unsafe fn make_window_transparent(window: *mut GtkWidget) {
    unsafe {
        gtk_widget_set_app_paintable(window, 1);
        let screen = gtk_widget_get_screen(window);
        if !screen.is_null() {
            let visual = gdk_screen_get_rgba_visual(screen);
            if !visual.is_null() {
                gtk_widget_set_visual(window, visual);
            }
            let provider = gtk_css_provider_new();
            let css = c_string(
                "window, label { background: transparent; background-color: transparent; }\
                 label { color: #f8fafc; text-shadow: 0 2px 8px rgba(0,0,0,0.85); }",
            );
            gtk_css_provider_load_from_data(provider, css.as_ptr(), -1, ptr::null_mut());
            gtk_style_context_add_provider_for_screen(screen, provider, 600);
        }
    }
}

struct Particle {
    x: f64,
    y: f64,
    vx: f64,
    vy: f64,
    size: f64,
    spin: f64,
    angle: f64,
    color: (f64, f64, f64),
}

struct ConfettiState {
    window: *mut GtkWidget,
    canvas: *mut GtkWidget,
    emoji_label: *mut GtkWidget,
    particles: Vec<Particle>,
    frames_left: i32,
    total_frames: i32,
    width: f64,
    height: f64,
}

fn next_rand(seed: &mut u64) -> f64 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    ((*seed >> 32) as f64) / (u32::MAX as f64)
}

fn make_particles(count: usize, width: f64, height: f64, big: bool) -> Vec<Particle> {
    let colors = [
        (0.98, 0.32, 0.32),
        (0.20, 0.77, 0.42),
        (0.28, 0.56, 1.00),
        (0.95, 0.78, 0.22),
        (0.72, 0.42, 1.00),
        (1.00, 0.48, 0.18),
    ];
    let mut seed = if big { 0xfeed_cafe } else { 0xdeca_fbad };
    let mut particles = Vec::with_capacity(count);
    for i in 0..count {
        let x = next_rand(&mut seed) * width;
        let y = -height * next_rand(&mut seed);
        let spread = if big { 4.8 } else { 3.0 };
        particles.push(Particle {
            x,
            y,
            vx: (next_rand(&mut seed) - 0.5) * spread,
            vy: 2.2 + next_rand(&mut seed) * if big { 6.2 } else { 4.2 },
            size: 5.0 + next_rand(&mut seed) * if big { 12.0 } else { 8.0 },
            spin: (next_rand(&mut seed) - 0.5) * 0.28,
            angle: next_rand(&mut seed) * std::f64::consts::TAU,
            color: colors[i % colors.len()],
        });
    }
    particles
}

unsafe extern "C" fn draw_confetti(
    widget: *mut GtkWidget,
    cr: *mut Cairo,
    data: *mut c_void,
) -> c_int {
    let state = unsafe { &mut *(data as *mut ConfettiState) };
    let width = unsafe { gtk_widget_get_allocated_width(widget) }.max(1) as f64;
    let height = unsafe { gtk_widget_get_allocated_height(widget) }.max(1) as f64;
    let elapsed = (state.total_frames - state.frames_left).max(0) as f64;
    let fade_in = (elapsed / 28.0).clamp(0.0, 1.0);
    let fade_out = (state.frames_left as f64 / 45.0).clamp(0.0, 1.0);
    let alpha = fade_in.min(fade_out);
    unsafe {
        cairo_set_operator(cr, 1);
        cairo_set_source_rgba(cr, 0.0, 0.0, 0.0, 0.0);
        cairo_paint(cr);
        cairo_set_operator(cr, 2);

        for particle in &state.particles {
            cairo_save(cr);
            cairo_translate(
                cr,
                particle.x * width / state.width,
                particle.y * height / state.height,
            );
            cairo_rotate(cr, particle.angle);
            cairo_set_source_rgba(
                cr,
                particle.color.0,
                particle.color.1,
                particle.color.2,
                0.92 * alpha,
            );
            if particle.size > 11.0 {
                cairo_arc(
                    cr,
                    0.0,
                    0.0,
                    particle.size * 0.45,
                    0.0,
                    std::f64::consts::TAU,
                );
            } else {
                cairo_rectangle(
                    cr,
                    -particle.size * 0.55,
                    -particle.size * 0.30,
                    particle.size * 1.10,
                    particle.size * 0.60,
                );
            }
            cairo_fill(cr);
            cairo_restore(cr);
        }
    }
    0
}

unsafe extern "C" fn tick_confetti(data: *mut c_void) -> c_int {
    let state = unsafe { &mut *(data as *mut ConfettiState) };
    state.frames_left -= 1;
    if state.frames_left <= 0 {
        unsafe { gtk_widget_destroy(state.window) };
        drop(unsafe { Box::from_raw(data as *mut ConfettiState) });
        return 0;
    }
    for particle in &mut state.particles {
        particle.x += particle.vx;
        particle.y += particle.vy;
        particle.vy += 0.035;
        particle.angle += particle.spin;
        if particle.y > state.height + 40.0 {
            particle.y = -20.0;
        }
        if particle.x < -40.0 {
            particle.x = state.width + 40.0;
        } else if particle.x > state.width + 40.0 {
            particle.x = -40.0;
        }
    }
    let elapsed = (state.total_frames - state.frames_left).max(0) as f64;
    let fade_in = (elapsed / 28.0).clamp(0.0, 1.0);
    let fade_out = (state.frames_left as f64 / 45.0).clamp(0.0, 1.0);
    unsafe { gtk_widget_set_opacity(state.emoji_label, fade_in.min(fade_out)) };
    unsafe { gtk_widget_queue_draw(state.canvas) };
    1
}

fn overlay_markup(message: &str, big: bool) -> String {
    let body = if big {
        "THE WEEKLY RATE LIMIT HAS BEEN RESET!"
    } else if message.contains("5 hour") {
        "The 5 hour rate limit has been reset!"
    } else {
        message
    };
    if big {
        format!(
            "<span font_desc=\"72\">🎉 🎊 🥳 ✨ 🎉</span>\n<span font_desc=\"24\" weight=\"bold\">{body}</span>"
        )
    } else {
        format!(
            "<span font_desc=\"58\">🎉 🎊 ✨</span>\n<span font_desc=\"20\" weight=\"bold\">{body}</span>"
        )
    }
}

fn show_confetti(message: &str, big: bool) {
    unsafe {
        let window = gtk_window_new(0);
        let width = 1920.0;
        let height = 1080.0;
        let frames = 10 * 60;
        gtk_layer_init_for_window(window);
        gtk_layer_set_namespace(window, c_string("codex-usage-party").as_ptr());
        gtk_layer_set_layer(window, 3);
        gtk_layer_set_anchor(window, 0, 1);
        gtk_layer_set_anchor(window, 1, 1);
        gtk_layer_set_anchor(window, 2, 1);
        gtk_layer_set_anchor(window, 3, 1);
        gtk_layer_set_margin(window, 2, 0);
        gtk_layer_set_exclusive_zone(window, -1);
        gtk_layer_set_keyboard_mode(window, 0);
        gtk_window_set_title(window, c_string("Codex Usage Party").as_ptr());
        gtk_window_set_default_size(window, width as c_int, height as c_int);
        gtk_window_set_decorated(window, 0);
        gtk_window_set_keep_above(window, 1);
        gtk_widget_set_size_request(window, width as c_int, height as c_int);
        make_window_transparent(window);

        let overlay = gtk_overlay_new();
        let canvas = gtk_drawing_area_new();
        gtk_widget_set_size_request(canvas, width as c_int, height as c_int);
        gtk_container_add(overlay, canvas);

        let emoji_label = gtk_label_new(ptr::null());
        gtk_label_set_markup(
            emoji_label,
            c_string(&overlay_markup(message, big)).as_ptr(),
        );
        gtk_label_set_xalign(emoji_label, 0.5);
        gtk_widget_set_halign(emoji_label, 3);
        gtk_widget_set_valign(emoji_label, 3);
        gtk_widget_set_opacity(emoji_label, 0.0);
        gtk_overlay_add_overlay(overlay, emoji_label);

        let state = Box::new(ConfettiState {
            window,
            canvas,
            emoji_label,
            particles: make_particles(if big { 260 } else { 150 }, width, height, big),
            frames_left: frames,
            total_frames: frames,
            width,
            height,
        });
        let state_ptr = Box::into_raw(state);
        g_signal_connect_data(
            canvas as *mut c_void,
            c_string("draw").as_ptr(),
            draw_confetti as *mut c_void,
            state_ptr as *mut c_void,
            ptr::null_mut(),
            0,
        );
        gtk_container_add(window, overlay);
        gtk_widget_show_all(window);
        g_timeout_add(16, Some(tick_confetti), state_ptr as *mut c_void);
    }
}

fn send_reset_notification(body: &str, party: bool) {
    let _ = Command::new("notify-send")
        .arg("Codex Usage")
        .arg(body)
        .spawn();
    if party_mode_enabled() {
        show_confetti(body, party);
    }
}

fn send_plain_notification(body: &str) {
    let _ = Command::new("notify-send")
        .arg("Codex Usage")
        .arg(body)
        .spawn();
}

fn maybe_notify_primary_reset(state: &mut AppState, rate: &RateLimits) {
    let current_reset = rate.primary.resets_at;
    if state.seen_primary_window {
        let reset_moved = state
            .last_primary_resets_at
            .zip(current_reset)
            .is_some_and(|(previous, current)| current > previous);
        if reset_moved {
            send_reset_notification("The 5 hour rate limit has been reset! 🎉", false);
        }
    }
    state.seen_primary_window = true;
    state.last_primary_resets_at = current_reset;
}

fn maybe_notify_fast_pace(state: &mut AppState, rate: &RateLimits) {
    if rate.primary.resets_at != state.pace_alert_window {
        state.pace_alert_active = false;
        state.pace_alert_window = rate.primary.resets_at;
    }
    let Some(pace) = primary_pace(&rate.primary) else {
        state.pace_alert_active = false;
        return;
    };
    if pace.ahead_percent <= PACE_ALERT_CLEAR_PERCENT {
        state.pace_alert_active = false;
    }
    if !state.pace_alert_active && pace.ahead_percent >= PACE_ALERT_AHEAD_PERCENT {
        send_plain_notification(
            "Slow down, cowboy! 🤠 You are using up your rate limit FAST. Watch out! 🐬",
        );
        state.pace_alert_active = true;
    }
}

fn maybe_notify_secondary_reset(state: &mut AppState, rate: &RateLimits) {
    let current_reset = rate.secondary.resets_at;
    if state.seen_secondary_window {
        let reset_moved = state
            .last_secondary_resets_at
            .zip(current_reset)
            .is_some_and(|(previous, current)| current > previous);
        if reset_moved {
            send_reset_notification("THE WEEKLY RATE LIMIT HAS BEEN RESET! 🎉🎊🥳✨", true);
        }
    }
    state.seen_secondary_window = true;
    state.last_secondary_resets_at = current_reset;
}

fn make_render_snapshot(stats: &Stats) -> RenderSnapshot {
    let rate = stats.rate_limits.clone().unwrap_or_default();
    let total_cost = sum_cost(&stats.by_model);
    let today_cost = sum_cost(&stats.today_by_model);
    let month_cost = sum_cost(&stats.month_by_model);
    RenderSnapshot {
        limit_markup: format!(
            "{}  <b>5h</b>  {:.0}%  |  reset in {} at {}",
            limit_dot(rate.primary.used_percent),
            rate.primary.used_percent,
            reset_text(rate.primary.resets_at),
            reset_clock_text(rate.primary.resets_at)
        ),
        weekly_markup: format!(
            "{}  <b>Weekly</b>  {:.0}%  |  reset in {} at {}",
            limit_dot(rate.secondary.used_percent),
            rate.secondary.used_percent,
            reset_text(rate.secondary.resets_at),
            reset_clock_text(rate.secondary.resets_at)
        ),
        pace_markup: format!(
            "{}  <b>Pace:</b>  {}",
            pace_dot(&rate.primary),
            pace_text(&rate.primary)
        ),
        pace_expected_markup: format!(
            "   Expected usage:  <b>{:.1}%</b>",
            primary_pace(&rate.primary)
                .map(|pace| pace.expected_percent)
                .unwrap_or(0.0)
        ),
        cost_today_markup: format!("🔵  Today's cost:  <b>{}</b>", dollars(today_cost)),
        cost_month_markup: format!("🔵  Monthly cost:  <b>{}</b>", dollars(month_cost)),
        cost_total_markup: format!("🔵  Total estimated cost:  <b>{}</b>", dollars(total_cost)),
        tokens_today_markup: format!(
            "🟣  Today's token usage:  <b>{}</b>",
            full_tokens(stats.today.total_tokens)
        ),
        tokens_total_markup: format!(
            "🟣  Total token usage:  <b>{}</b>",
            full_tokens(stats.total.total_tokens)
        ),
        party_mode_markup: party_mode_markup(),
        tray_label: format!(
            "5h {:.0}% | {}",
            rate.primary.used_percent,
            dollars(today_cost)
        ),
        title: format!(
            "Codex Usage | 5h {:.0}% | reset in {} at {} | Pace: {} | Weekly {:.0}% | reset in {} at {}",
            rate.primary.used_percent,
            reset_text(rate.primary.resets_at),
            reset_clock_text(rate.primary.resets_at),
            pace_text(&rate.primary),
            rate.secondary.used_percent,
            reset_text(rate.secondary.resets_at),
            reset_clock_text(rate.secondary.resets_at)
        ),
        icon_name: ensure_icon(rate.primary.used_percent),
    }
}

fn update_state() {
    let stats = collect_stats();
    let rate = stats.rate_limits.clone().unwrap_or_default();
    if let Some(state) = STATE.get() {
        let mut state = state.lock().unwrap();
        maybe_notify_primary_reset(&mut state, &rate);
        maybe_notify_fast_pace(&mut state, &rate);
        maybe_notify_secondary_reset(&mut state, &rate);
        let snapshot = make_render_snapshot(&stats);
        if state.last_render.as_ref() == Some(&snapshot) {
            return;
        }
        unsafe {
            set_markup(state.limit_label, &snapshot.limit_markup);
            set_markup(state.weekly_label, &snapshot.weekly_markup);
            set_markup(state.pace_label, &snapshot.pace_markup);
            set_markup(state.pace_expected_label, &snapshot.pace_expected_markup);
            set_markup(state.cost_today_label, &snapshot.cost_today_markup);
            set_markup(state.cost_month_label, &snapshot.cost_month_markup);
            set_markup(state.cost_total_label, &snapshot.cost_total_markup);
            set_markup(state.tokens_today_label, &snapshot.tokens_today_markup);
            set_markup(state.tokens_total_label, &snapshot.tokens_total_markup);
            set_markup(state.party_mode_label, &snapshot.party_mode_markup);
            let tray_label = c_string(&snapshot.tray_label);
            let guide = c_string("5h 000% | $000");
            app_indicator_set_label(state.indicator, tray_label.as_ptr(), guide.as_ptr());
            let title = c_string(&snapshot.title);
            app_indicator_set_title(state.indicator, title.as_ptr());
            let icon_name = c_string(&snapshot.icon_name);
            let icon_desc = c_string("Codex usage");
            app_indicator_set_icon_full(state.indicator, icon_name.as_ptr(), icon_desc.as_ptr());
        }
        state.last_render = Some(snapshot);
    }
}

unsafe fn menu_item(label: &str, sensitive: bool) -> *mut GtkWidget {
    let label = c_string(label);
    let item = unsafe { gtk_menu_item_new_with_label(label.as_ptr()) };
    unsafe { gtk_widget_set_sensitive(item, if sensitive { 1 } else { 0 }) };
    item
}

unsafe fn markup_menu_item(markup: &str) -> (*mut GtkWidget, *mut GtkWidget) {
    let item = unsafe { gtk_menu_item_new() };
    let label = unsafe { gtk_label_new(ptr::null()) };
    unsafe {
        gtk_label_set_xalign(label, 0.0);
        set_markup(label, markup);
        gtk_container_add(item, label);
        gtk_widget_set_sensitive(item, 1);
    }
    (item, label)
}

unsafe fn connect_activate(
    item: *mut GtkWidget,
    callback: unsafe extern "C" fn(*mut GtkWidget, *mut c_void),
) {
    let signal = c_string("activate");
    unsafe {
        g_signal_connect_data(
            item as *mut c_void,
            signal.as_ptr(),
            callback as *mut c_void,
            ptr::null_mut(),
            ptr::null_mut(),
            0,
        );
    }
}

fn main() {
    if std::env::args().any(|arg| arg == "--once") {
        let stats = collect_stats();
        println!("{}", make_details_text(&stats));
        return;
    }
    if std::env::args().any(|arg| arg == "--html") {
        let stats = collect_stats();
        println!("{}", make_details_html(&stats));
        return;
    }
    if std::env::args().any(|arg| arg == "--test-5h-reset") {
        unsafe {
            gtk_init(ptr::null_mut(), ptr::null_mut());
            send_reset_notification("The 5 hour rate limit has been reset! 🎉", false);
            g_timeout_add_seconds(12, Some(quit_timer), ptr::null_mut());
            gtk_main();
        }
        return;
    }
    if std::env::args().any(|arg| arg == "--test-weekly-reset") {
        unsafe {
            gtk_init(ptr::null_mut(), ptr::null_mut());
            send_reset_notification("THE WEEKLY RATE LIMIT HAS BEEN RESET! 🎉🎊🥳✨", true);
            g_timeout_add_seconds(12, Some(quit_timer), ptr::null_mut());
            gtk_main();
        }
        return;
    }
    if std::env::args().any(|arg| arg == "--test-pace-alert") {
        send_plain_notification(
            "Slow down, cowboy! 🤠 You are using up your rate limit FAST. Watch out! 🐬",
        );
        return;
    }

    unsafe {
        gtk_init(ptr::null_mut(), ptr::null_mut());
        let indicator = app_indicator_new(
            c_string("codex-usage-tray").as_ptr(),
            c_string("codex-desktop").as_ptr(),
            0,
        );
        app_indicator_set_status(indicator, 1);
        let icon_path = c_string(&icon_dir().to_string_lossy());
        app_indicator_set_icon_theme_path(indicator, icon_path.as_ptr());
        let menu = gtk_menu_new();
        let (rate_header, _rate_header_label) = markup_menu_item("<b>Rate limits</b>");
        let (limit_item, limit_label) = markup_menu_item("⚪  <b>5h</b>  loading...");
        let (weekly_item, weekly_label) = markup_menu_item("⚪  <b>Weekly</b>  loading...");
        let (pace_item, pace_label) = markup_menu_item("⚪  <b>Pace:</b>  loading...");
        let (pace_expected_item, pace_expected_label) =
            markup_menu_item("   Expected usage:  <b>loading...</b>");
        let (cost_today_item, cost_today_label) =
            markup_menu_item("🔵  Today's cost:  <b>loading...</b>");
        let (cost_month_item, cost_month_label) =
            markup_menu_item("🔵  Monthly cost:  <b>loading...</b>");
        let (cost_total_item, cost_total_label) =
            markup_menu_item("🔵  Total estimated cost:  <b>loading...</b>");
        let (tokens_today_item, tokens_today_label) =
            markup_menu_item("🟣  Today's token usage:  <b>loading...</b>");
        let (tokens_total_item, tokens_total_label) =
            markup_menu_item("🟣  Total token usage:  <b>loading...</b>");
        let (party_mode_item, party_mode_label) = markup_menu_item(&party_mode_markup());
        let details = menu_item("Details", true);
        let refresh = menu_item("Refresh", true);
        let quit = menu_item("Quit", true);
        for item in [
            rate_header,
            limit_item,
            weekly_item,
            gtk_separator_menu_item_new(),
            pace_item,
            pace_expected_item,
            gtk_separator_menu_item_new(),
            cost_today_item,
            cost_month_item,
            cost_total_item,
            gtk_separator_menu_item_new(),
            tokens_today_item,
            tokens_total_item,
            gtk_separator_menu_item_new(),
            party_mode_item,
            gtk_separator_menu_item_new(),
            details,
            gtk_separator_menu_item_new(),
            refresh,
            quit,
        ] {
            gtk_menu_shell_append(menu, item);
        }
        connect_activate(party_mode_item, on_toggle_party_mode);
        connect_activate(details, on_details);
        connect_activate(refresh, on_refresh);
        connect_activate(quit, on_quit);
        gtk_widget_show_all(menu);
        app_indicator_set_menu(indicator, menu as *mut GtkMenu);
        STATE
            .set(Mutex::new(AppState {
                indicator,
                limit_label,
                weekly_label,
                pace_label,
                pace_expected_label,
                cost_today_label,
                cost_month_label,
                cost_total_label,
                tokens_today_label,
                tokens_total_label,
                party_mode_label,
                last_render: None,
                seen_primary_window: false,
                last_primary_resets_at: None,
                pace_alert_active: false,
                pace_alert_window: None,
                seen_secondary_window: false,
                last_secondary_resets_at: None,
            }))
            .ok();
        update_state();
        g_timeout_add_seconds(REFRESH_SECONDS, Some(on_timer), ptr::null_mut());
        gtk_main();
    }
}
