use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout, Duration, Instant};
use waapi_rs::{ak, SubscriptionHandle, WaapiClient};

const MONITOR_TICK_MS: u64 = 1500;
const SCORE_FLUSH_INTERVAL_SECS: u64 = 3;
const BACKOFF_INITIAL_MS: u64 = 1500;
const BACKOFF_MAX_MS: u64 = 30_000;

/// Ports to probe for WAAPI (Wwise default is 8080, scan a few extras)
const WAAPI_SCAN_PORTS: &[u16] = &[8080, 8081, 8082, 8083];

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

// ── Score configuration ───────────────────────────────────────────────────────

/// Per-topic score values, loaded from `score_config.json` in the app data
/// directory. Missing keys fall back to the built-in defaults so users can
/// partially override the table.
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

    /// Load from `path`. On first run the file does not exist, so write the
    /// defaults there for the user to inspect and customise, then return them.
    /// If the file exists but is malformed, log a warning and return defaults.
    /// Any keys present in the file override their default; missing keys keep
    /// their default value.
    fn load_or_default(path: &PathBuf) -> Self {
        let defaults = Self::defaults();

        match std::fs::read_to_string(path) {
            Ok(content) => match serde_json::from_str::<HashMap<String, u64>>(&content) {
                Ok(mut loaded) => {
                    for (k, v) in &defaults {
                        loaded.entry(k.clone()).or_insert(*v);
                    }
                    ScoreConfig { scores: loaded }
                }
                Err(e) => {
                    eprintln!("[wwise-rank] failed to parse score_config.json: {e}, using defaults");
                    ScoreConfig { scores: defaults }
                }
            },
            Err(_) => {
                // First run – write defaults so the user can customise them
                if let Ok(data) = serde_json::to_string_pretty(&defaults) {
                    let _ = std::fs::write(path, data);
                }
                ScoreConfig { scores: defaults }
            }
        }
    }
}

// ── Subscribed topics ─────────────────────────────────────────────────────────

/// All WAAPI topics to subscribe to, in the order they will be attempted.
/// Scores are looked up at runtime from `ScoreConfig`.
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
    /// Per-topic event counts. Old `score.json` files that lack this field
    /// will deserialise with an empty map, preserving `total_score`.
    #[serde(default)]
    event_counts: HashMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
enum ConnState {
    Idle,
    Scanning,
    Connected,
}

/// Payload sent to the frontend via the "score-updated" event.
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
    session_score: u64, // resets each launch, not persisted
    conn_state: ConnState,
    recent_events: u32, // drives VU meter, decays over time
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

/// Ports that accept TCP on 127.0.0.1, in scan order (8080 first).
async fn tcp_open_waapi_ports(config: Arc<Config>) -> Vec<u16> {
    let mut join_handles = Vec::new();
    for &port in WAAPI_SCAN_PORTS {
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

    ports.sort_unstable_by_key(|p| WAAPI_SCAN_PORTS.iter().position(|x| x == p).unwrap_or(usize::MAX));
    ports
}

// ── Subscribe helper ──────────────────────────────────────────────────────────

/// Attempt to subscribe to a single topic. Returns the handle on success.
/// On failure, logs the error and returns `None` (subscription is optional).
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
        client.subscribe(topic, None, move |_, _| {
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

/// Attempts a full WAAPI handshake and subscriptions on `port`.
/// Returns `None` if this port is not WAAPI or no subscriptions succeed.
async fn try_connect(
    port: u16,
    app: &AppHandle,
    shared: &SharedState,
    config: &Arc<Config>,
    score_config: &Arc<ScoreConfig>,
) -> Option<(WaapiClient, Vec<SubscriptionHandle>)> {
    let url = format!("ws://127.0.0.1:{}/waapi", port);

    let client = match timeout(config.connect_timeout, WaapiClient::connect_with_url(&url)).await {
        Ok(Ok(c)) => c,
        _ => return None,
    };

    // Ping core to confirm the Authoring API is ready before subscribing.
    match timeout(config.getinfo_timeout, client.call(ak::wwise::core::GET_INFO, None, None)).await {
        Ok(Ok(_)) => {}
        _ => {
            let _ = timeout(config.disconnect_timeout, client.disconnect()).await;
            return None;
        }
    }

    let mut handles: Vec<SubscriptionHandle> = Vec::new();

    for &topic in TOPICS {
        let score = score_config.scores.get(topic).copied().unwrap_or(0);
        if let Some(h) = subscribe_one(
            &client,
            topic,
            score,
            Arc::clone(shared),  // SharedState = Arc<Mutex<AppState>>, clone is just Arc::clone
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
    score_config: Arc<ScoreConfig>,
) {
    let mut last_score_flush = Instant::now();
    let mut backoff_ms: u64 = config.backoff_initial_ms;

    loop {
        sleep(Duration::from_millis(backoff_ms)).await;

        // Probe TCP ports – skip scan entirely if nothing is listening.
        let candidates = tcp_open_waapi_ports(Arc::clone(&config)).await;
        if candidates.is_empty() {
            set_conn_state(&shared, &app, ConnState::Idle);
            backoff_ms = next_backoff(backoff_ms, config.backoff_max_ms);
            continue;
        }

        set_conn_state(&shared, &app, ConnState::Scanning);

        // Try each candidate port until one succeeds.
        let mut connection: Option<(WaapiClient, Vec<SubscriptionHandle>)> = None;
        for port in candidates {
            if let Some(conn) = try_connect(port, &app, &shared, &config, &score_config).await {
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

        // Connected – poll until the event loop dies.
        while client.is_connected() {
            sleep(Duration::from_millis(MONITOR_TICK_MS)).await;

            // Flush score to disk in batches.
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

            // Decay VU meter gradually.
            let mut st = shared.lock().unwrap_or_else(|e| e.into_inner());
            if st.recent_events > 0 {
                st.recent_events = st.recent_events.saturating_sub(2);
                emit_state(&app, &st);
            }
        }

        // Wwise closed / network dropped – clean up and retry.
        handles.clear();
        set_conn_state(&shared, &app, ConnState::Idle);
    }
}

// ── Tauri commands ─────────────────────────────────────────────────────────────

/// Called by the frontend on mount to get the initial state without waiting
/// for the first "score-updated" event.
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
    fn score_config_partial_override() {
        let tmp = std::env::temp_dir().join("test_score_config.json");
        let custom = serde_json::json!({"ak.wwise.core.object.created": 99});
        std::fs::write(&tmp, custom.to_string()).unwrap();
        let cfg = ScoreConfig::load_or_default(&tmp);
        assert_eq!(cfg.scores["ak.wwise.core.object.created"], 99);
        // defaults should fill in other keys
        assert_eq!(cfg.scores[ak::wwise::ui::SELECTION_CHANGED], 1);
        std::fs::remove_file(&tmp).ok();
    }
}

// ── Entry point ────────────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let score_path = data_dir.join("score.json");
            let score_config_path = data_dir.join("score_config.json");

            let score = load_score(&score_path);
            let score_config = Arc::new(ScoreConfig::load_or_default(&score_config_path));

            let shared = Arc::new(Mutex::new(AppState {
                score,
                score_dirty: false,
                session_score: 0,
                conn_state: ConnState::Idle,
                recent_events: 0,
            }));

            app.manage(shared.clone());

            let config = Arc::new(Config::load_from_env());
            app.manage(config.clone());

            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(monitor_loop(
                shared.clone(),
                app_handle,
                score_path,
                config,
                score_config,
            ));

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_score])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
