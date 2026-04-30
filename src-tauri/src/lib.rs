use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, LogicalSize, Manager};
use tauri_plugin_autostart::ManagerExt;
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tokio::time::{sleep, timeout, Duration, Instant};
use waapi_rs::{ak, SubscriptionHandle, WaapiClient};

type ReconnectSignal = Arc<Notify>;

const MONITOR_TICK_MS: u64 = 1500;
const SCORE_FLUSH_INTERVAL_SECS: u64 = 3;
const BACKOFF_INITIAL_MS: u64 = 1500;
const BACKOFF_MAX_MS: u64 = 30_000;

// ── Connection configuration ──────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Config {
    tcp_probe_timeout: Duration,
    connect_timeout: Duration,
    getinfo_timeout: Duration,
    subscribe_timeout: Duration,
    disconnect_timeout: Duration,
    backoff_initial_ms: u64,
    backoff_max_ms: u64,
}

impl Config {
    fn load_from_env() -> Self {
        let tcp_probe_ms = std::env::var("WWAAPI_TCP_PROBE_MS").ok().and_then(|s| s.parse().ok()).unwrap_or(300u64);
        let connect_secs = std::env::var("WWAAPI_CONNECT_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(4u64);
        let getinfo_secs = std::env::var("WWAAPI_GETINFO_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(3u64);
        let subscribe_secs = std::env::var("WWAAPI_SUBSCRIBE_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(5u64);
        let disconnect_secs = std::env::var("WWAAPI_DISCONNECT_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(2u64);
        let backoff_initial = std::env::var("WWAAPI_BACKOFF_INITIAL_MS").ok().and_then(|s| s.parse().ok()).unwrap_or(BACKOFF_INITIAL_MS);
        let backoff_max = std::env::var("WWAAPI_BACKOFF_MAX_MS").ok().and_then(|s| s.parse().ok()).unwrap_or(BACKOFF_MAX_MS);

        Config {
            tcp_probe_timeout: Duration::from_millis(tcp_probe_ms),
            connect_timeout: Duration::from_secs(connect_secs),
            getinfo_timeout: Duration::from_secs(getinfo_secs),
            subscribe_timeout: Duration::from_secs(subscribe_secs),
            disconnect_timeout: Duration::from_secs(disconnect_secs),
            backoff_initial_ms: backoff_initial,
            backoff_max_ms: backoff_max,
        }
    }
}

fn next_backoff(current_ms: u64, max_ms: u64) -> u64 {
    current_ms.saturating_mul(2).min(max_ms)
}

// ── Score configuration defaults ──────────────────────────────────────────────

/// Provides built-in default scores used to seed UserSettings on first run.
/// Kept as a struct so existing unit tests can reference it directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScoreConfig {
    scores: HashMap<String, u64>,
}

impl ScoreConfig {
    fn defaults() -> HashMap<String, u64> {
        [
            (ak::wwise::core::AUDIO_IMPORTED, 4u64),
            (ak::wwise::core::OBJECT_ATTENUATION_CURVE_CHANGED, 1),
            (ak::wwise::core::OBJECT_ATTENUATION_CURVE_LINK_CHANGED, 1),
            (ak::wwise::core::OBJECT_CHILD_ADDED, 3),
            (ak::wwise::core::OBJECT_CHILD_REMOVED, 2),
            (ak::wwise::core::OBJECT_CREATED, 5),
            (ak::wwise::core::OBJECT_CURVE_CHANGED, 3),
            (ak::wwise::core::OBJECT_NAME_CHANGED, 2),
            (ak::wwise::core::OBJECT_NOTES_CHANGED, 1),
            (ak::wwise::core::OBJECT_POST_DELETED, 3),
            (ak::wwise::core::OBJECT_PROPERTY_CHANGED, 3),
            (ak::wwise::core::OBJECT_REFERENCE_CHANGED, 4),
            ("ak.wwise.core.object.structureChanged", 5),
            (ak::wwise::core::PROJECT_LOADED, 3),
            (ak::wwise::core::PROJECT_POST_CLOSED, 2),
            (ak::wwise::core::PROJECT_SAVED, 3),
            (ak::wwise::core::SOUNDBANK_GENERATED, 5),
            (ak::wwise::core::SOUNDBANK_GENERATION_DONE, 5),
            (ak::wwise::core::SWITCH_CONTAINER_ASSIGNMENT_ADDED, 4),
            (ak::wwise::core::SWITCH_CONTAINER_ASSIGNMENT_REMOVED, 3),
            (ak::wwise::core::TRANSPORT_STATE_CHANGED, 3),
            (ak::wwise::ui::SELECTION_CHANGED, 1),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
    }
}

// ── User settings ─────────────────────────────────────────────────────────────

/// Per-topic settings: score value and whether the subscription is active.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicSetting {
    pub uri: String,
    pub score: u64,
    pub enabled: bool,
}

/// All user-editable settings, persisted to `settings.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSettings {
    pub port_start: u16,
    pub port_end: u16,
    pub topics: Vec<TopicSetting>,
}

type SharedSettings = Arc<Mutex<UserSettings>>;

/// Topics that are off by default (unsupported by some Wwise versions or noisy).
const TOPICS_DISABLED_BY_DEFAULT: &[&str] = &[
    ak::wwise::core::OBJECT_PROPERTY_CHANGED,
    "ak.wwise.core.object.structureChanged",
    ak::wwise::core::TRANSPORT_STATE_CHANGED,
];

impl Default for UserSettings {
    fn default() -> Self {
        let defaults = ScoreConfig::defaults();
        let topics = TOPICS
            .iter()
            .map(|&uri| TopicSetting {
                uri: uri.to_string(),
                score: defaults.get(uri).copied().unwrap_or(1),
                enabled: !TOPICS_DISABLED_BY_DEFAULT.contains(&uri),
            })
            .collect();
        UserSettings {
            port_start: 8080,
            port_end: 8083,
            topics,
        }
    }
}

impl UserSettings {
    /// Load from `path`, falling back to defaults. Unknown topics in the file
    /// are preserved; topics missing from the file are filled with defaults.
    fn load_or_default(path: &PathBuf) -> Self {
        match std::fs::read_to_string(path) {
            Ok(content) => match serde_json::from_str::<UserSettings>(&content) {
                Ok(mut loaded) => {
                    // Ensure every known topic is present (forward-compat).
                    let defaults = ScoreConfig::defaults();
                    for &uri in TOPICS {
                        if !loaded.topics.iter().any(|t| t.uri == uri) {
                            loaded.topics.push(TopicSetting {
                                uri: uri.to_string(),
                                score: defaults.get(uri).copied().unwrap_or(1),
                                enabled: true,
                            });
                        }
                    }
                    loaded
                }
                Err(e) => {
                    eprintln!("[wwise-rank] failed to parse settings.json: {e}, using defaults");
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    fn save(&self, path: &PathBuf) {
        if let Ok(data) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, data);
        }
    }
}

// ── Subscribed topics ─────────────────────────────────────────────────────────

const TOPICS: &[&str] = &[
    ak::wwise::core::AUDIO_IMPORTED,
    ak::wwise::core::OBJECT_ATTENUATION_CURVE_CHANGED,
    ak::wwise::core::OBJECT_ATTENUATION_CURVE_LINK_CHANGED,
    ak::wwise::core::OBJECT_CHILD_ADDED,
    ak::wwise::core::OBJECT_CHILD_REMOVED,
    ak::wwise::core::OBJECT_CREATED,
    ak::wwise::core::OBJECT_CURVE_CHANGED,
    ak::wwise::core::OBJECT_NAME_CHANGED,
    ak::wwise::core::OBJECT_NOTES_CHANGED,
    ak::wwise::core::OBJECT_POST_DELETED,
    ak::wwise::core::OBJECT_PROPERTY_CHANGED,
    ak::wwise::core::OBJECT_REFERENCE_CHANGED,
    "ak.wwise.core.object.structureChanged",
    ak::wwise::core::PROJECT_LOADED,
    ak::wwise::core::PROJECT_POST_CLOSED,
    ak::wwise::core::PROJECT_SAVED,
    ak::wwise::core::SOUNDBANK_GENERATED,
    ak::wwise::core::SOUNDBANK_GENERATION_DONE,
    ak::wwise::core::SWITCH_CONTAINER_ASSIGNMENT_ADDED,
    ak::wwise::core::SWITCH_CONTAINER_ASSIGNMENT_REMOVED,
    ak::wwise::core::TRANSPORT_STATE_CHANGED,
    ak::wwise::ui::SELECTION_CHANGED,
];

// ── Data structures ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ScoreData {
    total_score: u64,
    #[serde(default)]
    event_counts: HashMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
enum ConnState {
    Idle,
    Scanning,
    Connected,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScorePayload {
    pub total_score: u64,
    pub session_score: u64,
    pub event_counts: HashMap<String, u64>,
    pub state: String,
    pub recent_events: u32,
}

struct AppState {
    score: ScoreData,
    score_dirty: bool,
    session_score: u64,
    conn_state: ConnState,
    recent_events: u32,
}

type SharedState = Arc<Mutex<AppState>>;

// ── Persistence ───────────────────────────────────────────────────────────────

fn load_score(path: &PathBuf) -> ScoreData {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_score(path: &PathBuf, score: &ScoreData) {
    if let Ok(data) = serde_json::to_string_pretty(score) {
        let _ = std::fs::write(path, data);
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn emit_state(app: &AppHandle, state: &AppState) {
    let payload = ScorePayload {
        total_score: state.score.total_score,
        session_score: state.session_score,
        event_counts: state.score.event_counts.clone(),
        state: format!("{:?}", state.conn_state),
        recent_events: state.recent_events,
    };
    let _ = app.emit("score-updated", payload);
}

fn set_conn_state(shared: &SharedState, app: &AppHandle, new_state: ConnState) {
    let mut st = shared.lock().unwrap_or_else(|e| e.into_inner());
    if st.conn_state != new_state {
        st.conn_state = new_state;
        emit_state(app, &st);
    }
}

/// Probe ports in [start, end] on 127.0.0.1, return those that accept TCP.
async fn tcp_open_ports_in_range(start: u16, end: u16, config: &Arc<Config>) -> Vec<u16> {
    let mut join_handles = Vec::new();
    for port in start..=end {
        let probe_timeout = config.tcp_probe_timeout;
        join_handles.push(tokio::spawn(async move {
            let ok = timeout(
                probe_timeout,
                TcpStream::connect(("127.0.0.1", port)),
            )
            .await
            .map(|r| r.is_ok())
            .unwrap_or(false);
            if ok { Some(port) } else { None }
        }));
    }

    let mut ports = Vec::new();
    for h in join_handles {
        if let Ok(Some(p)) = h.await {
            ports.push(p);
        }
    }
    ports.sort_unstable();
    ports
}

// ── Subscribe helper ──────────────────────────────────────────────────────────

async fn subscribe_one(
    client: &WaapiClient,
    topic: &'static str,
    score: u64,
    shared: SharedState,
    app: AppHandle,
    subscribe_timeout: Duration,
) -> Option<SubscriptionHandle> {
    match timeout(
        subscribe_timeout,
        client.subscribe(topic, None, move |_kwargs| {
            let mut st = shared.lock().unwrap_or_else(|e| e.into_inner());
            *st.score.event_counts.entry(topic.to_string()).or_insert(0) += 1;
            st.score.total_score += score;
            st.score_dirty = true;
            st.session_score += score;
            let boost: u32 = if st.recent_events < 8 { 3 } else if st.recent_events < 16 { 2 } else { 1 };
            st.recent_events = (st.recent_events + boost).min(24);
            emit_state(&app, &st);
        }),
    )
    .await
    {
        Ok(Ok(h)) => Some(h),
        Ok(Err(e)) => {
            eprintln!("[wwise-rank] subscribe failed for '{topic}': {e}");
            None
        }
        Err(_) => {
            eprintln!("[wwise-rank] subscribe timed out for '{topic}'");
            None
        }
    }
}

// ── WAAPI connect + subscribe ─────────────────────────────────────────────────

async fn try_connect(
    port: u16,
    app: &AppHandle,
    shared: &SharedState,
    config: &Arc<Config>,
    settings: &UserSettings,
) -> Option<(WaapiClient, Vec<SubscriptionHandle>)> {
    let url = format!("ws://127.0.0.1:{}/waapi", port);

    let client = match timeout(config.connect_timeout, WaapiClient::connect_with_url(&url)).await {
        Ok(Ok(c)) => c,
        _ => return None,
    };

    match timeout(config.getinfo_timeout, client.call(ak::wwise::core::GET_INFO, None, None)).await {
        Ok(Ok(_)) => {}
        _ => {
            let _ = timeout(config.disconnect_timeout, client.disconnect()).await;
            return None;
        }
    }

    let mut handles: Vec<SubscriptionHandle> = Vec::new();

    for &topic in TOPICS {
        // Look up this topic in user settings; skip if disabled.
        let topic_cfg = settings.topics.iter().find(|t| t.uri == topic);
        let enabled = topic_cfg.map_or(true, |t| t.enabled);
        if !enabled {
            continue;
        }
        let score = topic_cfg.map_or(1, |t| t.score);

        if let Some(h) = subscribe_one(
            &client,
            topic,
            score,
            Arc::clone(shared),
            app.clone(),
            config.subscribe_timeout,
        )
        .await
        {
            handles.push(h);
        }
    }

    if handles.is_empty() {
        eprintln!("[wwise-rank] no subscriptions succeeded on port {port}, disconnecting");
        let _ = timeout(config.disconnect_timeout, client.disconnect()).await;
        return None;
    }

    Some((client, handles))
}

// ── Background monitor loop ───────────────────────────────────────────────────

async fn monitor_loop(
    shared: SharedState,
    app: AppHandle,
    data_path: PathBuf,
    config: Arc<Config>,
    shared_settings: SharedSettings,
    reconnect_signal: ReconnectSignal,
) {
    let mut last_score_flush = Instant::now();
    let mut backoff_ms: u64 = config.backoff_initial_ms;

    loop {
        // Read current settings at the start of each cycle.
        let settings = shared_settings.lock().unwrap_or_else(|e| e.into_inner()).clone();

        // Backoff wait — wake immediately if settings change.
        tokio::select! {
            _ = sleep(Duration::from_millis(backoff_ms)) => {}
            _ = reconnect_signal.notified() => {
                backoff_ms = config.backoff_initial_ms;
            }
        }

        let candidates = tcp_open_ports_in_range(settings.port_start, settings.port_end, &config).await;
        if candidates.is_empty() {
            set_conn_state(&shared, &app, ConnState::Idle);
            backoff_ms = next_backoff(backoff_ms, config.backoff_max_ms);
            continue;
        }

        set_conn_state(&shared, &app, ConnState::Scanning);

        let mut connection: Option<(WaapiClient, Vec<SubscriptionHandle>)> = None;
        for port in candidates {
            if let Some(conn) = try_connect(port, &app, &shared, &config, &settings).await {
                connection = Some(conn);
                break;
            }
        }

        let (client, mut handles) = match connection {
            Some(c) => c,
            None => {
                set_conn_state(&shared, &app, ConnState::Idle);
                backoff_ms = next_backoff(backoff_ms, config.backoff_max_ms);
                continue;
            }
        };

        set_conn_state(&shared, &app, ConnState::Connected);
        backoff_ms = config.backoff_initial_ms;

        // Connected — poll until disconnected or settings changed.
        'connected: loop {
            tokio::select! {
                _ = sleep(Duration::from_millis(MONITOR_TICK_MS)) => {
                    let score_to_flush = {
                        let mut st = shared.lock().unwrap_or_else(|e| e.into_inner());
                        if st.score_dirty && last_score_flush.elapsed() >= Duration::from_secs(SCORE_FLUSH_INTERVAL_SECS) {
                            st.score_dirty = false;
                            Some(st.score.clone())
                        } else {
                            None
                        }
                    };
                    if let Some(snapshot) = score_to_flush {
                        let flush_path = data_path.clone();
                        let _ = tokio::task::spawn_blocking(move || save_score(&flush_path, &snapshot)).await;
                        last_score_flush = Instant::now();
                    }

                    let mut st = shared.lock().unwrap_or_else(|e| e.into_inner());
                    if st.recent_events > 0 {
                        st.recent_events = st.recent_events.saturating_sub(2);
                        emit_state(&app, &st);
                    }

                    if !client.is_connected() {
                        break 'connected;
                    }
                }
                _ = reconnect_signal.notified() => {
                    eprintln!("[wwise-rank] settings changed, reconnecting");
                    break 'connected;
                }
            }
        }

        handles.clear();
        let _ = timeout(config.disconnect_timeout, client.disconnect()).await;
        set_conn_state(&shared, &app, ConnState::Idle);
    }
}

// ── Tauri commands ─────────────────────────────────────────────────────────────

#[tauri::command]
fn get_score(state: tauri::State<SharedState>) -> ScorePayload {
    let st = match state.lock() { Ok(g) => g, Err(e) => e.into_inner() };
    ScorePayload {
        total_score: st.score.total_score,
        session_score: st.session_score,
        event_counts: st.score.event_counts.clone(),
        state: format!("{:?}", st.conn_state),
        recent_events: st.recent_events,
    }
}

#[tauri::command]
fn get_settings(settings: tauri::State<SharedSettings>) -> UserSettings {
    settings.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

#[tauri::command]
fn save_settings(
    new_settings: UserSettings,
    settings: tauri::State<SharedSettings>,
    reconnect: tauri::State<ReconnectSignal>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let path = data_dir.join("settings.json");
    {
        let mut lock = settings.lock().unwrap_or_else(|e| e.into_inner());
        *lock = new_settings;
        lock.save(&path);
    }
    reconnect.notify_one();
    Ok(())
}

#[tauri::command]
fn set_window_height(height: f64, app: tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        // Clear any minimum-size constraint first so the window can shrink.
        let _ = win.set_min_size(None::<LogicalSize<f64>>);
        let _ = win.set_size(LogicalSize::new(320.0_f64, height));
    }
}

#[tauri::command]
fn get_default_settings() -> UserSettings {
    UserSettings::default()
}

#[tauri::command]
fn get_autostart(app: tauri::AppHandle) -> Result<bool, String> {
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

#[tauri::command]
fn set_autostart(enabled: bool, app: tauri::AppHandle) -> Result<(), String> {
    let al = app.autolaunch();
    if enabled {
        al.enable().map_err(|e| e.to_string())
    } else {
        al.disable().map_err(|e| e.to_string())
    }
}

#[tauri::command]
fn reset_total_score(state: tauri::State<SharedState>, app: tauri::AppHandle) {
    let data_dir = match app.path().app_data_dir() {
        Ok(d) => d,
        Err(_) => return,
    };
    let path = data_dir.join("score.json");
    let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
    st.score.total_score = 0;
    st.score.event_counts.clear();
    st.score_dirty = false;
    save_score(&path, &st.score);
    emit_state(&app, &st);
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_load_from_env_and_defaults() {
        std::env::set_var("WWAAPI_TCP_PROBE_MS", "123");
        std::env::set_var("WWAAPI_CONNECT_SECS", "2");
        let c = Config::load_from_env();
        assert_eq!(c.tcp_probe_timeout.as_millis(), 123);
        assert_eq!(c.connect_timeout.as_secs(), 2);
        std::env::remove_var("WWAAPI_TCP_PROBE_MS");
        std::env::remove_var("WWAAPI_CONNECT_SECS");
    }

    #[test]
    fn backoff_computation() {
        assert_eq!(next_backoff(1000, 8000), 2000);
        assert_eq!(next_backoff(5000, 8000), 8000);
        assert_eq!(next_backoff(9000, 8000), 8000);
    }

    #[test]
    fn score_config_defaults_covers_all_topics() {
        let defaults = ScoreConfig::defaults();
        for &topic in TOPICS {
            assert!(defaults.contains_key(topic), "missing default score for '{topic}'");
        }
    }

    #[test]
    fn user_settings_default_covers_all_topics() {
        let s = UserSettings::default();
        for &topic in TOPICS {
            assert!(
                s.topics.iter().any(|t| t.uri == topic),
                "missing default topic setting for '{topic}'"
            );
        }
    }

    #[test]
    fn user_settings_load_or_default_merges_new_topics() {
        let tmp = std::env::temp_dir().join("test_settings.json");
        // Write a settings file that's missing some topics
        let partial = serde_json::json!({
            "port_start": 9090,
            "port_end": 9090,
            "topics": [
                { "uri": "ak.wwise.core.object.created", "score": 99, "enabled": true }
            ]
        });
        std::fs::write(&tmp, partial.to_string()).unwrap();
        let loaded = UserSettings::load_or_default(&tmp);
        assert_eq!(loaded.port_start, 9090);
        // Custom score preserved
        let created = loaded.topics.iter().find(|t| t.uri == "ak.wwise.core.object.created").unwrap();
        assert_eq!(created.score, 99);
        // Missing topics filled in
        assert!(loaded.topics.iter().any(|t| t.uri == ak::wwise::ui::SELECTION_CHANGED));
        std::fs::remove_file(&tmp).ok();
    }
}

// ── Entry point ────────────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::Builder::new().build())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let score_path = data_dir.join("score.json");
            let settings_path = data_dir.join("settings.json");

            let score = load_score(&score_path);
            let user_settings = UserSettings::load_or_default(&settings_path);
            // Write defaults on first run so the user can inspect the file.
            if !settings_path.exists() {
                user_settings.save(&settings_path);
            }

            let shared = Arc::new(Mutex::new(AppState {
                score,
                score_dirty: false,
                session_score: 0,
                conn_state: ConnState::Idle,
                recent_events: 0,
            }));

            let shared_settings = Arc::new(Mutex::new(user_settings));
            let reconnect_signal = Arc::new(Notify::new());

            app.manage(shared.clone());
            app.manage(shared_settings.clone());
            app.manage(reconnect_signal.clone());

            let config = Arc::new(Config::load_from_env());
            app.manage(config.clone());

            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(monitor_loop(
                shared.clone(),
                app_handle,
                score_path,
                config,
                shared_settings,
                reconnect_signal,
            ));

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_score,
            get_settings,
            get_default_settings,
            save_settings,
            reset_total_score,
            set_window_height,
            get_autostart,
            set_autostart,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
