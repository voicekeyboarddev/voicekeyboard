use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Action {
    Text { value: String },
    Shortcut { keys: Vec<String> },
    Prompt,
    Agentic,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParsedOutput {
    pub actions: Vec<Action>,
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorScreenshot {
    pub png_base64: String,
    pub width: u32,
    pub height: u32,
    pub cursor_x: i32,
    pub cursor_y: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FocusedTextContext {
    pub source: String,
    pub element_name: Option<String>,
    pub control_type: Option<String>,
    pub class_name: Option<String>,
    #[serde(default)]
    pub automation_id: Option<String>,
    #[serde(default)]
    pub parent_name: Option<String>,
    #[serde(default)]
    pub parent_class: Option<String>,
    #[serde(default)]
    pub parent_control_type: Option<String>,
    pub text_before_cursor: Option<String>,
    pub selected_text: Option<String>,
    pub text_after_cursor: Option<String>,
    pub full_text: Option<String>,
    pub truncated: bool,
    pub cursor_known: bool,
    pub element_bounds: Option<[i32; 4]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowContext {
    pub title: String,
    pub app_name: String,
    pub cursor_x: i32,
    pub cursor_y: i32,
    pub focused_text: Option<FocusedTextContext>,
    pub cursor_screenshot: Option<CursorScreenshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub ts: String,
    pub level: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticLogFile {
    pub name: String,
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLog {
    pub ts: String,
    pub stage: String,
    pub ok: bool,
    pub transcript: String,
    pub output: String,
    pub ttft_ms: Option<f64>,
    pub tokens_per_second: Option<f64>,
    pub total_ms: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInputSnapshot {
    pub ts: String,
    pub stage: String,
    pub endpoint: String,
    pub prompt: String,
    #[serde(default)]
    pub image_attached: bool,
    pub reasoning_mode: Option<String>,
    pub reasoning_budget: Option<i32>,
    pub context: Option<WindowContext>,
    pub audio_path: Option<String>,
    pub audio_duration_ms: Option<u64>,
    pub audio_format: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SystemMetrics {
    pub app_ram_mb: Option<f64>,
    pub server_ram_mb: Option<f64>,
    pub total_ram_mb: Option<f64>,
    pub gpu_util_percent: Option<f64>,
    pub gpu_mem_used_mb: Option<f64>,
    pub gpu_mem_total_mb: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingEntry {
    pub id: u64,
    pub ts: String,
    pub audio_duration_ms: u64,
    pub audio_path: String,
    pub transcript: String,
    pub output: String,
    pub actions: Vec<Action>,
    pub transcription_ttft_ms: Option<f64>,
    pub interpretation_ttft_ms: Option<f64>,
    pub transcription_total_ms: Option<f64>,
    pub interpretation_total_ms: Option<f64>,
    pub context: Option<WindowContext>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PromptPanelKind {
    Prompt,
    Agentic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptPanelState {
    pub kind: PromptPanelKind,
    pub state: String,
    pub title: String,
    pub transcript: String,
    pub source_output: String,
    pub text: String,
    pub delivery: Option<String>,
    pub recording_id: Option<u64>,
    pub can_insert: bool,
    pub can_save_wrong: bool,
    pub collapsed: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusSnapshot {
    pub status: String,
    pub hook_running: bool,
    pub audio_running: bool,
    pub paused: bool,
    pub model_status: String,
    pub current_window: Option<WindowContext>,
    pub transcript: String,
    pub parsed_actions: Vec<Action>,
    pub pending_confirmation: bool,
    pub pending_text: String,
    pub logs: Vec<LogEntry>,
    pub log_files: Vec<DiagnosticLogFile>,
    pub request_logs: Vec<RequestLog>,
    pub model_inputs: Vec<ModelInputSnapshot>,
    pub recordings: Vec<RecordingEntry>,
    pub prompt_panel: Option<PromptPanelState>,
    pub metrics: SystemMetrics,
    pub settings: crate::settings::Settings,
}
