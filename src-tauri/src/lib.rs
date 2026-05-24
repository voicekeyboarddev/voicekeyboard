mod audio;
mod clipboard;
mod context;
mod gesture;
mod injection;
mod logging;
mod metrics;
mod model;
mod model_setup;
mod parser;
mod safety;
mod settings;
mod types;

use crate::{
    audio::{to_mono_16k, write_wav_16k, AudioManager},
    gesture::{GestureEvent, GestureHook, TriggerButton},
    injection::InjectionAbort,
    logging::AuditLogger,
    model::{InterpretationMode, ModelClient, RecentTextContext},
    safety::SafetyTier,
    settings::Settings,
    types::{
        Action, DiagnosticLogFile, FocusedTextContext, ModelInputSnapshot, PromptPanelKind,
        PromptPanelState, RecordingEntry, RequestLog, StatusSnapshot, WindowContext,
    },
};
use base64::{engine::general_purpose, Engine as _};
use chrono::Utc;
use parking_lot::Mutex;
use std::{
    collections::{HashSet, VecDeque},
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

const MAX_RECORDING_DURATION: Duration = Duration::from_secs(20);
const FINAL_STILLNESS_MS: u64 = 250;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum FeedbackLabel {
    Positive,
    Negative,
}

#[derive(Debug, Clone)]
struct FeedbackCandidate {
    recording: RecordingEntry,
    model_inputs: Vec<ModelInputSnapshot>,
    window: Option<WindowContext>,
}

struct RuntimeState {
    status: String,
    transcript: String,
    parsed_actions: Vec<Action>,
    current_window: Option<WindowContext>,
    pending_actions: Option<Vec<Action>>,
    pending_replacement_context: Option<FocusedTextContext>,
    request_logs: VecDeque<RequestLog>,
    model_inputs: VecDeque<ModelInputSnapshot>,
    recordings: VecDeque<RecordingEntry>,
    pending_feedback_recording: Option<RecordingEntry>,
    feedback_candidate: Option<FeedbackCandidate>,
    prompt_panel: Option<PromptPanelState>,
    next_recording_id: u64,
    metrics: types::SystemMetrics,
    measured_first_request: bool,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            status: "ready".to_string(),
            transcript: String::new(),
            parsed_actions: Vec::new(),
            current_window: None,
            pending_actions: None,
            pending_replacement_context: None,
            request_logs: VecDeque::new(),
            model_inputs: VecDeque::new(),
            recordings: VecDeque::new(),
            pending_feedback_recording: None,
            feedback_candidate: None,
            prompt_panel: None,
            next_recording_id: 1,
            metrics: types::SystemMetrics::default(),
            measured_first_request: false,
        }
    }
}

struct GestureSession {
    button: TriggerButton,
    start: Instant,
    start_x: f64,
    start_y: f64,
    last_x: f64,
    last_y: f64,
    last_move_at: Instant,
    listening: bool,
    context: Option<WindowContext>,
}

pub struct AppCore {
    app: Mutex<Option<tauri::AppHandle>>,
    settings: Mutex<Settings>,
    audio: AudioManager,
    gesture_hook: GestureHook,
    model: ModelClient,
    logger: AuditLogger,
    state: Mutex<RuntimeState>,
    gesture: Mutex<Option<GestureSession>>,
    abort: InjectionAbort,
    processing: AtomicBool,
    paused: AtomicBool,
    /// Time of the last mouse-button-down event (any button). Used for double-click abort.
    last_press: Mutex<Option<Instant>>,
}

impl AppCore {
    fn new() -> Arc<Self> {
        let settings = settings::load_settings();
        let logger = AuditLogger::new(settings.log_retention_bytes);
        Arc::new(Self {
            app: Mutex::new(None),
            audio: AudioManager::new(Duration::from_secs(settings.rolling_history_seconds)),
            settings: Mutex::new(settings),
            gesture_hook: GestureHook::new(),
            model: ModelClient::new(),
            logger,
            state: Mutex::new(RuntimeState::default()),
            gesture: Mutex::new(None),
            abort: InjectionAbort::new(),
            processing: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            last_press: Mutex::new(None),
        })
    }

    fn set_app(&self, app: tauri::AppHandle) {
        *self.app.lock() = Some(app);
    }

    fn snapshot(&self) -> StatusSnapshot {
        let state = self.state.lock();
        StatusSnapshot {
            status: state.status.clone(),
            hook_running: self.gesture_hook.is_enabled(),
            audio_running: self.audio.is_running(),
            paused: self.paused.load(Ordering::SeqCst),
            model_status: self.model.status(),
            current_window: state.current_window.clone(),
            transcript: state.transcript.clone(),
            parsed_actions: state.parsed_actions.clone(),
            pending_confirmation: state.pending_actions.is_some(),
            pending_text: state
                .pending_actions
                .as_ref()
                .map(|actions| text_from_actions(actions))
                .unwrap_or_default(),
            logs: self.logger.recent(),
            log_files: diagnostic_log_files(),
            request_logs: state.request_logs.iter().cloned().collect(),
            model_inputs: state.model_inputs.iter().cloned().collect(),
            recordings: state.recordings.iter().cloned().collect(),
            prompt_panel: state.prompt_panel.clone(),
            metrics: state.metrics.clone(),
            settings: self.settings.lock().clone(),
        }
    }

    fn emit_snapshot(&self) {
        if let Some(app) = self.app.lock().as_ref() {
            let _ = app.emit("status", self.snapshot());
        }
    }

    fn set_prompt_panel(&self, panel: Option<PromptPanelState>) {
        let has_panel = panel.is_some();
        self.state.lock().prompt_panel = panel;
        self.emit_snapshot();
        self.update_overlay(if has_panel { "prompt-panel" } else { "idle" }, None);
    }

    fn mutate_prompt_panel(&self, update: impl FnOnce(&mut PromptPanelState)) {
        {
            let mut state = self.state.lock();
            if let Some(panel) = state.prompt_panel.as_mut() {
                update(panel);
            }
        }
        self.emit_snapshot();
        self.update_overlay("prompt-panel", None);
    }

    fn set_status(&self, status: impl Into<String>) {
        self.state.lock().status = status.into();
        self.emit_snapshot();
    }

    fn log(&self, level: &str, message: impl Into<String>) {
        self.logger.log(level, message);
        self.emit_snapshot();
    }

    fn log_detected_context(&self, context: Option<&WindowContext>) {
        let label = model::context_kind_label(context);
        let detail = context
            .map(|window| {
                let focused = window.focused_text.as_ref();
                format!(
                    "detected context: {label}; app={}; title={}; element={}; control={}; class={}; automation_id={}; parent={}",
                    window.app_name,
                    window.title,
                    focused.and_then(|f| f.element_name.as_deref()).unwrap_or(""),
                    focused.and_then(|f| f.control_type.as_deref()).unwrap_or(""),
                    focused.and_then(|f| f.class_name.as_deref()).unwrap_or(""),
                    focused.and_then(|f| f.automation_id.as_deref()).unwrap_or(""),
                    focused.and_then(|f| f.parent_name.as_deref()).unwrap_or("")
                )
            })
            .unwrap_or_else(|| format!("detected context: {label}; no window context"));
        self.log("info", detail);
    }

    async fn warm_model_for_use(
        self: &Arc<Self>,
        settings: Settings,
        reason: &str,
    ) -> anyhow::Result<()> {
        if settings.model_path.trim().is_empty()
            || !std::path::Path::new(&settings.model_path).exists()
        {
            self.model.set_status("model setup required");
            self.set_status("model setup required");
            self.update_overlay("idle", None);
            anyhow::bail!("model file is not configured or does not exist");
        }

        self.set_status("warming");
        self.update_overlay(
            "processing",
            Some("Starting and warming local model".to_string()),
        );
        let started = Instant::now();
        self.log(
            "info",
            format!(
                "warming local model ({reason}); context length {} tokens",
                settings.context_length_tokens.clamp(2048, 32768)
            ),
        );
        self.model.ensure_running(&settings).await?;
        self.sample_metrics();
        self.update_overlay("idle", None);
        self.set_status("ready");
        self.log(
            "info",
            format!(
                "local model warmed in {:.1}s; resource usage sampled",
                started.elapsed().as_secs_f64()
            ),
        );
        Ok(())
    }

    fn save_feedback(
        &self,
        label: FeedbackLabel,
        recording_id: Option<u64>,
        expected_output: Option<String>,
    ) -> anyhow::Result<PathBuf> {
        let candidate = {
            let state = self.state.lock();
            if let Some(id) = recording_id {
                let recording = state
                    .recordings
                    .iter()
                    .find(|r| r.id == id)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("recording {} not found", id))?;
                FeedbackCandidate {
                    recording,
                    model_inputs: state.model_inputs.iter().cloned().collect(),
                    window: state.current_window.clone(),
                }
            } else if let Some(candidate) = state.feedback_candidate.clone() {
                candidate
            } else {
                let recording = state
                    .recordings
                    .front()
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("no completed recording to save"))?;
                FeedbackCandidate {
                    recording,
                    model_inputs: state.model_inputs.iter().cloned().collect(),
                    window: state.current_window.clone(),
                }
            }
        };

        let path = write_feedback_example(&candidate, &label, expected_output)?;
        self.log(
            "info",
            format!("saved {:?} feedback example to {}", label, path.display()),
        );
        Ok(path)
    }

    fn push_request_log(&self, entry: RequestLog) {
        self.logger.log(
            if entry.ok { "info" } else { "error" },
            format!(
                "request {}: ok={} ttft={:?}ms tps={:?} transcript={:?} output={:?}",
                entry.stage,
                entry.ok,
                entry.ttft_ms,
                entry.tokens_per_second,
                entry.transcript,
                entry.output
            ),
        );
        let mut state = self.state.lock();
        state.request_logs.push_front(entry);
        while state.request_logs.len() > 80 {
            state.request_logs.pop_back();
        }
        drop(state);
        self.emit_snapshot();
    }

    fn push_model_input(&self, entry: ModelInputSnapshot) {
        let mut state = self.state.lock();
        state.model_inputs.push_front(entry);
        while state.model_inputs.len() > 12 {
            state.model_inputs.pop_back();
        }
        drop(state);
        self.emit_snapshot();
    }

    fn recent_text_context(
        &self,
        settings: &Settings,
        current_transcript: &str,
        max_items_override: Option<usize>,
        context: Option<&WindowContext>,
    ) -> Vec<RecentTextContext> {
        if !settings.recent_context_enabled {
            return Vec::new();
        }

        let now = Utc::now();
        let window_cutoff =
            now - chrono::Duration::seconds(settings.recent_context_window_seconds as i64);
        let request_cap = settings.recent_context_max_requests.max(1);
        let has_selection = context_has_selection(context);
        let item_cap = max_items_override
            .unwrap_or(settings.recent_context_max_items)
            .max(1)
            .min(if has_selection { 2 } else { usize::MAX });

        let logs = self
            .state
            .lock()
            .request_logs
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let mut seen = HashSet::new();
        let mut preferred = Vec::new();
        let mut fallback = Vec::new();

        for (index, log) in logs.into_iter().enumerate() {
            if !log.ok {
                continue;
            }
            if log.transcript.trim().is_empty() && log.output.trim().is_empty() {
                continue;
            }
            if log.stage == "transcription" && log.output.trim() == current_transcript.trim() {
                continue;
            }
            if is_recent_command_noise(&log) {
                continue;
            }

            let within_request_cap = index < request_cap;
            let parsed_ts = chrono::DateTime::parse_from_rfc3339(&log.ts)
                .ok()
                .map(|ts| ts.with_timezone(&Utc));
            let within_time_window = parsed_ts.map(|ts| ts >= window_cutoff).unwrap_or(false);
            if !within_request_cap && !within_time_window {
                continue;
            }

            let age_seconds = parsed_ts
                .map(|ts| now.signed_duration_since(ts).num_seconds().max(0) as u64)
                .unwrap_or(0);
            let item = RecentTextContext {
                stage: log.stage.clone(),
                transcript: log.transcript.trim().to_string(),
                output: log.output.trim().to_string(),
                age_seconds,
            };
            let key = format!("{}|{}|{}", item.stage, item.transcript, item.output);
            if !seen.insert(key) {
                continue;
            }

            if has_selection && !is_continuity_worthy(&item) {
                continue;
            }

            if is_continuity_worthy(&item) {
                preferred.push(item);
            } else {
                fallback.push(item);
            }
        }

        preferred.extend(fallback);
        preferred.truncate(item_cap);
        preferred
    }

    fn push_recording(&self, mut entry: RecordingEntry) -> RecordingEntry {
        let mut state = self.state.lock();
        entry.id = state.next_recording_id;
        state.next_recording_id += 1;
        state.recordings.push_front(entry);
        while state.recordings.len() > 10 {
            if let Some(old) = state.recordings.pop_back() {
                let _ = std::fs::remove_file(old.audio_path);
            }
        }
        let saved = state
            .recordings
            .front()
            .cloned()
            .expect("recording was just saved");
        drop(state);
        self.emit_snapshot();
        saved
    }

    fn sample_metrics(&self) {
        let metrics = metrics::collect(self.model.server_pid());
        self.state.lock().metrics = metrics;
        self.emit_snapshot();
    }

    fn sample_first_request_metrics(&self) {
        let should_sample = {
            let mut state = self.state.lock();
            if state.measured_first_request {
                false
            } else {
                state.measured_first_request = true;
                true
            }
        };
        if should_sample {
            self.sample_metrics();
            self.log("info", "resource usage sampled after first model request");
        }
    }

    fn start_workers(self: &Arc<Self>) -> anyhow::Result<()> {
        let settings = self.settings.lock().clone();
        self.audio
            .buffer()
            .set_max_history(Duration::from_secs(settings.rolling_history_seconds));
        let settings = self.settings.lock().clone();
        self.audio.start(&settings.microphone_device)?;
        self.log("info", format!("audio started: {}", self.audio.status()));

        let (tx, rx) = mpsc::channel();
        let core_for_capture = Arc::clone(self);
        let capture: gesture::ContextCapture = Arc::new(move |x: i32, y: i32| {
            let settings = core_for_capture.settings.lock().clone();
            if !settings.context_enabled {
                return None;
            }
            let context = context::window_context_at_point_with_settings(x, y, &settings);
            if is_voice_keyboard_window(context.as_ref()) {
                core_for_capture.update_overlay("idle", None);
                return None;
            }
            context
        });
        self.gesture_hook.start(tx, capture)?;
        self.log("info", "global mouse/keyboard hook enabled");

        let core = Arc::clone(self);
        std::thread::Builder::new()
            .name("voice-keyboard-controller".to_string())
            .spawn(move || {
                while let Ok(event) = rx.recv() {
                    core.handle_gesture_event(event);
                }
            })?;

        let core = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            let settings = core.settings.lock().clone();
            match core.warm_model_for_use(settings, "startup").await {
                Ok(_) => {}
                Err(err) => {
                    core.update_overlay("idle", None);
                    if core.model.status() != "model setup required" {
                        core.set_status("error");
                    }
                    core.log("error", format!("model warm-up failed: {err}"));
                }
            }
            core.emit_snapshot();
        });

        Ok(())
    }

    fn stop_hook(&self) {
        self.gesture_hook.stop();
        self.set_status("hook stopped");
        self.log("info", "global hook disabled");
    }

    fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
        if paused {
            self.gesture.lock().take();
            self.update_overlay("idle", None);
            self.set_status("paused");
            self.log("info", "recording trigger paused");
        } else {
            self.set_status("ready");
            self.log("info", "recording trigger resumed");
        }
    }

    async fn shutdown(&self) {
        self.abort.abort();
        self.gesture.lock().take();
        self.gesture_hook.stop();
        self.audio.stop();
        let settings = self.settings.lock().clone();
        self.model.shutdown(&settings).await;
        self.update_overlay("idle", None);
        self.log("info", "application shutdown requested");
        let app = self.app.lock().clone();
        if let Some(app) = app {
            app.exit(0);
        }
    }

    fn handle_gesture_event(self: &Arc<Self>, event: GestureEvent) {
        if self.paused.load(Ordering::SeqCst) {
            if matches!(event, GestureEvent::Escape) {
                self.abort.abort();
                self.log("warn", "Escape kill switch requested while paused");
            }
            return;
        }
        match event {
            GestureEvent::Down {
                button,
                at,
                x,
                y,
                context,
            } => self.pointer_down(button, at, x, y, context),
            GestureEvent::LeftMove { at, x, y } => self.left_move(at, x, y),
            GestureEvent::Up { button, at, x, y } => self.pointer_up(button, at, x, y),
            GestureEvent::Escape => {
                self.abort.abort();
                self.log("warn", "Escape kill switch requested");
            }
        }
    }

    fn pointer_down(
        self: &Arc<Self>,
        button: TriggerButton,
        at: Instant,
        x: f64,
        y: f64,
        prefetched_context: Option<WindowContext>,
    ) {
        if is_voice_keyboard_window(prefetched_context.as_ref())
            || context::active_window_context()
                .as_ref()
                .is_some_and(|context| is_voice_keyboard_window(Some(context)))
        {
            self.gesture.lock().take();
            self.update_overlay("idle", None);
            self.log("debug", "ignored mouse press inside Voice Keyboard UI");
            return;
        }

        // Double-click anywhere abort: two presses (any button) within 400 ms while the
        // model is busy = bail out. Mirrors the right-click abort but fires for left-click
        // double-clicks too, so users can interrupt regardless of which button they prefer.
        let mut last_press = self.last_press.lock();
        let is_double_click = last_press
            .map(|prev| at.saturating_duration_since(prev).as_millis() < 400)
            .unwrap_or(false);
        *last_press = Some(at);
        drop(last_press);

        if is_double_click {
            let status = self.state.lock().status.clone();
            if matches!(
                status.as_str(),
                "listening" | "transcribing" | "interpreting" | "injecting" | "processing"
            ) {
                self.abort.abort();
                self.gesture.lock().take();
                self.set_status("aborted");
                self.update_overlay("idle", None);
                self.log("info", "double-click abort");
                if status == "injecting" {
                    std::thread::spawn(|| {
                        std::thread::sleep(Duration::from_millis(150));
                        let _ = injection::send_undo();
                    });
                }
                return;
            }
        }

        if button == TriggerButton::Right {
            let status = self.state.lock().status.clone();
            if matches!(
                status.as_str(),
                "listening" | "transcribing" | "interpreting" | "injecting"
            ) {
                self.abort.abort();
                self.gesture.lock().take();
                self.set_status("aborted");
                self.log("info", "right-click abort");
                if status == "injecting" {
                    std::thread::spawn(|| {
                        std::thread::sleep(Duration::from_millis(150));
                        let _ = injection::send_undo();
                    });
                }
                return;
            }
            if !self.settings.lock().right_click_trigger_enabled {
                return;
            }
        }
        // If the previous segment is still being processed, ignore new presses entirely so
        // we don't leave a stale gesture session behind that gets stuck in "listening".
        if self.processing.load(Ordering::SeqCst) {
            self.log(
                "debug",
                "ignored mouse press while a previous segment is still processing",
            );
            return;
        }
        let model_status = self.model.status();
        if model_status != "warm" {
            self.log(
                "debug",
                format!("ignored mouse press while model is not ready ({model_status})"),
            );
            return;
        }
        *self.gesture.lock() = Some(GestureSession {
            button,
            start: at,
            start_x: x,
            start_y: y,
            last_x: x,
            last_y: y,
            last_move_at: at,
            listening: false,
            // Prefer the context captured inside the rdev hook callback (which fires before
            // the click is delivered to the target window and therefore still sees any
            // selection). Only fall back to a fresh capture if the prefetched one is missing.
            context: if self.settings.lock().context_enabled {
                let settings = self.settings.lock().clone();
                prefetched_context.or_else(|| {
                    context::window_context_at_point_with_settings(
                        x.round() as i32,
                        y.round() as i32,
                        &settings,
                    )
                })
            } else {
                None
            },
        });

        let release_watcher = Arc::clone(self);
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(20));
            let still_tracking = release_watcher
                .gesture
                .lock()
                .as_ref()
                .map(|session| session.start == at && session.button == button)
                .unwrap_or(false);
            if !still_tracking {
                break;
            }
            if !trigger_button_down(button) {
                let (x, y) = context::cursor_position()
                    .map(|(x, y)| (x as f64, y as f64))
                    .unwrap_or((0.0, 0.0));
                release_watcher.log(
                    "debug",
                    "trigger button release observed by physical button watcher",
                );
                release_watcher.pointer_up(button, Instant::now(), x, y);
                break;
            }
        });

        let core = Arc::clone(self);
        let hold = self.settings.lock().trigger_hold_ms;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(hold));
            let mut gesture = core.gesture.lock();
            if let Some(session) = gesture.as_mut() {
                if session.start == at && !session.listening {
                    if !trigger_button_down(session.button) {
                        gesture.take();
                        drop(gesture);
                        core.set_status("ready");
                        core.update_overlay("idle", None);
                        core.log(
                            "debug",
                            "ignored hold timer because trigger button is no longer down",
                        );
                        return;
                    }
                    session.listening = true;
                    let start = session.start;
                    let button = session.button;
                    drop(gesture);
                    core.set_status("listening");
                    core.update_overlay("recording", None);
                    // Keep the target app focused while the mouse is down. In terminals,
                    // showing an overlay can cancel drag selection.
                    let clipboard_core = Arc::clone(&core);
                    std::thread::spawn(move || {
                        let probe_skip_reason = clipboard_core
                            .gesture
                            .lock()
                            .as_ref()
                            .and_then(|session| {
                                (session.start == start)
                                    .then(|| clipboard_probe_skip_reason(session.context.as_ref()))
                            })
                            .flatten();
                        if let Some(reason) = probe_skip_reason {
                            clipboard_core.log(
                                "debug",
                                format!("skipped clipboard selection probe: {reason}"),
                            );
                            return;
                        }
                        capture_selection_with_clipboard_probe(&clipboard_core, start);
                    });
                    let watchdog = Arc::clone(&core);
                    std::thread::spawn(move || {
                        std::thread::sleep(MAX_RECORDING_DURATION);
                        let should_stop = watchdog
                            .gesture
                            .lock()
                            .as_ref()
                            .map(|session| {
                                session.start == start
                                    && session.button == button
                                    && session.listening
                            })
                            .unwrap_or(false);
                        if should_stop {
                            let (x, y) = context::cursor_position()
                                .map(|(x, y)| (x as f64, y as f64))
                                .unwrap_or((0.0, 0.0));
                            watchdog.log("warn", "recording auto-stopped after max duration");
                            watchdog.pointer_up(button, Instant::now(), x, y);
                        }
                    });
                }
            }
        });
    }

    fn left_move(&self, at: Instant, x: f64, y: f64) {
        if let Some(session) = self.gesture.lock().as_mut() {
            session.last_x = x;
            session.last_y = y;
            session.last_move_at = at;
        }
    }

    fn pointer_up(self: &Arc<Self>, button: TriggerButton, at: Instant, x: f64, y: f64) {
        let Some(session) = self.gesture.lock().take() else {
            return;
        };
        if session.button != button {
            *self.gesture.lock() = Some(session);
            return;
        }
        let settings = self.settings.lock().clone();
        let duration_ms = at.saturating_duration_since(session.start).as_millis() as u64;
        let moved = ((x - session.start_x).powi(2) + (y - session.start_y).powi(2)).sqrt();
        let stable_ms = at
            .saturating_duration_since(session.last_move_at)
            .as_millis() as u64;
        if duration_ms < settings.trigger_hold_ms {
            self.log(
                "debug",
                format!("ignored short mouse gesture ({duration_ms} ms, {moved:.1}px)"),
            );
            self.set_status("ready");
            return;
        }
        if stable_ms < FINAL_STILLNESS_MS {
            self.log(
                "debug",
                format!(
                    "ignored held gesture because pointer was still moving {stable_ms} ms before release"
                ),
            );
            self.set_status("ready");
            self.update_overlay("idle", None);
            return;
        }
        if session.button == TriggerButton::Right && session.listening {
            injection::dismiss_context_menu_soon();
        }
        if self.processing.swap(true, Ordering::SeqCst) {
            self.log(
                "warn",
                "ignored gesture while another segment is processing",
            );
            // Recovery path: if the hold-timer thread already flipped status to "listening"
            // while a previous segment was running, reset the UI back to ready so the user
            // doesn't see the app stuck recording forever.
            self.set_status("ready");
            self.update_overlay("idle", None);
            return;
        }

        self.update_overlay("processing", None);

        let core = Arc::clone(self);
        let initial_context = session.context.clone();
        tauri::async_runtime::spawn(async move {
            core.clone()
                .process_segment(session.start, at, settings, initial_context)
                .await;
            core.processing.store(false, Ordering::SeqCst);
            core.emit_snapshot();
        });
    }

    async fn process_segment(
        self: Arc<Self>,
        start: Instant,
        end: Instant,
        settings: Settings,
        initial_context: Option<WindowContext>,
    ) {
        self.set_status("processing");
        let pre_roll = Duration::from_millis(settings.pre_roll_ms);
        let post_roll = Duration::from_millis(settings.post_roll_ms);
        // Wait for post_roll_ms of audio AFTER the trigger release before extracting.
        // This adds directly to perceived latency (no way to add tail audio without
        // recording it first) — keep the default small. Captures the last syllable
        // when users release the trigger fractionally early.
        if post_roll > Duration::ZERO {
            tokio::time::sleep(post_roll).await;
        }
        let segment = self
            .audio
            .buffer()
            .extract(start - pre_roll, end + post_roll);
        let samples = to_mono_16k(&segment);
        let audio_duration_ms = (samples.len() as u64 * 1000) / audio::TARGET_SAMPLE_RATE as u64;
        let level = audio::max_window_rms(&samples, 160);
        if samples.is_empty() || level < settings.vad_rms_threshold {
            self.set_status("ready");
            self.update_overlay("idle", None);
            self.log(
                "info",
                format!(
                    "discarded audio: no speech detected (peak rms {level:.4}, threshold {:.4})",
                    settings.vad_rms_threshold
                ),
            );
            return;
        }

        let wav_path = temp_wav_path();
        if let Err(err) = write_wav_16k(&wav_path, &samples) {
            self.set_status("error");
            self.log("error", format!("failed to write wav: {err}"));
            return;
        }

        let context = if settings.context_enabled {
            initial_context
                .or_else(|| context::active_window_context_with_screenshot_settings(&settings))
        } else {
            None
        };
        self.log_detected_context(context.as_ref());
        self.state.lock().current_window = context.clone();
        self.emit_snapshot();
        self.update_overlay("processing", None);
        self.push_model_input(ModelInputSnapshot {
            ts: Utc::now().to_rfc3339(),
            stage: "transcription".to_string(),
            endpoint: format!(
                "{}/v1/chat/completions",
                settings.server_url.trim_end_matches('/')
            ),
            prompt: model::transcription_prompt(context.as_ref(), &settings.spoken_languages),
            image_attached: false,
            reasoning_mode: Some("off".to_string()),
            reasoning_budget: None,
            context: context.clone(),
            audio_path: Some(wav_path.to_string_lossy().to_string()),
            audio_duration_ms: Some(audio_duration_ms),
            audio_format: Some("wav 16kHz mono pcm_s16le".to_string()),
        });

        let transcript_response = match self
            .model
            .transcribe(&settings, &wav_path, context.as_ref())
            .await
        {
            Ok(response) => response,
            Err(err) => {
                self.set_status("error");
                self.update_overlay("error", Some(short_overlay_error("Transcription failed")));
                self.push_request_log(RequestLog {
                    ts: Utc::now().to_rfc3339(),
                    stage: "transcription".to_string(),
                    ok: false,
                    transcript: String::new(),
                    output: err.to_string(),
                    ttft_ms: None,
                    tokens_per_second: None,
                    total_ms: None,
                });
                self.log("error", format!("transcription failed: {err}"));
                return;
            }
        };
        let transcript = transcript_response.content.clone();
        self.push_request_log(RequestLog {
            ts: Utc::now().to_rfc3339(),
            stage: "transcription".to_string(),
            ok: true,
            transcript: transcript.clone(),
            output: transcript.clone(),
            ttft_ms: transcript_response.ttft_ms,
            tokens_per_second: transcript_response.tokens_per_second,
            total_ms: transcript_response.total_ms,
        });
        self.sample_first_request_metrics();

        self.state.lock().transcript = transcript.clone();
        self.emit_snapshot();
        self.update_overlay("transcript", Some(transcript.clone()));

        let replacement_context = preserved_selection_context(context.as_ref());
        let prepared_transcript =
            normalize_transcript_for_interpretation(&transcript, context.as_ref());
        if prepared_transcript != transcript {
            self.log(
                "info",
                format!(
                    "normalized transcript for interpretation: {:?} -> {:?}",
                    transcript, prepared_transcript
                ),
            );
        }
        let interpretation_mode =
            if should_use_thinking_handoff(&settings, &prepared_transcript, context.as_ref()) {
                InterpretationMode::Thinking
            } else {
                InterpretationMode::Fast
            };
        let recent_context = self.recent_text_context(
            &settings,
            &prepared_transcript,
            Some(match interpretation_mode {
                InterpretationMode::Fast => settings.recent_context_max_items,
                InterpretationMode::Thinking => settings.thinking_handoff_context_items,
            }),
            context.as_ref(),
        );
        // Live chunk injection can type partial model deltas into the target app before
        // the final parsed action is known. Buffer the full interpretation first so
        // normal dictation is injected exactly once.
        let stream_live_text = false;
        let streaming_cursor = injection::begin_streaming_injection();
        self.abort.reset();
        let abort = self.abort.clone();
        let streaming_settings = settings.clone();
        if interpretation_mode == InterpretationMode::Thinking {
            self.set_status("specialized-agent");
            self.update_overlay(
                "processing",
                Some("Passing task to specialized agent".to_string()),
            );
            self.log(
                "info",
                format!(
                    "passing task to specialized agent with reasoning=on budget={}",
                    settings.thinking_handoff_reasoning_budget
                ),
            );
        } else {
            self.set_status("interpreting");
            self.update_overlay("processing", Some("Interpreting transcript".to_string()));
        }
        let interpretation_prompt = match interpretation_mode {
            InterpretationMode::Fast => model::interpretation_prompt(
                &prepared_transcript,
                context.as_ref(),
                false,
                &settings.common_terms,
                &settings.spoken_languages,
                &recent_context,
            ),
            InterpretationMode::Thinking => model::thinking_interpretation_prompt(
                &prepared_transcript,
                context.as_ref(),
                false,
                &settings.common_terms,
                &settings.spoken_languages,
                &recent_context,
            ),
        };
        let direct_override = if interpretation_mode == InterpretationMode::Fast {
            direct_interpretation_override(&prepared_transcript, context.as_ref())
        } else {
            None
        };
        let model_was_called = direct_override.is_none();
        let interpreted_result = if let Some(output) = direct_override {
            self.log(
                "info",
                format!(
                    "used direct interpretation override for {:?}",
                    prepared_transcript
                ),
            );
            Ok(model::StreamingInterpretation {
                response: model::ModelResponse {
                    content: output,
                    ttft_ms: Some(0.0),
                    tokens_per_second: None,
                    total_ms: Some(0.0),
                },
                streamed_text: String::new(),
                used_fallback_prompt: false,
                image_attached: false,
                prompt: String::new(),
            })
        } else {
            self.model
                .interpret_streaming_text(
                    &settings,
                    &prepared_transcript,
                    context.as_ref(),
                    &recent_context,
                    interpretation_mode,
                    |text| {
                        if stream_live_text {
                            injection::inject_text_chunk(
                                text,
                                &streaming_settings,
                                &abort,
                                streaming_cursor,
                            )
                        } else {
                            Ok(())
                        }
                    },
                )
                .await
        };
        if model_was_called {
            if let Ok(interpreted) = interpreted_result.as_ref() {
                self.push_model_input(ModelInputSnapshot {
                    ts: Utc::now().to_rfc3339(),
                    stage: interpretation_mode.stage_name().to_string(),
                    endpoint: format!(
                        "{}/v1/chat/completions",
                        settings.server_url.trim_end_matches('/')
                    ),
                    prompt: if interpreted.prompt.is_empty() {
                        interpretation_prompt
                    } else {
                        interpreted.prompt.clone()
                    },
                    image_attached: interpreted.image_attached,
                    reasoning_mode: Some(if interpretation_mode == InterpretationMode::Thinking {
                        "on".to_string()
                    } else {
                        "off".to_string()
                    }),
                    reasoning_budget: if interpretation_mode == InterpretationMode::Thinking {
                        Some(settings.thinking_handoff_reasoning_budget)
                    } else {
                        None
                    },
                    context: context.clone(),
                    audio_path: None,
                    audio_duration_ms: None,
                    audio_format: None,
                });
            }
        }
        let (interpreted_result, interpretation_ok) = match interpreted_result {
            Ok(result) => {
                if result.used_fallback_prompt {
                    self.log(
                        "warn",
                        "interpretation succeeded only after retrying with the legacy fallback prompt",
                    );
                }
                (result, true)
            }
            Err(err) => {
                self.model.set_status("warm");
                if is_context_length_error(&err) {
                    self.set_status("error");
                    self.update_overlay(
                        "error",
                        Some(short_overlay_error("Model context exceeded")),
                    );
                    self.push_request_log(RequestLog {
                        ts: Utc::now().to_rfc3339(),
                        stage: interpretation_mode.stage_name().to_string(),
                        ok: false,
                        transcript: transcript.clone(),
                        output: format!("context exceeded: {err}"),
                        ttft_ms: None,
                        tokens_per_second: None,
                        total_ms: None,
                    });
                    self.log("error", format!("model context exceeded: {err}"));
                    return;
                }
                self.log(
                    "warn",
                    format!(
                        "streaming interpretation failed, falling back to transcript text: {err}"
                    ),
                );
                (
                    model::StreamingInterpretation {
                        response: model::ModelResponse {
                            content: transcript.clone(),
                            ttft_ms: None,
                            tokens_per_second: None,
                            total_ms: None,
                        },
                        streamed_text: String::new(),
                        used_fallback_prompt: false,
                        image_attached: false,
                        prompt: String::new(),
                    },
                    false,
                )
            }
        };
        let interpreted_response = interpreted_result.response;
        let streamed_text = if stream_live_text {
            interpreted_result.streamed_text
        } else {
            String::new()
        };
        let interpreted = interpreted_response.content.clone();
        self.push_request_log(RequestLog {
            ts: Utc::now().to_rfc3339(),
            stage: interpretation_mode.stage_name().to_string(),
            ok: interpretation_ok,
            transcript: transcript.clone(),
            output: interpreted.clone(),
            ttft_ms: interpreted_response.ttft_ms,
            tokens_per_second: interpreted_response.tokens_per_second,
            total_ms: interpreted_response.total_ms,
        });
        self.sample_first_request_metrics();
        let parsed = parser::parse_output(&interpreted, settings.shortcuts_enabled);
        if parsed.actions.is_empty() {
            self.set_status("ready");
            self.update_overlay("idle", None);
            self.log("warn", "model produced no usable actions");
            return;
        }

        self.state.lock().parsed_actions = parsed.actions.clone();
        let recording = self.push_recording(RecordingEntry {
            id: 0,
            ts: Utc::now().to_rfc3339(),
            audio_duration_ms,
            audio_path: wav_path.to_string_lossy().to_string(),
            transcript: transcript.clone(),
            output: interpreted.clone(),
            actions: parsed.actions.clone(),
            transcription_ttft_ms: transcript_response.ttft_ms,
            interpretation_ttft_ms: interpreted_response.ttft_ms,
            transcription_total_ms: transcript_response.total_ms,
            interpretation_total_ms: interpreted_response.total_ms,
            context: context.clone(),
        });
        self.state.lock().pending_feedback_recording = Some(recording.clone());
        self.emit_snapshot();

        if parsed.actions.len() == 1 && matches!(parsed.actions[0], Action::Prompt) {
            self.clone()
                .handle_prompt_handoff(
                    transcript,
                    interpreted,
                    audio_duration_ms,
                    wav_path,
                    context,
                    replacement_context,
                    recent_context,
                    recording.id,
                    settings,
                )
                .await;
            return;
        }

        if parsed.actions.len() == 1 && matches!(parsed.actions[0], Action::Agentic) {
            self.show_agentic_placeholder(recording.id, transcript, interpreted);
            self.state.lock().pending_feedback_recording = None;
            self.set_status("ready");
            return;
        }

        let decision = if copy_shortcut_needs_confirmation(&parsed.actions, context.as_ref()) {
            safety::SafetyDecision {
                tier: SafetyTier::Confirm,
                reason: "copy shortcut in a non-text context without readable selected text"
                    .to_string(),
            }
        } else {
            safety::evaluate(&parsed.actions, &settings)
        };
        match decision.tier {
            SafetyTier::Block => {
                self.state.lock().pending_feedback_recording = None;
                self.set_status("blocked");
                self.update_overlay("blocked", Some(decision.reason.clone()));
                self.log("warn", format!("blocked actions: {}", decision.reason));
            }
            SafetyTier::Confirm => {
                let mut state = self.state.lock();
                state.pending_actions = Some(parsed.actions);
                state.pending_replacement_context = replacement_context;
                drop(state);
                self.set_status("confirm");
                self.update_overlay("confirm", Some(decision.reason.clone()));
                self.log(
                    "warn",
                    format!("confirmation required: {}", decision.reason),
                );
            }
            SafetyTier::Allow => {
                if !streamed_text.is_empty() {
                    let remaining = actions_after_streamed_text(&parsed.actions, &streamed_text);
                    if remaining.is_empty() {
                        self.set_status("ready");
                        self.update_overlay(
                            "done",
                            Some("Streamed text into the active app".to_string()),
                        );
                        self.log("info", "streamed text action while JSON was arriving");
                    } else {
                        self.inject_actions(remaining, settings).await;
                    }
                } else {
                    self.inject_actions_with_context(parsed.actions, settings, replacement_context)
                        .await;
                }
            }
        }
    }

    async fn inject_actions(self: &Arc<Self>, actions: Vec<Action>, settings: Settings) {
        self.inject_actions_with_context(actions, settings, None)
            .await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_prompt_handoff(
        self: Arc<Self>,
        transcript: String,
        source_output: String,
        audio_duration_ms: u64,
        wav_path: PathBuf,
        context: Option<WindowContext>,
        replacement_context: Option<FocusedTextContext>,
        recent_context: Vec<RecentTextContext>,
        recording_id: u64,
        settings: Settings,
    ) {
        self.set_status("specialized-agent");
        self.set_prompt_panel(Some(PromptPanelState {
            kind: PromptPanelKind::Prompt,
            state: "streaming".to_string(),
            title: "Prompt handoff".to_string(),
            transcript: transcript.clone(),
            source_output: source_output.clone(),
            text: String::new(),
            delivery: None,
            recording_id: Some(recording_id),
            can_insert: false,
            can_save_wrong: false,
            collapsed: false,
            error: None,
        }));
        self.log("info", "prompt handoff started");
        let prompt_provider = settings.prompt_provider.trim().to_ascii_lowercase();
        let use_custom_prompt = prompt_provider == "custom" || prompt_provider == "openai";
        let prompt_endpoint = if use_custom_prompt && !settings.prompt_endpoint_url.trim().is_empty()
        {
            format!(
                "{}/v1/chat/completions",
                settings.prompt_endpoint_url.trim_end_matches('/')
            )
        } else {
            format!("{}/v1/chat/completions", settings.server_url.trim_end_matches('/'))
        };
        self.push_model_input(ModelInputSnapshot {
            ts: Utc::now().to_rfc3339(),
            stage: "prompt-handoff".to_string(),
            endpoint: prompt_endpoint,
            prompt: model::prompt_handoff_prompt(&transcript, context.as_ref(), &recent_context),
            image_attached: context
                .as_ref()
                .and_then(|ctx| ctx.cursor_screenshot.as_ref())
                .is_some(),
            reasoning_mode: Some(if use_custom_prompt { "provider".to_string() } else { "on".to_string() }),
            reasoning_budget: if use_custom_prompt {
                None
            } else {
                Some(settings.thinking_handoff_reasoning_budget)
            },
            context: context.clone(),
            audio_path: Some(wav_path.to_string_lossy().to_string()),
            audio_duration_ms: Some(audio_duration_ms),
            audio_format: Some("wav 16kHz mono pcm_s16le".to_string()),
        });

        let stream_core = self.clone();
        let response = self
            .model
            .prompt_handoff(
                &settings,
                &transcript,
                context.as_ref(),
                Some(&wav_path),
                &recent_context,
                move |chunk| {
                    stream_core.mutate_prompt_panel(|panel| {
                        panel.text.push_str(chunk);
                    });
                    Ok(())
                },
            )
            .await;

        match response {
            Ok(result) => {
                if result.used_media_fallback {
                    self.log("warn", "prompt handoff retried without audio/image media");
                }
                self.push_request_log(RequestLog {
                    ts: Utc::now().to_rfc3339(),
                    stage: "prompt-handoff".to_string(),
                    ok: true,
                    transcript: transcript.clone(),
                    output: result.text.clone(),
                    ttft_ms: result.response.ttft_ms,
                    tokens_per_second: result.response.tokens_per_second,
                    total_ms: result.response.total_ms,
                });
                self.mutate_prompt_panel(|panel| {
                    panel.state = "done".to_string();
                    panel.text = result.text.clone();
                    panel.delivery = Some(result.delivery.clone());
                    panel.can_insert = result.delivery == "keyboard";
                });

                if result.delivery == "keyboard" && settings.prompt_auto_inject_keyboard {
                    if target_still_focused(context.as_ref()) {
                        self.inject_actions_with_context(
                            vec![Action::Text {
                                value: result.text.clone(),
                            }],
                            settings,
                            replacement_context,
                        )
                        .await;
                    } else {
                        self.mutate_prompt_panel(|panel| {
                            panel.title = "Prompt result - focus changed".to_string();
                            panel.can_insert = false;
                            panel.can_save_wrong = true;
                            panel.error = Some(
                                "Target focus changed, so the result was not typed.".to_string(),
                            );
                        });
                        self.state.lock().pending_feedback_recording = None;
                        self.set_status("ready");
                    }
                } else {
                    self.state.lock().pending_feedback_recording = None;
                    self.set_status("ready");
                }
            }
            Err(err) => {
                self.push_request_log(RequestLog {
                    ts: Utc::now().to_rfc3339(),
                    stage: "prompt-handoff".to_string(),
                    ok: false,
                    transcript,
                    output: err.to_string(),
                    ttft_ms: None,
                    tokens_per_second: None,
                    total_ms: None,
                });
                self.mutate_prompt_panel(|panel| {
                    panel.state = "error".to_string();
                    panel.title = "Prompt handoff failed".to_string();
                    panel.error = Some(err.to_string());
                });
                self.state.lock().pending_feedback_recording = None;
                self.set_status("error");
            }
        }
    }

    fn show_agentic_placeholder(&self, recording_id: u64, transcript: String, source_output: String) {
        self.set_prompt_panel(Some(PromptPanelState {
            kind: PromptPanelKind::Agentic,
            state: "placeholder".to_string(),
            title: "Agentic mode requested".to_string(),
            transcript,
            source_output,
            text: "Agentic mode is not implemented yet. The request was captured, but no clipboard, coding, filesystem, notes, or computer-use action was run.".to_string(),
            delivery: Some("ui".to_string()),
            recording_id: Some(recording_id),
            can_insert: false,
            can_save_wrong: true,
            collapsed: false,
            error: None,
        }));
    }

    async fn inject_actions_with_context(
        self: &Arc<Self>,
        actions: Vec<Action>,
        settings: Settings,
        replacement_context: Option<FocusedTextContext>,
    ) {
        self.set_status("injecting");
        self.update_overlay("injecting", None);
        let abort = self.abort.clone();
        let logger = self.logger.clone();
        let logger_for_block = self.logger.clone();
        let result = tauri::async_runtime::spawn_blocking(move || {
            let mut replaced_selection = false;
            let mut focus_changed_skipped = false;
            let actions_to_inject = if let Some(context) = replacement_context {
                let replacement = text_from_actions(&actions);
                if !replacement.is_empty()
                    && context::replace_preserved_selection(&context, &replacement)?
                {
                    replaced_selection = true;
                    actions
                        .iter()
                        .filter(|action| matches!(action, Action::Shortcut { .. }))
                        .cloned()
                        .collect::<Vec<_>>()
                } else if !replacement.is_empty() {
                    // Same-focus UIA replace didn't apply. Check whether focus changed
                    // since capture; if so, locate the captured selection in the new
                    // field via its surrounding-context anchor and replace it there.
                    // If focus changed and we can't safely locate it, drop the text
                    // injection entirely (typing into the wrong field is worse than
                    // doing nothing).
                    match context::try_replace_in_changed_focus(&context, &replacement)? {
                        context::ChangedFocusReplace::Replaced => {
                            replaced_selection = true;
                            logger_for_block.log(
                                "info",
                                "focus changed since capture — found and replaced selection in new focus via anchor"
                                    .to_string(),
                            );
                            actions
                                .iter()
                                .filter(|action| matches!(action, Action::Shortcut { .. }))
                                .cloned()
                                .collect::<Vec<_>>()
                        }
                        context::ChangedFocusReplace::FocusChangedNotFound => {
                            focus_changed_skipped = true;
                            logger_for_block.log(
                                "warn",
                                "focus changed since capture and original selection not found in new focus — skipping text injection"
                                    .to_string(),
                            );
                            // Still allow shortcut actions through (e.g. {{Enter}}).
                            actions
                                .iter()
                                .filter(|action| matches!(action, Action::Shortcut { .. }))
                                .cloned()
                                .collect::<Vec<_>>()
                        }
                        context::ChangedFocusReplace::SameFocus => actions.clone(),
                    }
                } else {
                    actions.clone()
                }
            } else {
                actions.clone()
            };
            injection::inject(&actions_to_inject, &settings, &abort)?;
            Ok::<_, anyhow::Error>((actions, replaced_selection, focus_changed_skipped))
        })
        .await;

        match result {
            Ok(Ok((actions, replaced_selection, focus_changed_skipped))) => {
                let current_window = self.state.lock().current_window.clone();
                let saved_recording = {
                    let mut state = self.state.lock();
                    if let Some(recording) = state.pending_feedback_recording.take() {
                        let model_inputs = state.model_inputs.iter().cloned().collect();
                        let saved_recording = recording.clone();
                        state.feedback_candidate = Some(FeedbackCandidate {
                            recording,
                            model_inputs,
                            window: current_window.clone(),
                        });
                        Some(saved_recording)
                    } else {
                        None
                    }
                };
                self.set_status("ready");
                // Show the small post-injection card. update_overlay() runs strictly
                // after injection::inject() returned, so making the overlay interactive
                // from this point on cannot disturb SendInput. The Done card itself
                // carries a ✕ icon (rendered by the frontend when overlay-state.done_id
                // is present) that opens the wrong-output popup.
                self.update_overlay(
                    "done",
                    Some(format!("Injected {} action(s)", actions.len())),
                );
                // Auto-dismiss the Done card after a short timer so the overlay does
                // not linger as a foreground-eligible surface.
                if let Some(recording) = saved_recording.as_ref() {
                    let core = Arc::clone(self);
                    let dismiss_id = recording.id;
                    tauri::async_runtime::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(6)).await;
                        // Only hide if still on the Done card for the same recording.
                        // Don't clobber a feedback popup the user expanded or any
                        // later state (recording/processing/injecting/error/...).
                        let should_clear = {
                            let state = core.state.lock();
                            state.prompt_panel.is_none()
                                && state
                                    .feedback_candidate
                                    .as_ref()
                                    .map(|fc| fc.recording.id == dismiss_id)
                                    .unwrap_or(false)
                                && state.status == "ready"
                        };
                        if should_clear {
                            core.update_overlay("idle", None);
                        }
                    });
                }
                let _ = saved_recording;
                if replaced_selection {
                    logger.log(
                        "info",
                        format!(
                            "replaced preserved selection via UI Automation, then handled {} action(s): {}",
                            actions.len(),
                            action_summary(&actions)
                        ),
                    );
                } else if focus_changed_skipped {
                    logger.log(
                        "warn",
                        format!(
                            "focus changed — text injection skipped; only shortcut actions (if any) emitted from {} action(s)",
                            actions.len()
                        ),
                    );
                } else {
                    logger.log(
                        "info",
                        format!(
                            "injected {} action(s): {}",
                            actions.len(),
                            action_summary(&actions)
                        ),
                    );
                }
            }
            Ok(Err(err)) => {
                self.set_status("error");
                self.update_overlay("error", Some(short_overlay_error("Injection failed")));
                logger.log("error", format!("injection failed: {err}"));
            }
            Err(err) => {
                self.set_status("error");
                self.update_overlay("error", Some(short_overlay_error("Injection task failed")));
                logger.log("error", format!("injection task failed: {err}"));
            }
        }
    }

    fn update_overlay(&self, state: &str, text: Option<String>) {
        if let Some(app) = self.app.lock().as_ref() {
            if let Some(window) = app.get_webview_window("overlay") {
                let (transcript, pending_text, prompt_panel) = {
                    let runtime = self.state.lock();
                    (
                        runtime.transcript.clone(),
                        runtime
                            .pending_actions
                            .as_ref()
                            .map(|actions| text_from_actions(actions))
                            .unwrap_or_default(),
                        runtime.prompt_panel.clone(),
                    )
                };
                let payload = serde_json::json!({
                    "state": state,
                    "text": text.unwrap_or_default(),
                    "transcript": transcript,
                    "pending_text": pending_text,
                    "prompt_panel": prompt_panel,
                });
                let (width, height) = overlay_dimensions(state);
                let _ = window.set_size(tauri::LogicalSize::new(width, height));
                // Only confirm/prompt-panel surfaces are interactive. The "done"
                // card is status-only; making it interactive previously let a
                // ✕ button be clickable but the feature was removed because of
                // unsolvable Windows/WebView2 focus quirks (see commit history).
                let _ = window
                    .set_ignore_cursor_events(!matches!(state, "confirm" | "prompt-panel"));
                let _ = app.emit_to("overlay", "overlay-state", payload);
                match state {
                    "idle" if self.state.lock().prompt_panel.is_none() => {
                            let _ = window.hide();
                        }
                    _ => {
                        position_overlay(app);
                        let _ = window.show();
                    }
                }
            }
        }
    }
}

fn diagnostic_log_files() -> Vec<DiagnosticLogFile> {
    let log_dir = settings::config_dir().join("logs");
    ["audit.jsonl", "llama-server.log"]
        .into_iter()
        .filter_map(|name| {
            let path = log_dir.join(name);
            let content = read_log_tail(&path, 256 * 1024)?;
            Some(DiagnosticLogFile {
                name: name.to_string(),
                path: path.to_string_lossy().to_string(),
                content,
            })
        })
        .collect()
}

fn read_log_tail(path: &std::path::Path, max_bytes: usize) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let start = bytes.len().saturating_sub(max_bytes);
    Some(String::from_utf8_lossy(&bytes[start..]).to_string())
}

fn actions_after_streamed_text(actions: &[Action], streamed_text: &str) -> Vec<Action> {
    let mut remaining_streamed = streamed_text;
    let mut remaining_actions = Vec::new();
    for action in actions {
        match action {
            Action::Text { value } if !remaining_streamed.is_empty() => {
                if let Some(rest) = remaining_streamed.strip_prefix(value) {
                    remaining_streamed = rest;
                } else if let Some(rest) = value.strip_prefix(remaining_streamed) {
                    if !rest.is_empty() {
                        remaining_actions.push(Action::Text {
                            value: rest.to_string(),
                        });
                    }
                    remaining_streamed = "";
                } else {
                    remaining_actions.push(action.clone());
                }
            }
            _ => remaining_actions.push(action.clone()),
        }
    }
    remaining_actions
}

fn trigger_button_down(button: TriggerButton) -> bool {
    match button {
        TriggerButton::Left => context::left_mouse_button_down(),
        TriggerButton::Right => context::right_mouse_button_down(),
    }
}

fn target_still_focused(target: Option<&WindowContext>) -> bool {
    let Some(target) = target else {
        return true;
    };
    let Some(now) = context::active_window_context() else {
        return false;
    };
    now.app_name == target.app_name && now.title == target.title
}

fn text_from_actions(actions: &[Action]) -> String {
    actions
        .iter()
        .filter_map(|action| match action {
            Action::Text { value } => Some(value.as_str()),
            Action::Shortcut { .. } => None,
            Action::Prompt | Action::Agentic => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn action_summary(actions: &[Action]) -> String {
    actions
        .iter()
        .map(|action| match action {
            Action::Text { value } => {
                format!("text({:?})", clip_log_text(value, 120))
            }
            Action::Shortcut { keys } => format!("shortcut({})", keys.join("+")),
            Action::Prompt => "prompt-handoff".to_string(),
            Action::Agentic => "agentic-handoff".to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn clip_log_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        format!("{}...", text.chars().take(max_chars).collect::<String>())
    }
}

fn preserved_selection_context(context: Option<&WindowContext>) -> Option<FocusedTextContext> {
    context
        .and_then(|window| window.focused_text.as_ref())
        .filter(|text| {
            text.selected_text
                .as_ref()
                .map(|selected| !selected.is_empty())
                .unwrap_or(false)
        })
        .cloned()
}

fn capture_selection_with_clipboard_probe(core: &Arc<AppCore>, start: Instant) {
    let Some(captured) = clipboard::capture_selection_via_clipboard() else {
        return;
    };
    let trimmed = captured.trim().to_string();
    if trimmed.is_empty() {
        return;
    }
    let mut gesture = core.gesture.lock();
    let Some(session) = gesture.as_mut() else {
        return;
    };
    if session.start != start {
        return;
    }
    let mut ctx = session.context.clone().unwrap_or_else(|| WindowContext {
        title: String::new(),
        app_name: String::new(),
        cursor_x: 0,
        cursor_y: 0,
        focused_text: None,
        cursor_screenshot: None,
    });
    let mut ft = ctx
        .focused_text
        .clone()
        .unwrap_or_else(|| FocusedTextContext {
            source: "clipboard probe".to_string(),
            element_name: None,
            control_type: None,
            class_name: None,
            automation_id: None,
            parent_name: None,
            parent_class: None,
            parent_control_type: None,
            text_before_cursor: None,
            selected_text: None,
            text_after_cursor: None,
            full_text: None,
            truncated: false,
            cursor_known: false,
            element_bounds: None,
        });
    let already_has_selection = ft
        .selected_text
        .as_ref()
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    if !already_has_selection {
        ft.selected_text = Some(trimmed.clone());
        ft.source = if ft.source == "clipboard probe" {
            "clipboard probe".to_string()
        } else {
            format!("{} + clipboard", ft.source)
        };
    }
    ctx.focused_text = Some(ft);
    session.context = Some(ctx);
    core.log(
        "info",
        format!(
            "captured {} chars of selected text via clipboard probe",
            trimmed.chars().count()
        ),
    );
}

fn short_overlay_error(prefix: &str) -> String {
    format!("{prefix}. See Diagnostics for details.")
}

fn is_context_length_error(err: &anyhow::Error) -> bool {
    let text = err.to_string().to_ascii_lowercase();
    text.contains("exceeds the available context size")
        || text.contains("context size")
        || text.contains("context length")
        || text.contains("too many tokens")
}

fn context_has_selection(context: Option<&WindowContext>) -> bool {
    context
        .and_then(|window| window.focused_text.as_ref())
        .and_then(|text| text.selected_text.as_ref())
        .map(|text| !text.trim().is_empty())
        .unwrap_or(false)
}

fn copy_shortcut_needs_confirmation(actions: &[Action], context: Option<&WindowContext>) -> bool {
    actions.iter().any(is_copy_shortcut)
        && !context_has_selection(context)
        && !is_text_capable_context(context)
}

fn is_copy_shortcut(action: &Action) -> bool {
    let Action::Shortcut { keys } = action else {
        return false;
    };
    let normalized = keys
        .iter()
        .map(|key| key.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    normalized
        .iter()
        .any(|key| matches!(key.as_str(), "ctrl" | "control"))
        && normalized.iter().any(|key| key == "c")
}

fn clipboard_probe_skip_reason(context: Option<&WindowContext>) -> Option<String> {
    let Some(ctx) = context else {
        return Some("no foreground context".to_string());
    };
    if terminal_context_kind(Some(ctx)).is_some() {
        return Some(format!("terminal context ({})", app_context_label(ctx)));
    }
    if is_canvas_or_non_text_app(ctx) {
        return Some(format!("non-text/canvas app ({})", app_context_label(ctx)));
    }
    if is_text_capable_context(Some(ctx)) {
        return None;
    }
    Some(format!(
        "focused control is not text-capable ({})",
        app_context_label(ctx)
    ))
}

fn is_text_capable_context(context: Option<&WindowContext>) -> bool {
    let Some(ctx) = context else {
        return false;
    };
    let app = ctx.app_name.to_ascii_lowercase();
    let title = ctx.title.to_ascii_lowercase();
    if is_canvas_or_non_text_app(ctx) || terminal_context_kind(Some(ctx)).is_some() {
        return false;
    }
    if [
        "chrome", "msedge", "firefox", "brave", "notepad", "code.exe", "winword", "excel",
        "powerpnt", "outlook", "onenote", "wordpad",
    ]
    .iter()
    .any(|needle| app.contains(needle) || title.contains(needle))
    {
        return true;
    }
    let Some(text) = ctx.focused_text.as_ref() else {
        return false;
    };
    let haystack = [
        text.source.as_str(),
        text.control_type.as_deref().unwrap_or(""),
        text.class_name.as_deref().unwrap_or(""),
        text.automation_id.as_deref().unwrap_or(""),
        text.element_name.as_deref().unwrap_or(""),
        text.parent_control_type.as_deref().unwrap_or(""),
        text.parent_class.as_deref().unwrap_or(""),
        text.parent_name.as_deref().unwrap_or(""),
    ]
    .join(" ")
    .to_ascii_lowercase();
    [
        "edit",
        "document",
        "text",
        "value",
        "textarea",
        "rich edit",
        "contenteditable",
        "omnibox",
        "urlbar",
        "search",
    ]
    .iter()
    .any(|needle| haystack.contains(needle))
}

fn is_canvas_or_non_text_app(ctx: &WindowContext) -> bool {
    let app = ctx.app_name.to_ascii_lowercase();
    let title = ctx.title.to_ascii_lowercase();
    [
        "mspaint",
        "paint",
        "photoshop",
        "illustrator",
        "blender",
        "krita",
        "gimp",
    ]
    .iter()
    .any(|needle| app.contains(needle) || title.contains(needle))
}

fn app_context_label(ctx: &WindowContext) -> String {
    if ctx.title.trim().is_empty() {
        ctx.app_name.clone()
    } else {
        format!("{} / {}", ctx.app_name, ctx.title)
    }
}

fn is_voice_keyboard_window(context: Option<&WindowContext>) -> bool {
    let Some(ctx) = context else {
        return false;
    };
    let app = ctx.app_name.to_ascii_lowercase();
    let title = ctx.title.to_ascii_lowercase();
    app.contains("voice-keyboard")
        || app.contains("voice_keyboard")
        || title == "voice keyboard"
        || title.contains("local llm voice keyboard")
}

fn normalize_transcript_for_interpretation(
    transcript: &str,
    context: Option<&WindowContext>,
) -> String {
    let trimmed = transcript.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let lowered = trimmed.to_ascii_lowercase();
    let normalized = match lowered.as_str() {
        "and do" | "an do" | "undo voice" => "undo".to_string(),
        "and delete" | "an delete" => "delete".to_string(),
        "and enter" | "an enter" => "enter".to_string(),
        _ => {
            if let Some(rest) = lowered
                .strip_prefix("do control ")
                .or_else(|| lowered.strip_prefix("do ctrl "))
            {
                format!("control {}", rest.trim())
            } else if let Some(rest) = lowered.strip_prefix("hit control ") {
                format!("control {}", rest.trim())
            } else {
                trimmed.to_string()
            }
        }
    };

    if let Some(payload) = extract_line_directive_payload(&normalized) {
        return payload;
    }

    if let Some(command) = direct_terminal_command_hint(&normalized, context) {
        return command;
    }

    normalized
}

fn extract_line_directive_payload(transcript: &str) -> Option<String> {
    let lowered = transcript.trim().to_ascii_lowercase();
    let filler_stripped = lowered
        .trim_start_matches("okay, ")
        .trim_start_matches("okay ")
        .trim_start_matches("now, ")
        .trim_start_matches("now ")
        .trim_start_matches("please ");

    for (needle, token) in [
        ("on the next line", "{{Enter}}"),
        ("in the next line", "{{Enter}}"),
        ("next line", "{{Enter}}"),
        ("new line", "{{Enter}}"),
        ("next paragraph", "{{Enter}}{{Enter}}"),
        ("new paragraph", "{{Enter}}{{Enter}}"),
    ] {
        if let Some(index) = filler_stripped.find(needle) {
            let remainder = transcript.trim()[transcript
                .to_ascii_lowercase()
                .find(needle)
                .unwrap_or(index)
                + needle.len()..]
                .trim_start_matches(|c: char| c == ',' || c == ':' || c.is_whitespace())
                .trim_start_matches("type ")
                .trim_start_matches("write ")
                .trim_start_matches("say ")
                .trim();
            if remainder.is_empty() {
                return Some(token.to_string());
            }
            return Some(format!("{token}{remainder}"));
        }
    }
    None
}

fn direct_interpretation_override(
    transcript: &str,
    context: Option<&WindowContext>,
) -> Option<String> {
    let lowered = transcript.trim().to_ascii_lowercase();
    if lowered.is_empty() {
        return None;
    }
    if transcript.contains("{{") {
        return Some(transcript.trim().to_string());
    }

    if let Some(shortcut) = shortcut_override(&lowered) {
        return Some(shortcut);
    }

    if let Some(command) = direct_terminal_command_hint(&lowered, context) {
        return Some(command);
    }

    if let Some(navigation) = direct_navigation_hint(&lowered, context) {
        return Some(navigation);
    }

    None
}

fn shortcut_override(lowered: &str) -> Option<String> {
    let repeat = repeat_count(lowered);
    let base = lowered
        .trim_end_matches(" twice")
        .trim_end_matches(" thrice")
        .trim_end_matches(" three times")
        .trim_end_matches(" two times")
        .trim_end_matches(" one time")
        .trim_end_matches(" once")
        .trim();

    let token = match base {
        "undo" | "and do" | "an do" | "control z" | "ctrl z" => "{{Ctrl+Z}}",
        "redo" | "control y" | "ctrl y" => "{{Ctrl+Y}}",
        "copy" | "copy this" | "copy that" | "control c" | "ctrl c" | "do control c"
        | "do ctrl c" => "{{Ctrl+C}}",
        "cut" | "cut this" | "control x" | "ctrl x" => "{{Ctrl+X}}",
        "paste" | "control v" | "ctrl v" => "{{Ctrl+V}}",
        "select all" | "control a" | "ctrl a" => "{{Ctrl+A}}",
        "save" | "control s" | "ctrl s" => "{{Ctrl+S}}",
        "find" | "search this page" | "control f" | "ctrl f" => "{{Ctrl+F}}",
        "new tab" | "control t" | "ctrl t" => "{{Ctrl+T}}",
        "close tab" | "control w" | "ctrl w" => "{{Ctrl+W}}",
        "enter"
        | "press enter"
        | "hit enter"
        | "submit"
        | "go"
        | "return"
        | "and enter"
        | "an enter"
        | "shortcut enter"
        | "enter key"
        | "press enter key"
        | "press the enter key" => "{{Enter}}",
        "tab" | "press tab" => "{{Tab}}",
        "escape" | "esc" | "press escape" => "{{Escape}}",
        "backspace" | "press backspace" => "{{Backspace}}",
        "delete" | "delete this" | "delete that" | "delete selected" | "remove this"
        | "erase this" | "clear this" | "and delete" | "an delete" => "{{Delete}}",
        "up" | "arrow up" | "press up" => "{{Up}}",
        "down" | "arrow down" | "press down" => "{{Down}}",
        "left" | "arrow left" | "press left" => "{{Left}}",
        "right" | "arrow right" | "press right" => "{{Right}}",
        _ => return None,
    };

    Some(
        std::iter::repeat_n(token, repeat)
            .collect::<Vec<_>>()
            .join(""),
    )
}

fn repeat_count(text: &str) -> usize {
    if text.contains("thrice") || text.contains("three times") {
        3
    } else if text.contains("twice") || text.contains("two times") {
        2
    } else {
        1
    }
}

fn direct_terminal_command_hint(
    transcript: &str,
    context: Option<&WindowContext>,
) -> Option<String> {
    let terminal_kind = terminal_context_kind(context)?;
    let lowered = transcript.trim().to_ascii_lowercase();
    let enter = "{{Enter}}";

    let command = if lowered.contains("ip address") || lowered.contains("ip config") {
        "ipconfig".to_string()
    } else if lowered.contains("list everything")
        || lowered.contains("list all")
        || lowered.contains("show everything")
    {
        match terminal_kind {
            TerminalKind::Cmd => "dir /a".to_string(),
            TerminalKind::PowerShell => "Get-ChildItem -Force".to_string(),
        }
    } else {
        return None;
    };

    Some(format!("{command}{enter}"))
}

fn direct_navigation_hint(transcript: &str, context: Option<&WindowContext>) -> Option<String> {
    let kind = model::context_kind_label(context);
    let is_address_bar = kind == "browser address/search bar";
    let is_search_box = kind == "search box";
    if !is_address_bar && !is_search_box {
        return None;
    }

    let mut text = transcript
        .trim()
        .trim_end_matches('.')
        .trim()
        .to_ascii_lowercase();
    if text.is_empty() {
        return None;
    }

    if is_search_box {
        for prefix in ["search for ", "search "] {
            if let Some(rest) = text.strip_prefix(prefix) {
                text = rest.trim().to_string();
                break;
            }
        }
        return (!text.is_empty()).then(|| format!("{text}{{{{Enter}}}}"));
    }

    let enter = "{{Enter}}";
    let url = if let Some(rest) = text
        .strip_prefix("search google for ")
        .or_else(|| text.strip_prefix("google "))
        .or_else(|| text.strip_prefix("search for "))
        .or_else(|| text.strip_prefix("search "))
    {
        format!("https://www.google.com/search?q={}", encode_query(rest))
    } else if let Some(rest) = text
        .strip_prefix("search youtube for ")
        .or_else(|| text.strip_prefix("youtube "))
    {
        format!(
            "https://www.youtube.com/results?search_query={}",
            encode_query(rest)
        )
    } else if let Some(rest) = text
        .strip_prefix("search amazon for ")
        .or_else(|| text.strip_prefix("amazon "))
        .or_else(|| text.strip_prefix("buy "))
    {
        format!("https://www.amazon.in/s?k={}", encode_query(rest))
    } else if let Some(rest) = text
        .strip_prefix("search flipkart for ")
        .or_else(|| text.strip_prefix("flipkart "))
    {
        format!("https://www.flipkart.com/search?q={}", encode_query(rest))
    } else if let Some(rest) = text
        .strip_prefix("wikipedia ")
        .or_else(|| text.strip_prefix("what is "))
    {
        format!("https://en.wikipedia.org/wiki/{}", encode_query(rest))
    } else if let Some(rest) = text
        .strip_prefix("github ")
        .or_else(|| text.strip_prefix("find repo "))
    {
        format!("https://github.com/search?q={}", encode_query(rest))
    } else if let Some(rest) = text.strip_prefix("stack overflow ") {
        format!("https://stackoverflow.com/search?q={}", encode_query(rest))
    } else if let Some(rest) = text
        .strip_prefix("maps to ")
        .or_else(|| text.strip_prefix("directions to "))
        .or_else(|| text.strip_prefix("map "))
    {
        format!("https://maps.google.com/maps?q={}", encode_query(rest))
    } else if let Some(site) = direct_site_url(&text) {
        site.to_string()
    } else if looks_like_navigation_transcript(&text) {
        if text.starts_with("http://") || text.starts_with("https://") {
            text
        } else {
            format!("https://{text}")
        }
    } else {
        return None;
    };

    Some(format!("{url}{enter}"))
}

fn direct_site_url(text: &str) -> Option<&'static str> {
    let stripped = text.strip_prefix("open ").unwrap_or(text).trim();
    match stripped {
        "gmail" => Some("https://mail.google.com"),
        "youtube" => Some("https://www.youtube.com"),
        "netflix" => Some("https://www.netflix.com"),
        "facebook" => Some("https://www.facebook.com"),
        "twitter" | "x" => Some("https://www.twitter.com"),
        "linkedin" => Some("https://www.linkedin.com"),
        "whatsapp web" => Some("https://web.whatsapp.com"),
        "github" => Some("https://www.github.com"),
        "amazon" => Some("https://www.amazon.in"),
        "google drive" | "drive" => Some("https://drive.google.com"),
        "google docs" | "docs" => Some("https://docs.google.com"),
        "google sheets" | "sheets" => Some("https://sheets.google.com"),
        "chatgpt" | "chat gpt" => Some("https://chat.openai.com"),
        _ => None,
    }
}

fn encode_query(text: &str) -> String {
    text.trim()
        .as_bytes()
        .iter()
        .flat_map(|byte| match *byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                vec![*byte as char]
            }
            b' ' | b'\t' | b'\n' => vec!['+'],
            other => format!("%{other:02X}").chars().collect(),
        })
        .collect()
}

#[derive(Clone, Copy)]
enum TerminalKind {
    Cmd,
    PowerShell,
}

fn terminal_context_kind(context: Option<&WindowContext>) -> Option<TerminalKind> {
    let ctx = context?;
    let app = ctx.app_name.to_ascii_lowercase();
    let title = ctx.title.to_ascii_lowercase();

    if app.contains("cmd.exe") || title.contains("command prompt") {
        Some(TerminalKind::Cmd)
    } else if app.contains("powershell")
        || app.contains("pwsh")
        || title.contains("powershell")
        || (app.contains("windowsterminal") && !title.contains("cmd"))
    {
        Some(TerminalKind::PowerShell)
    } else {
        None
    }
}

fn should_use_thinking_handoff(
    settings: &Settings,
    transcript: &str,
    context: Option<&WindowContext>,
) -> bool {
    if !settings.thinking_handoff_enabled {
        return false;
    }

    let transcript_text = transcript.trim();
    if transcript_text.is_empty() {
        return false;
    }
    if looks_like_navigation_transcript(&transcript_text.to_ascii_lowercase()) {
        return false;
    }

    let lowered = transcript_text.to_ascii_lowercase();
    let has_rewrite_intent = [
        "rewrite",
        "rephrase",
        "paraphrase",
        "translate",
        "summarize",
        "summarise",
        "polish",
        "proofread",
        "grammar",
        "grammer",
        "fix this",
        "correct this",
        "make this clearer",
        "improve this",
        "shorten this",
        "expand this",
        "format this",
        "fix the grammar",
        "translate this",
    ]
    .iter()
    .any(|phrase| lowered.contains(phrase));
    if !has_rewrite_intent {
        return false;
    }

    let selected_len = context
        .and_then(|window| window.focused_text.as_ref())
        .and_then(|text| text.selected_text.as_ref())
        .map(|text| text.trim().chars().count())
        .unwrap_or(0);
    let selected_is_paragraph = context
        .and_then(|window| window.focused_text.as_ref())
        .and_then(|text| text.selected_text.as_ref())
        .map(|text| text.contains('\n') || text.matches(". ").count() >= 2 || text.len() >= 180)
        .unwrap_or(false);
    let has_selection = selected_len > 0;

    (has_selection
        && [
            "translate",
            "rewrite",
            "rephrase",
            "paraphrase",
            "grammar",
            "grammer",
            "proofread",
            "polish",
            "format this",
            "fix this",
            "correct this",
            "shorten this",
            "expand this",
            "make this clearer",
            "improve this",
        ]
        .iter()
        .any(|phrase| lowered.contains(phrase)))
        || transcript_text.chars().count() >= settings.thinking_handoff_min_chars
        || selected_len >= settings.thinking_handoff_min_chars
        || selected_is_paragraph
}

fn is_continuity_worthy(item: &RecentTextContext) -> bool {
    let output = item.output.to_ascii_lowercase();
    let transcript = item.transcript.to_ascii_lowercase();
    item.stage.contains("thinking")
        || output.chars().count() >= 24
        || output.contains(char::is_whitespace)
        || transcript.contains("rewrite")
        || transcript.contains("translate")
        || transcript.contains("grammar")
        || transcript.contains("summar")
}

fn is_recent_command_noise(log: &RequestLog) -> bool {
    let output = log.output.trim().to_ascii_lowercase();
    let transcript = log.transcript.trim().to_ascii_lowercase();
    output.is_empty()
        || output.contains("{{")
        || looks_like_navigation_transcript(&output)
        || output.starts_with("http://")
        || output.starts_with("https://")
        || output.starts_with("cd ")
        || output == "dir"
        || output.starts_with("dir ")
        || output.starts_with("get-childitem")
        || output.starts_with("git ")
        || output.starts_with("npm ")
        || output.starts_with("cargo ")
        || output.starts_with("python ")
        || output == "ipconfig"
        || transcript == "press enter"
        || transcript == "undo"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected_context(selected_text: &str) -> WindowContext {
        WindowContext {
            title: "Editor".to_string(),
            app_name: "notepad.exe".to_string(),
            cursor_x: 0,
            cursor_y: 0,
            focused_text: Some(FocusedTextContext {
                source: "uia".to_string(),
                element_name: None,
                control_type: Some("Edit".to_string()),
                class_name: None,
                automation_id: None,
                parent_name: None,
                parent_class: None,
                parent_control_type: None,
                text_before_cursor: Some(String::new()),
                selected_text: Some(selected_text.to_string()),
                text_after_cursor: Some(String::new()),
                full_text: None,
                truncated: false,
                cursor_known: true,
                element_bounds: None,
            }),
            cursor_screenshot: None,
        }
    }

    #[test]
    fn thinking_handoff_triggers_for_rewrite_of_long_selection() {
        let settings = Settings::default();
        let context = selected_context(
            "this is a long paragraph that should be rewritten for clarity and grammar. it has several sentences and plenty of text to justify the handoff. the wording is repetitive, the structure is awkward, and the tone needs to sound much more professional for a client email. please smooth it out while preserving the intent and the key details.",
        );
        assert!(should_use_thinking_handoff(
            &settings,
            "rewrite this to be more professional",
            Some(&context)
        ));
    }

    #[test]
    fn thinking_handoff_skips_navigation_commands() {
        let settings = Settings::default();
        assert!(!should_use_thinking_handoff(&settings, "gmail.com", None));
    }

    #[test]
    fn thinking_handoff_triggers_for_short_translation_with_selection() {
        let settings = Settings::default();
        let context = selected_context("good morning team");
        assert!(should_use_thinking_handoff(
            &settings,
            "Translate this also into Spanish.",
            Some(&context)
        ));
    }

    #[test]
    fn normalizes_undo_misrecognition() {
        assert_eq!(
            normalize_transcript_for_interpretation("and do", None),
            "undo".to_string()
        );
        assert_eq!(
            direct_interpretation_override("undo", None),
            Some("{{Ctrl+Z}}".to_string())
        );
    }

    #[test]
    fn extracts_next_line_directive_as_shortcut_text() {
        assert_eq!(
            normalize_transcript_for_interpretation(
                "Okay, now on the next line type thank you for your help",
                None
            ),
            "{{Enter}}thank you for your help".to_string()
        );
    }

    #[test]
    fn clipboard_probe_skips_paint_and_terminals() {
        let paint = WindowContext {
            title: "Untitled - Paint".to_string(),
            app_name: "mspaint.exe".to_string(),
            cursor_x: 0,
            cursor_y: 0,
            focused_text: None,
            cursor_screenshot: None,
        };
        assert!(clipboard_probe_skip_reason(Some(&paint))
            .unwrap()
            .contains("non-text"));

        let terminal = WindowContext {
            title: "Command Prompt".to_string(),
            app_name: "cmd.exe".to_string(),
            cursor_x: 0,
            cursor_y: 0,
            focused_text: None,
            cursor_screenshot: None,
        };
        assert!(clipboard_probe_skip_reason(Some(&terminal))
            .unwrap()
            .contains("terminal"));
    }

    #[test]
    fn clipboard_probe_allows_text_context() {
        let context = selected_context("");
        assert!(clipboard_probe_skip_reason(Some(&context)).is_none());
    }

    #[test]
    fn copy_shortcut_confirms_in_non_text_without_selection() {
        let paint = WindowContext {
            title: "Untitled - Paint".to_string(),
            app_name: "mspaint.exe".to_string(),
            cursor_x: 0,
            cursor_y: 0,
            focused_text: None,
            cursor_screenshot: None,
        };
        let actions = vec![Action::Shortcut {
            keys: vec!["Ctrl".to_string(), "C".to_string()],
        }];
        assert!(copy_shortcut_needs_confirmation(&actions, Some(&paint)));
        assert!(!copy_shortcut_needs_confirmation(
            &actions,
            Some(&selected_context("hello"))
        ));
    }

    #[test]
    fn ignores_own_ui_for_gestures() {
        let context = WindowContext {
            title: "Voice Keyboard".to_string(),
            app_name: "voice-keyboard.exe".to_string(),
            cursor_x: 0,
            cursor_y: 0,
            focused_text: None,
            cursor_screenshot: None,
        };
        assert!(is_voice_keyboard_window(Some(&context)));
    }

    #[test]
    fn enter_shortcut_variants_are_direct() {
        assert_eq!(
            direct_interpretation_override("shortcut enter", None),
            Some("{{Enter}}".to_string())
        );
        assert_eq!(
            direct_interpretation_override("press the enter key", None),
            Some("{{Enter}}".to_string())
        );
    }
}

fn looks_like_navigation_transcript(text: &str) -> bool {
    if text.contains(char::is_whitespace) {
        return false;
    }
    text.starts_with("http://")
        || text.starts_with("https://")
        || text.starts_with("www.")
        || [
            ".com", ".org", ".net", ".io", ".ai", ".dev", ".app", ".co", ".in",
        ]
        .iter()
        .any(|suffix| text.ends_with(suffix))
}

/// Strip png_base64 from a JSON value, saving any found images as PNG files.
/// Returns the modified JSON and a list of saved paths.
fn extract_screenshots(
    val: &mut serde_json::Value,
    screenshots_dir: &std::path::Path,
    prefix: &str,
) {
    match val {
        serde_json::Value::Object(map) => {
            if let Some(b64_val) = map.remove("png_base64") {
                if let Some(b64_str) = b64_val.as_str() {
                    if let Ok(bytes) = general_purpose::STANDARD.decode(b64_str) {
                        let filename = format!("{}_{}.png", prefix, Utc::now().timestamp_millis());
                        let path = screenshots_dir.join(&filename);
                        if fs::write(&path, bytes).is_ok() {
                            map.insert(
                                "screenshot_path".to_string(),
                                serde_json::Value::String(path.to_string_lossy().to_string()),
                            );
                        }
                    }
                }
            }
            for v in map.values_mut() {
                extract_screenshots(v, screenshots_dir, prefix);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr.iter_mut() {
                extract_screenshots(v, screenshots_dir, prefix);
            }
        }
        _ => {}
    }
}

fn write_feedback_example(
    candidate: &FeedbackCandidate,
    label: &FeedbackLabel,
    expected_output: Option<String>,
) -> anyhow::Result<PathBuf> {
    let dataset_dir = settings::config_dir().join("dataset");
    let audio_dir = dataset_dir.join("audio");
    let screenshots_dir = dataset_dir.join("screenshots");
    fs::create_dir_all(&audio_dir)?;
    fs::create_dir_all(&screenshots_dir)?;

    let label_text = match label {
        FeedbackLabel::Positive => "positive",
        FeedbackLabel::Negative => "negative",
    };
    let ts_ms = Utc::now().timestamp_millis();
    let audio_path = audio_dir.join(format!(
        "{}_{}_{}_.wav",
        candidate.recording.id, label_text, ts_ms
    ));
    fs::copy(&candidate.recording.audio_path, &audio_path)?;

    // Serialize model_inputs and window to JSON, extracting any screenshots
    let mut inputs_val =
        serde_json::to_value(&candidate.model_inputs).unwrap_or(serde_json::Value::Null);
    extract_screenshots(
        &mut inputs_val,
        &screenshots_dir,
        &format!("{}_{}", candidate.recording.id, label_text),
    );

    let mut window_val = serde_json::to_value(&candidate.window).unwrap_or(serde_json::Value::Null);
    extract_screenshots(
        &mut window_val,
        &screenshots_dir,
        &format!("{}_{}_win", candidate.recording.id, label_text),
    );

    let example = serde_json::json!({
        "ts": Utc::now().to_rfc3339(),
        "label": label,
        "audio_path": audio_path.to_string_lossy(),
        "transcript": &candidate.recording.transcript,
        "output": &candidate.recording.output,
        "expected_output": expected_output,
        "actions": &candidate.recording.actions,
        "model_inputs": inputs_val,
        "audio_duration_ms": candidate.recording.audio_duration_ms,
        "transcription_ttft_ms": candidate.recording.transcription_ttft_ms,
        "interpretation_ttft_ms": candidate.recording.interpretation_ttft_ms,
        "window": window_val,
    });

    let path = dataset_dir.join("feedback.jsonl");
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(file, "{}", serde_json::to_string(&example)?)?;
    Ok(path)
}

fn replace_text_actions(actions: Vec<Action>, replacement: String) -> Vec<Action> {
    let mut inserted = false;
    let mut next = Vec::new();
    for action in actions {
        match action {
            Action::Text { .. } if !inserted => {
                if !replacement.is_empty() {
                    next.push(Action::Text {
                        value: replacement.clone(),
                    });
                }
                inserted = true;
            }
            Action::Text { .. } => {}
            other => next.push(other),
        }
    }
    if !inserted && !replacement.is_empty() {
        next.insert(0, Action::Text { value: replacement });
    }
    next
}

fn overlay_dimensions(state: &str) -> (f64, f64) {
    if state == "confirm" {
        (430.0, 214.0)
    } else if state == "prompt-panel" {
        (620.0, 330.0)
    } else {
        (360.0, 96.0)
    }
}

fn create_overlay_window(app: &tauri::App) -> tauri::Result<()> {
    let (width, height) = overlay_dimensions("idle");
    let overlay =
        WebviewWindowBuilder::new(app, "overlay", WebviewUrl::App("index.html?overlay".into()))
            .title("Voice Keyboard State")
            .inner_size(width, height)
            .decorations(false)
            .transparent(true)
            .always_on_top(true)
            .skip_taskbar(true)
            .resizable(false)
            .visible(false)
            .shadow(false)
            .build()?;
    let _ = overlay.set_ignore_cursor_events(true);
    position_overlay(app.handle());
    Ok(())
}

fn position_overlay(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("overlay") {
        if let Ok(Some(monitor)) = window.primary_monitor() {
            let size = monitor.size();
            let scale = monitor.scale_factor();
            let logical_width = size.width as f64 / scale;
            let logical_height = size.height as f64 / scale;
            let window_size = window
                .inner_size()
                .ok()
                .map(|s| (s.width as f64 / scale, s.height as f64 / scale))
                .unwrap_or_else(|| overlay_dimensions("idle"));
            let x = ((logical_width - window_size.0) / 2.0).max(12.0);
            let bottom_margin = if window_size.1 > 140.0 { 110.0 } else { 28.0 };
            let y = (logical_height - window_size.1 - bottom_margin).max(12.0);
            let _ = window.set_position(tauri::LogicalPosition::new(x, y));
        }
    }
}

fn temp_wav_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "voice-keyboard-{}.wav",
        chrono::Utc::now().timestamp_millis()
    ))
}

#[tauri::command]
fn get_status(core: tauri::State<'_, Arc<AppCore>>) -> StatusSnapshot {
    core.snapshot()
}

#[tauri::command]
fn start_hook(core: tauri::State<'_, Arc<AppCore>>) -> Result<StatusSnapshot, String> {
    core.start_workers().map_err(|e| e.to_string())?;
    Ok(core.snapshot())
}

#[tauri::command]
fn stop_hook(core: tauri::State<'_, Arc<AppCore>>) -> StatusSnapshot {
    core.stop_hook();
    core.snapshot()
}

#[tauri::command]
fn set_paused(core: tauri::State<'_, Arc<AppCore>>, paused: bool) -> StatusSnapshot {
    core.set_paused(paused);
    core.snapshot()
}

#[tauri::command]
async fn shutdown_app(core: tauri::State<'_, Arc<AppCore>>) -> Result<(), String> {
    core.shutdown().await;
    Ok(())
}

#[tauri::command]
fn snapshot_context(core: tauri::State<'_, Arc<AppCore>>) -> StatusSnapshot {
    let settings = core.settings.lock().clone();
    let context = context::active_window_context_with_screenshot_settings(&settings);
    core.state.lock().current_window = context;
    core.emit_snapshot();
    core.snapshot()
}

#[tauri::command]
fn list_audio_input_devices() -> Result<Vec<audio::AudioInputDevice>, String> {
    audio::input_devices().map_err(|e| e.to_string())
}

#[tauri::command]
fn abort_injection(core: tauri::State<'_, Arc<AppCore>>) -> StatusSnapshot {
    core.abort.abort();
    core.set_status("aborted");
    core.log("warn", "injection abort requested from UI");
    core.snapshot()
}

#[tauri::command]
async fn confirm_pending(core: tauri::State<'_, Arc<AppCore>>) -> Result<StatusSnapshot, String> {
    let (actions, replacement_context) = {
        let mut state = core.state.lock();
        (
            state.pending_actions.take(),
            state.pending_replacement_context.take(),
        )
    };
    if let Some(actions) = actions {
        let settings = core.settings.lock().clone();
        core.inject_actions_with_context(actions, settings, replacement_context)
            .await;
    }
    Ok(core.snapshot())
}

#[tauri::command]
async fn confirm_pending_text(
    core: tauri::State<'_, Arc<AppCore>>,
    text: String,
) -> Result<StatusSnapshot, String> {
    let (actions, replacement_context) = {
        let mut state = core.state.lock();
        (
            state.pending_actions.take(),
            state.pending_replacement_context.take(),
        )
    };
    if let Some(actions) = actions {
        let settings = core.settings.lock().clone();
        core.inject_actions_with_context(
            replace_text_actions(actions, text),
            settings,
            replacement_context,
        )
        .await;
    }
    Ok(core.snapshot())
}

#[tauri::command]
fn deny_pending(core: tauri::State<'_, Arc<AppCore>>) -> StatusSnapshot {
    let mut state = core.state.lock();
    state.pending_actions = None;
    state.pending_replacement_context = None;
    state.pending_feedback_recording = None;
    drop(state);
    core.set_status("ready");
    core.log("info", "pending actions denied");
    core.snapshot()
}

#[tauri::command]
fn save_feedback_example(
    core: tauri::State<'_, Arc<AppCore>>,
    correct: bool,
    recording_id: Option<u64>,
    expected_output: Option<String>,
) -> Result<StatusSnapshot, String> {
    let label = if correct {
        FeedbackLabel::Positive
    } else {
        FeedbackLabel::Negative
    };
    core.log(
        "info",
        format!(
            "save_feedback_example invoked: correct={correct} recording_id={recording_id:?} expected_len={}",
            expected_output.as_deref().map(|s| s.len()).unwrap_or(0)
        ),
    );
    if let Err(err) = core.save_feedback(label, recording_id, expected_output) {
        let msg = format!("save_feedback failed: {err:#}");
        core.log("error", &msg);
        // Surface the error in the popup so the user knows something went wrong.
        core.mutate_prompt_panel(|panel| {
            panel.error = Some(msg.clone());
        });
        return Err(msg);
    }
    if !correct {
        core.set_prompt_panel(None);
    }
    Ok(core.snapshot())
}

#[tauri::command]
fn set_prompt_panel_collapsed(core: tauri::State<'_, Arc<AppCore>>, collapsed: bool) -> StatusSnapshot {
    core.mutate_prompt_panel(|panel| {
        panel.collapsed = collapsed;
    });
    core.snapshot()
}

#[tauri::command]
fn dismiss_prompt_panel(core: tauri::State<'_, Arc<AppCore>>) -> StatusSnapshot {
    core.set_prompt_panel(None);
    core.snapshot()
}

#[tauri::command]
fn open_dataset_folder() -> Result<(), String> {
    let dataset_dir = settings::config_dir().join("dataset");
    std::fs::create_dir_all(&dataset_dir).map_err(|e| e.to_string())?;
    std::process::Command::new("explorer")
        .arg(&dataset_dir)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn open_models_folder(core: tauri::State<'_, Arc<AppCore>>) -> Result<(), String> {
    let settings = core.settings.lock().clone();
    let models_dir = settings::models_dir(&settings);
    std::fs::create_dir_all(&models_dir).map_err(|e| e.to_string())?;
    std::process::Command::new("explorer")
        .arg(&models_dir)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn get_model_setup_info(
    core: tauri::State<'_, Arc<AppCore>>,
) -> Result<model_setup::ModelSetupInfo, String> {
    let settings = core.settings.lock().clone();
    model_setup::setup_info(&settings)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn download_model_candidate(
    app: tauri::AppHandle,
    core: tauri::State<'_, Arc<AppCore>>,
    repo: String,
    file: String,
    hf_token: Option<String>,
) -> Result<StatusSnapshot, String> {
    let settings = core.settings.lock().clone();
    core.log("info", format!("downloading model {repo}/{file}"));
    let next = model_setup::download_model(&settings, &repo, &file, hf_token, |progress| {
        let _ = app.emit("model-download-progress", progress);
    })
    .await
    .map_err(|e| e.to_string())?;
    if settings.model_path != next.model_path || settings.mmproj_path != next.mmproj_path {
        core.model.shutdown(&settings).await;
    }
    *core.settings.lock() = next.clone();
    core.log("info", "model downloaded and settings updated");
    core.warm_model_for_use(next, "downloaded model")
        .await
        .map_err(|e| {
            core.set_status("error");
            core.log("error", format!("model warm-up failed after download: {e}"));
            e.to_string()
        })?;
    Ok(core.snapshot())
}

#[tauri::command]
async fn select_local_model(
    core: tauri::State<'_, Arc<AppCore>>,
    path: String,
) -> Result<StatusSnapshot, String> {
    let settings = core.settings.lock().clone();
    let next = model_setup::select_local_model(&settings, &path).map_err(|e| e.to_string())?;
    if settings.model_path != next.model_path || settings.mmproj_path != next.mmproj_path {
        core.model.shutdown(&settings).await;
    }
    *core.settings.lock() = next.clone();
    core.log("info", "local model selected and settings updated");
    core.warm_model_for_use(next, "selected model")
        .await
        .map_err(|e| {
            core.set_status("error");
            core.log(
                "error",
                format!("model warm-up failed after selection: {e}"),
            );
            e.to_string()
        })?;
    Ok(core.snapshot())
}

#[tauri::command]
async fn save_settings_cmd(
    core: tauri::State<'_, Arc<AppCore>>,
    settings: Settings,
) -> Result<StatusSnapshot, String> {
    let previous = core.settings.lock().clone();
    settings::save_settings(&settings).map_err(|e| e.to_string())?;
    core.audio
        .buffer()
        .set_max_history(Duration::from_secs(settings.rolling_history_seconds));
    let model_config_changed = previous.model_path != settings.model_path
        || previous.mmproj_path != settings.mmproj_path
        || previous.llama_server_path != settings.llama_server_path
        || previous.llama_device != settings.llama_device
        || previous.server_url != settings.server_url
        || previous.image_tokens != settings.image_tokens
        || previous.context_length_tokens != settings.context_length_tokens;
    if model_config_changed {
        core.model.shutdown(&previous).await;
    }
    let microphone_changed = previous.microphone_device != settings.microphone_device;
    if microphone_changed {
        core.audio.stop();
        core.audio
            .start(&settings.microphone_device)
            .map_err(|e| e.to_string())?;
        core.log("info", format!("audio restarted: {}", core.audio.status()));
    }
    *core.settings.lock() = settings.clone();
    core.log("info", "settings saved");
    if model_config_changed {
        core.warm_model_for_use(settings, "settings change")
            .await
            .map_err(|e| {
                core.set_status("error");
                core.log(
                    "error",
                    format!("model warm-up failed after settings save: {e}"),
                );
                e.to_string()
            })?;
    }
    Ok(core.snapshot())
}

#[tauri::command]
fn calibrate_audio(core: tauri::State<'_, Arc<AppCore>>) -> Result<StatusSnapshot, String> {
    let end = Instant::now();
    let start = end - Duration::from_secs(3);
    let segment = core.audio.buffer().extract(start, end);
    let samples = to_mono_16k(&segment);
    if samples.len() < audio::TARGET_SAMPLE_RATE as usize {
        return Err("not enough microphone history yet; wait a second and try again".to_string());
    }

    let noise_peak = audio::max_window_rms(&samples, 250);
    let threshold = (noise_peak * 1.8 + 0.0015).clamp(0.002, 0.02);
    let mut settings = core.settings.lock().clone();
    settings.vad_rms_threshold = threshold;
    settings.vad_calibrated = true;
    settings::save_settings(&settings).map_err(|e| e.to_string())?;
    *core.settings.lock() = settings;
    core.log(
        "info",
        format!("audio calibrated: ambient peak rms {noise_peak:.4}, VAD threshold {threshold:.4}"),
    );
    Ok(core.snapshot())
}

#[tauri::command]
fn skip_calibration(core: tauri::State<'_, Arc<AppCore>>) -> Result<StatusSnapshot, String> {
    let mut settings = core.settings.lock().clone();
    settings.vad_calibrated = true;
    settings.calibration_prompt_enabled = false;
    settings::save_settings(&settings).map_err(|e| e.to_string())?;
    *core.settings.lock() = settings;
    core.log("info", "audio calibration prompt dismissed");
    Ok(core.snapshot())
}

#[tauri::command]
async fn test_model(core: tauri::State<'_, Arc<AppCore>>) -> Result<StatusSnapshot, String> {
    let settings = core.settings.lock().clone();
    core.model
        .ensure_running(&settings)
        .await
        .map_err(|e| e.to_string())?;
    core.sample_metrics();
    core.log("info", "model health check passed; resource usage sampled");
    Ok(core.snapshot())
}

#[tauri::command]
async fn reset_llama_backend(
    core: tauri::State<'_, Arc<AppCore>>,
) -> Result<StatusSnapshot, String> {
    let settings = core.settings.lock().clone();
    if !settings.managed_server {
        core.log(
            "warn",
            "reset llama.cpp backend requested, but app-managed server is disabled",
        );
        return Err("Reset is only available when App-managed llama.cpp is enabled".to_string());
    }
    core.set_status("warming");
    core.update_overlay(
        "processing",
        Some("Resetting local model backend".to_string()),
    );
    core.log("warn", "resetting app-managed llama.cpp backend");
    core.model.shutdown(&settings).await;
    core.warm_model_for_use(settings, "manual backend reset")
        .await
        .map_err(|e| {
            core.set_status("error");
            core.update_overlay(
                "error",
                Some(short_overlay_error("llama.cpp backend reset failed")),
            );
            core.log("error", format!("llama.cpp backend reset failed: {e}"));
            e.to_string()
        })?;
    core.log("info", "llama.cpp backend reset completed");
    Ok(core.snapshot())
}

#[tauri::command]
fn test_parsing(core: tauri::State<'_, Arc<AppCore>>, input: String) -> StatusSnapshot {
    let settings = core.settings.lock().clone();
    let parsed = parser::parse_output(&input, settings.shortcuts_enabled);
    core.state.lock().parsed_actions = parsed.actions;
    core.log("info", "parser test completed");
    core.snapshot()
}

#[tauri::command]
fn test_audio(core: tauri::State<'_, Arc<AppCore>>) -> StatusSnapshot {
    let end = Instant::now();
    let start = end - Duration::from_secs(2);
    let segment = core.audio.buffer().extract(start, end);
    let samples = to_mono_16k(&segment);
    core.log(
        "info",
        format!(
            "audio test: {} samples, rms {:.4}",
            samples.len(),
            audio::rms(&samples)
        ),
    );
    core.snapshot()
}

#[tauri::command]
fn play_recording(core: tauri::State<'_, Arc<AppCore>>, id: u64) -> Result<StatusSnapshot, String> {
    let recording = core
        .state
        .lock()
        .recordings
        .iter()
        .find(|entry| entry.id == id)
        .cloned()
        .ok_or_else(|| "recording not found".to_string())?;
    play_wav(&recording.audio_path).map_err(|e| e.to_string())?;
    core.log("info", format!("playing recording {}", recording.id));
    Ok(core.snapshot())
}

fn play_wav(path: &str) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let script = format!(
            "Add-Type -AssemblyName System; $p = New-Object System.Media.SoundPlayer '{}'; $p.PlaySync();",
            path.replace('\'', "''")
        );
        std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                &script,
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        anyhow::bail!("recording playback is only implemented on Windows")
    }
}

#[tauri::command]
async fn test_injection(core: tauri::State<'_, Arc<AppCore>>) -> Result<StatusSnapshot, String> {
    let mut settings = core.settings.lock().clone();
    settings.dry_run = true;
    core.inject_actions(
        vec![Action::Text {
            value: "voice keyboard dry run".to_string(),
        }],
        settings,
    )
    .await;
    Ok(core.snapshot())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt::init();
    let core = AppCore::new();
    let core_for_close = core.clone();
    tauri::Builder::default()
        .manage(core.clone())
        .setup(move |app| {
            create_overlay_window(app)?;
            core.set_app(app.handle().clone());
            let core_for_start = core.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(err) = core_for_start.start_workers() {
                    core_for_start.log("error", format!("startup failed: {err}"));
                    core_for_start.set_status("error");
                }
            });
            Ok(())
        })
        .on_window_event(move |window, event| {
            if window.label() == "main" {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let core = core_for_close.clone();
                    tauri::async_runtime::spawn(async move {
                        core.shutdown().await;
                    });
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            start_hook,
            stop_hook,
            set_paused,
            shutdown_app,
            snapshot_context,
            list_audio_input_devices,
            abort_injection,
            confirm_pending,
            confirm_pending_text,
            deny_pending,
            save_feedback_example,
            set_prompt_panel_collapsed,
            dismiss_prompt_panel,
            open_dataset_folder,
            open_models_folder,
            get_model_setup_info,
            download_model_candidate,
            select_local_model,
            save_settings_cmd,
            calibrate_audio,
            skip_calibration,
            test_audio,
            test_model,
            reset_llama_backend,
            test_parsing,
            play_recording,
            test_injection
        ])
        .run(tauri::generate_context!())
        .expect("error while running Voice Keyboard");
}
