import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./styles.css";

type Action =
  | { type: "text"; value: string }
  | { type: "shortcut"; keys: string[] };

type Settings = {
  audio_chunk_ms: number;
  rolling_history_seconds: number;
  pre_roll_ms: number;
  trigger_hold_ms: number;
  right_click_trigger_enabled: boolean;
  movement_tolerance_px: number;
  vad_rms_threshold: number;
  vad_calibrated: boolean;
  microphone_device: string;
  calibration_prompt_enabled: boolean;
  shortcuts_enabled: boolean;
  context_enabled: boolean;
  dry_run: boolean;
  confirm_large_text_chars: number;
  confirm_close_shortcuts: boolean;
  kill_switch_enabled: boolean;
  injection_delay_ms: number;
  managed_server: boolean;
  server_url: string;
  llama_server_path: string;
  llama_device: string;
  model_path: string;
  mmproj_path: string;
  image_width: number;
  image_height: number;
  image_tokens: number;
  context_length_tokens: number;
  recent_model_paths: string[];
  log_retention_bytes: number;
  common_terms: string;
  spoken_languages: string;
  recent_context_enabled: boolean;
  recent_context_max_requests: number;
  recent_context_window_seconds: number;
  recent_context_max_items: number;
  thinking_handoff_enabled: boolean;
  thinking_handoff_min_chars: number;
  thinking_handoff_reasoning_budget: number;
  thinking_handoff_context_items: number;
};

type SystemMetrics = {
  app_ram_mb?: number | null;
  server_ram_mb?: number | null;
  total_ram_mb?: number | null;
  gpu_util_percent?: number | null;
  gpu_mem_used_mb?: number | null;
  gpu_mem_total_mb?: number | null;
};

type RecordingEntry = {
  id: number;
  ts: string;
  audio_duration_ms: number;
  audio_path: string;
  transcript: string;
  output: string;
  actions: Action[];
  transcription_ttft_ms?: number | null;
  interpretation_ttft_ms?: number | null;
  transcription_total_ms?: number | null;
  interpretation_total_ms?: number | null;
  context?: {
    title: string;
    app_name: string;
    focused_text?: {
      selected_text?: string | null;
      text_before_cursor?: string | null;
      text_after_cursor?: string | null;
      source?: string | null;
      control_type?: string | null;
    } | null;
  } | null;
};

type Snapshot = {
  status: string;
  hook_running: boolean;
  audio_running: boolean;
  paused: boolean;
  model_status: string;
  current_window?: {
    title: string;
    app_name: string;
    cursor_x: number;
    cursor_y: number;
    focused_text?: {
      source: string;
      element_name?: string | null;
      control_type?: string | null;
      class_name?: string | null;
      automation_id?: string | null;
      parent_name?: string | null;
      parent_class?: string | null;
      parent_control_type?: string | null;
      text_before_cursor?: string | null;
      selected_text?: string | null;
      text_after_cursor?: string | null;
      full_text?: string | null;
      truncated: boolean;
      cursor_known: boolean;
      element_bounds?: [number, number, number, number] | null;
    } | null;
    cursor_screenshot?: {
      png_base64: string;
      width: number;
      height: number;
      cursor_x: number;
      cursor_y: number;
    } | null;
  } | null;
  transcript: string;
  parsed_actions: Action[];
  pending_confirmation: boolean;
  pending_text: string;
  logs: { ts: string; level: string; message: string }[];
  request_logs: {
    ts: string;
    stage: string;
    ok: boolean;
    transcript: string;
    output: string;
    ttft_ms?: number | null;
    tokens_per_second?: number | null;
    total_ms?: number | null;
  }[];
  model_inputs: {
    ts: string;
    stage: string;
    endpoint: string;
    prompt: string;
    reasoning_mode?: string | null;
    reasoning_budget?: number | null;
    context?: Snapshot["current_window"];
    audio_path?: string | null;
    audio_duration_ms?: number | null;
    audio_format?: string | null;
  }[];
  recordings: RecordingEntry[];
  metrics: SystemMetrics;
  settings: Settings;
};

type GpuDevice = {
  id: string;
  name: string;
  backend: string;
  memory_total_mb?: number | null;
  memory_free_mb?: number | null;
};

type ModelCandidate = {
  repo: string;
  file: string;
  size_bytes?: number | null;
  size_label: string;
  family: string;
  quant: string;
  min_vram_mb: number;
  recommended: boolean;
  reason: string;
};

type LocalModelFile = {
  path: string;
  name: string;
  size_bytes?: number | null;
  active: boolean;
};

type AudioInputDevice = {
  name: string;
  is_default: boolean;
};

type ModelDownloadProgress = {
  repo: string;
  file: string;
  downloaded_bytes: number;
  total_bytes?: number | null;
  phase: string;
  done: boolean;
};

type ModelSetupInfo = {
  model_present: boolean;
  model_path: string;
  mmproj_path: string;
  models_dir: string;
  gpu_devices: GpuDevice[];
  cpu_only_warning?: string | null;
  candidates: ModelCandidate[];
  local_models: LocalModelFile[];
};

type OverlayState = {
  state: string;
  text: string;
  transcript?: string;
  pending_text?: string;
};

const app = document.querySelector<HTMLDivElement>("#app")!;
const isOverlay = new URLSearchParams(window.location.search).has("overlay");
if (isOverlay) {
  document.documentElement.classList.add("overlay-document");
  document.body.classList.add("overlay-document");
}

let snapshot: Snapshot | null = null;
let activeTab: "main" | "diagnostics" | "settings" = "main";
let overlay = false;
let transcriptToast = "";
let transcriptTimer: number | undefined;
let overlayState: OverlayState = { state: "idle", text: "" };
let parserInput = '{{Ctrl+C}}';
let pendingNegativeId: number | null = null;
let feedbackToast = "";
let feedbackToastTimer: number | undefined;
let modelSetup: ModelSetupInfo | null = null;
let modelSetupLoading = false;
let modelDownloadKey: string | null = null;
let modelDownloadBusy = false;
let downloadProgress: Record<string, ModelDownloadProgress> = {};
let modelSetupDismissed = false;
let audioInputDevices: AudioInputDevice[] = [];

function hasTauriRuntime() {
  return "__TAURI_INTERNALS__" in window;
}

function statusClass(status: string) {
  if (status === "ready") return "ok";
  if (["listening", "processing", "injecting", "confirm", "paused", "interpreting", "specialized-agent"].includes(status)) return "busy";
  if (["blocked", "error", "aborted"].includes(status)) return "bad";
  return "";
}

function stageLabel(stage: string) {
  if (stage === "interpretation-thinking") return "specialized agent";
  return stage;
}

function esc(value: string) {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

function actionLabel(action: Action) {
  if (action.type === "text") return `Text: ${action.value}`;
  return `Shortcut: ${action.keys.join("+")}`;
}

function stripBase64(obj: unknown): unknown {
  if (typeof obj !== "object" || obj === null) return obj;
  if (Array.isArray(obj)) return obj.map(stripBase64);
  const result: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(obj as Record<string, unknown>)) {
    result[k] = k === "png_base64" ? "[image — see preview above]" : stripBase64(v);
  }
  return result;
}

function detectedContextLabel(win?: Snapshot["current_window"] | null) {
  if (!win) return "no window context";
  const app = win.app_name.toLowerCase();
  const title = win.title.toLowerCase();
  if (app.includes("cmd.exe") || title.includes("command prompt")) return "Windows Command Prompt";
  if (app.includes("powershell") || app.includes("pwsh") || title.includes("powershell")) return "PowerShell terminal";
  if (app.includes("windowsterminal") || app.includes("windows terminal")) {
    return title.includes("cmd") || title.includes("command prompt")
      ? "Windows Command Prompt"
      : "PowerShell terminal";
  }

  const text = win.focused_text;
  if (!text) return "generic";
  const isBrowser =
    app.includes("chrome") ||
    app.includes("msedge") ||
    app.includes("firefox") ||
    app.includes("brave") ||
    app.includes("opera") ||
    app.includes("vivaldi");
  const className = (text.class_name ?? "").toLowerCase();
  const name = (text.element_name ?? "").toLowerCase();
  const automationId = (text.automation_id ?? "").toLowerCase();
  const parentName = (text.parent_name ?? "").toLowerCase();
  const parentClass = (text.parent_class ?? "").toLowerCase();
  const controlType = (text.control_type ?? "").toLowerCase();

  if (
    isBrowser &&
    (className.includes("omnibox") ||
      className.includes("urlbar") ||
      automationId.includes("urlbar") ||
      automationId.includes("address") ||
      automationId.includes("omnibox") ||
      automationId === "urlinput" ||
      name.includes("address") ||
      name.includes("location") ||
      name.includes("address and search") ||
      parentName.includes("address") ||
      parentName.includes("navigation") ||
      parentClass.includes("omnibox") ||
      parentClass.includes("urlbar"))
  ) {
    return "browser address/search bar";
  }

  const looksLikeSearch =
    (controlType.includes("edit") || controlType.includes("text") || controlType === "") &&
    (name.includes("search") ||
      automationId.includes("search") ||
      parentName.includes("search") ||
      parentClass.includes("search")) &&
    !name.includes("search results");
  return looksLikeSearch ? "search box" : "generic";
}

function showFeedbackToast(msg: string) {
  feedbackToast = msg;
  if (feedbackToastTimer) window.clearTimeout(feedbackToastTimer);
  feedbackToastTimer = window.setTimeout(() => {
    feedbackToast = "";
    render();
  }, 2000);
  render();
}

function render() {
  if (isOverlay) {
    renderOverlay();
    return;
  }
  const s = snapshot;
  app.innerHTML = `
    <main class="shell">
      <section class="topbar">
        <div>
          <div class="eyebrow">Local LLM Voice Keyboard</div>
          <h1>Voice Keyboard</h1>
        </div>
        <div class="status-pill ${s ? statusClass(s.status) : ""}">
          <span></span>${s?.status ?? "starting"}
        </div>
      </section>

      <nav class="tabs">
        ${tabButton("main", "Main")}
        ${tabButton("diagnostics", "Diagnostics")}
        ${tabButton("settings", "Settings")}
      </nav>

      ${s ? renderTab(s) : `<section class="panel">Loading backend state...</section>`}
      ${s && shouldShowCalibration(s) ? calibrationPrompt(s) : ""}
      ${s && shouldShowModelSetup(s) ? modelSetupPopup(s) : ""}
    </main>
  `;
  bind();
}

function renderOverlay() {
  const title =
    overlayState.state === "recording" ? "Recording" :
    overlayState.state === "processing" ? "Processing" :
    overlayState.state === "transcript" ? "Transcript" :
    overlayState.state === "injecting" ? "Injecting" :
    overlayState.state === "done" ? "Done" :
    overlayState.state === "confirm" ? "Confirm" :
    overlayState.state === "blocked" ? "Blocked" :
    overlayState.state === "error" ? "Error" : "Voice Keyboard";
  const detail =
    overlayState.text ||
    (overlayState.state === "recording" ? "Release mouse to transcribe" :
    overlayState.state === "processing" ? "Listening ended. Local model is thinking." :
    overlayState.state === "injecting" ? "Sending actions to the active app." :
    "Ready");
  if (overlayState.state === "confirm") {
    app.innerHTML = `
      <main class="overlay-shell confirm-overlay">
        <div class="overlay-editor">
          <div class="overlay-editor-head">
            <div>
              <strong>Confirm insert</strong>
              <p>${esc(detail)}</p>
            </div>
          </div>
          <label>
            <span>Transcript</span>
            <input readonly value="${esc(overlayState.transcript || "")}" />
          </label>
          <label>
            <span>Edit before adding</span>
            <textarea id="overlay-pending-text">${esc(overlayState.pending_text || "")}</textarea>
          </label>
          <div class="overlay-editor-actions">
            <button class="secondary" data-overlay-cmd="deny">No</button>
            <button data-overlay-cmd="confirm-text">Yes</button>
          </div>
        </div>
      </main>
    `;
    bindOverlay();
    return;
  }
  app.innerHTML = `
    <main class="overlay-shell ${esc(overlayState.state)}">
      <div class="overlay-glass">
        <div class="orb"><span></span></div>
        <div class="overlay-copy">
          <strong>${esc(title)}</strong>
          <p>${esc(detail)}</p>
        </div>
      </div>
    </main>
  `;
  bindOverlay();
}

function shouldShowCalibration(s: Snapshot) {
  return s.settings.calibration_prompt_enabled && !s.settings.vad_calibrated;
}

function shouldShowModelSetup(s: Snapshot) {
  const present = modelSetup?.model_present ?? false;
  return Boolean(modelSetup && !present && !modelSetupDismissed && !s.model_status.includes("warm"));
}

function calibrationPrompt(s: Snapshot) {
  return `
    <section class="calibration">
      <div>
        <strong>Calibrate microphone sensitivity</strong>
        <p>Stay quiet for three seconds, then calibrate. Current VAD threshold: ${s.settings.vad_rms_threshold.toFixed(4)}</p>
      </div>
      <div class="row">
        <button data-cmd="calibrate-audio">Calibrate</button>
        <button class="secondary" data-cmd="skip-calibration">Not now</button>
      </div>
    </section>
  `;
}

function tabButton(tab: typeof activeTab, label: string) {
  return `<button class="tab ${activeTab === tab ? "active" : ""}" data-tab="${tab}">${label}</button>`;
}

function focusedTextContext(win: NonNullable<Snapshot["current_window"]>) {
  const text = win.focused_text;
  if (!text) return "";
  const bounds = text.element_bounds
    ? `Bounds: ${text.element_bounds[0]}, ${text.element_bounds[1]} - ${text.element_bounds[2]}, ${text.element_bounds[3]}`
    : "";
  return `
    <div class="focused-context">
      <b>Focused text context</b>
      <small>${esc(text.source)}${text.control_type ? ` · ${esc(text.control_type)}` : ""}${text.cursor_known ? " · caret known" : " · caret unknown"}${bounds ? ` · ${esc(bounds)}` : ""}</small>
      ${text.element_name ? `<small>Element: ${esc(text.element_name)}</small>` : ""}
      ${text.text_before_cursor ? `<label>Before cursor</label><pre>${esc(text.text_before_cursor)}</pre>` : ""}
      ${text.selected_text ? `<label>Selected</label><pre>${esc(text.selected_text)}</pre>` : ""}
      ${text.text_after_cursor ? `<label>After cursor</label><pre>${esc(text.text_after_cursor)}</pre>` : ""}
      ${text.full_text ? `<label>Focused text${text.truncated ? " (truncated)" : ""}</label><pre>${esc(text.full_text)}</pre>` : ""}
    </div>
  `;
}

function renderTab(s: Snapshot) {
  if (activeTab === "diagnostics") return diagnostics(s);
  if (activeTab === "settings") return settingsView(s);
  return mainView(s);
}

function mainView(s: Snapshot) {
  const win = s.current_window;
  const specializedActive = s.status === "specialized-agent";
  return `
    ${s.pending_confirmation ? `
      <section class="confirm">
        <div>
          <strong>Confirmation required</strong>
          <p>Review the parsed actions before injection.</p>
        </div>
        <div class="row">
          <button data-cmd="confirm">Inject</button>
          <button class="secondary" data-cmd="deny">Deny</button>
        </div>
      </section>
    ` : ""}
    <section class="grid">
      <div class="panel main-panel">
        <div class="toolbar">
          <button data-cmd="start">Start hook</button>
          <button class="secondary" data-cmd="stop">Stop hook</button>
          <button class="secondary" data-cmd="${s.paused ? "resume" : "pause"}">${s.paused ? "Resume" : "Pause"}</button>
          <button class="secondary" data-cmd="calibrate-audio">Calibrate mic</button>
          <button class="secondary" data-cmd="context">Snapshot context</button>
          <button class="danger" data-cmd="abort">Abort injection</button>
          <button class="danger" data-cmd="exit-app">Stop app</button>
        </div>
        <div class="info-grid">
          <div><span>Audio</span><strong>${s.audio_running ? "running" : "stopped"}</strong></div>
          <div><span>Hook</span><strong>${s.hook_running ? "enabled" : "disabled"}</strong></div>
          <div><span>Model</span><strong>${esc(s.model_status)}</strong></div>
          <div><span>Paused</span><strong>${s.paused ? "yes" : "no"}</strong></div>
        </div>
        ${specializedActive ? `
          <section class="confirm" style="margin-top:12px">
            <div>
              <strong>Specialized agent active</strong>
              <p>Passing the current rewrite or translation task to the slower reasoning path.</p>
            </div>
          </section>
        ` : ""}
        <h2>Resource Use</h2>
        ${metricsGrid(s.metrics)}
        <h2>Current Window</h2>
        <div class="window-box">
          <strong>${esc(win?.app_name ?? "unknown")}</strong>
          <span>${esc(win?.title ?? "No foreground context captured")}</span>
          <small>${win ? `Mouse cursor x=${win.cursor_x}, y=${win.cursor_y}` : ""}</small>
          ${win ? focusedTextContext(win) : ""}
          ${win?.cursor_screenshot ? `
            <div class="cursor-context">
              <b>Cursor context screenshot</b>
              <small>Red marker: cursor area inside the captured app context</small>
              <img
                alt="Cursor-centered context screenshot"
                width="${win.cursor_screenshot.width}"
                height="${win.cursor_screenshot.height}"
                src="data:image/png;base64,${win.cursor_screenshot.png_base64}"
              />
            </div>
          ` : ""}
        </div>
        <h2>Transcript</h2>
        <pre class="output">${esc(s.transcript || "No transcript yet")}</pre>
        <h2>Parsed Action Preview</h2>
        <div class="actions">
          ${s.parsed_actions.length
            ? s.parsed_actions.map((a) => `<div>${esc(actionLabel(a))}</div>`).join("")
            : `<div class="muted">No parsed actions yet</div>`}
        </div>
        <h2>Previous Recordings</h2>
        ${recordingsTable(s)}
        <h2>Recent Requests</h2>
        ${requestTable(s)}
      </div>
      <div class="panel log-panel">
        <h2>Live Log</h2>
        ${logs(s)}
      </div>
    </section>
  `;
}

function diagnostics(s: Snapshot) {
  return `
    <section class="grid">
      <div class="panel">
        <h2>System Status</h2>
        <div class="info-grid diagnostics">
          <div><span>Runtime</span><strong>${esc(s.status)}</strong></div>
          <div><span>Audio</span><strong>${s.audio_running ? "streaming" : "stopped"}</strong></div>
          <div><span>Model</span><strong>${esc(s.model_status)}</strong></div>
          <div><span>Trigger</span><strong>${s.paused ? "paused" : s.hook_running ? "enabled" : "disabled"}</strong></div>
          <div><span>Detected context</span><strong>${esc(detectedContextLabel(s.current_window))}</strong></div>
        </div>
        <h2>Resource Use</h2>
        ${metricsGrid(s.metrics)}
        <div class="toolbar stacked">
          <button data-cmd="test-audio">Test audio input</button>
          <button data-cmd="test-model">Test model response</button>
          <button data-cmd="test-injection">Test injection dry-run</button>
          <button data-cmd="calibrate-audio">Calibrate mic sensitivity</button>
          <button class="secondary" data-cmd="open-models-folder">Open models folder</button>
        </div>
        <label class="field">
          <span>Parser input</span>
          <input id="parser-input" value="${esc(parserInput)}" />
        </label>
        <button data-cmd="test-parser">Test parsing</button>
        <h2>Previous Recordings</h2>
        <div class="toolbar">
          <button class="secondary" data-cmd="open-dataset-folder">Open dataset folder</button>
          ${feedbackToast ? `<span class="feedback-toast">${esc(feedbackToast)}</span>` : ""}
        </div>
        ${recordingsTable(s)}
        <h2>Exact Model Inputs</h2>
        ${modelInputs(s)}
        <h2>Recent Model Requests</h2>
        ${requestTable(s)}
      </div>
      <div class="panel log-panel">
        <h2>Diagnostics Log</h2>
        ${logs(s)}
      </div>
    </section>
  `;
}

function metricsGrid(metrics: SystemMetrics) {
  const gpuMem =
    typeof metrics.gpu_mem_used_mb === "number" && typeof metrics.gpu_mem_total_mb === "number"
      ? `${formatMb(metrics.gpu_mem_used_mb)} / ${formatMb(metrics.gpu_mem_total_mb)}`
      : "-";
  return `
    <div class="info-grid metrics-grid">
      <div><span>App RAM</span><strong>${formatMb(metrics.app_ram_mb)}</strong></div>
      <div><span>llama RAM</span><strong>${formatMb(metrics.server_ram_mb)}</strong></div>
      <div><span>Total RAM</span><strong>${formatMb(metrics.total_ram_mb)}</strong></div>
      <div><span>GPU</span><strong>${formatPercent(metrics.gpu_util_percent)}</strong></div>
      <div><span>GPU VRAM</span><strong>${gpuMem}</strong></div>
    </div>
  `;
}

function recordingsTable(s: Snapshot) {
  if (!s.recordings.length) {
    return `<div class="muted">No completed recordings yet</div>`;
  }
  return `
    <div class="recording-table">
      <div class="recording-row head">
        <span>Time</span><span>Audio</span><span>Trans TTFT</span><span>Interp TTFT</span><span>Transcript</span><span>Actions</span><span>Audio</span><span>Dataset</span>
      </div>
      ${s.recordings.slice(0, 8).map((r) => `
        <div class="recording-row">
          <span>${new Date(r.ts).toLocaleTimeString()}</span>
          <span>${formatDuration(r.audio_duration_ms)}</span>
          <span>${formatMs(r.transcription_ttft_ms)}</span>
          <span>${formatMs(r.interpretation_ttft_ms)}</span>
          <span>${esc(r.transcript).slice(0, 220)}</span>
          <span>${esc(r.actions.map(actionLabel).join("; ")).slice(0, 140)}</span>
          <span><button class="mini" data-play-recording="${r.id}">Play</button></span>
          <span class="dataset-btns">
            <button class="mini pos" data-save-correct="${r.id}" title="Save as correct example">+</button>
            <button class="mini neg" data-save-wrong="${r.id}" title="Save as wrong example">−</button>
          </span>
        </div>
        ${r.context ? (() => {
          const ft = r.context.focused_text;
          const before = ft?.text_before_cursor ?? "";
          const after = ft?.text_after_cursor ?? "";
          const sel = ft?.selected_text ?? "";
          const hasSelection = !!sel;
          const hasField = !!(before || after || sel);
          return `
            <div class="recording-ctx">
              <div class="ctx-line">
                <span class="ctx-tag">${esc(r.context.app_name)}</span>
                ${hasSelection
                  ? `<span class="ctx-sel-badge"><span class="ctx-sel-label">SELECTED</span><span class="ctx-selected-text">${esc(sel.slice(0, 200))}</span></span>`
                  : `<span class="ctx-cursor-badge">no selection</span>`}
              </div>
              <div class="ctx-line ctx-fieldstate">
                <span class="ctx-cursor-label">field:</span>
                ${hasField
                  ? `<code class="ctx-content"><span class="ctx-before">${esc(before.slice(-80))}</span>${hasSelection ? `<mark class="ctx-sel-inline">${esc(sel.slice(0, 80))}</mark>` : `<span class="ctx-cursor">┃</span>`}<span class="ctx-after">${esc(after.slice(0, 80))}</span></code>`
                  : `<code class="ctx-content ctx-empty">(empty)</code>`}
              </div>
            </div>
          `;
        })() : ""}
        ${pendingNegativeId === r.id ? `
          <div class="recording-row negative-input-row">
            <span class="negative-input-cell">
              <label>Expected output:</label>
              <input id="expected-answer-${r.id}" type="text" placeholder="type the correct output…" />
              <button class="mini" data-save-wrong-confirm="${r.id}">Save</button>
              <button class="mini secondary" data-save-cancel>Cancel</button>
            </span>
          </div>
        ` : ""}
      `).join("")}
    </div>
  `;
}

function requestTable(s: Snapshot) {
  if (!s.request_logs.length) {
    return `<div class="muted">No model requests yet</div>`;
  }
  return `
    <div class="request-table">
      <div class="request-row head">
        <span>Time</span><span>Stage</span><span>TTFT</span><span>Tok/s</span><span>Transcript / Output</span>
      </div>
      ${s.request_logs.slice(0, 12).map((r) => `
        <div class="request-row ${r.ok ? "ok" : "bad"}">
          <span>${new Date(r.ts).toLocaleTimeString()}</span>
          <span>${esc(stageLabel(r.stage))}</span>
          <span>${formatMs(r.ttft_ms)}</span>
          <span>${formatNumber(r.tokens_per_second)}</span>
          <span>${esc((r.transcript || r.output).slice(0, 180))}</span>
        </div>
      `).join("")}
    </div>
  `;
}

function modelInputs(s: Snapshot) {
  if (!s.model_inputs.length) {
    return `<div class="muted">No captured model inputs yet</div>`;
  }
  return `
    <div class="model-inputs">
      ${s.model_inputs.slice(0, 4).map((input, i) => `
        <details class="model-input" ${i === 0 ? "open" : ""}>
          <summary>
            <span>${new Date(input.ts).toLocaleTimeString()}</span>
            <strong>${esc(stageLabel(input.stage))}</strong>
            <small>${esc(input.audio_format || "text")}${input.audio_duration_ms ? ` · ${formatDuration(input.audio_duration_ms)}` : ""}${input.reasoning_mode ? ` · reasoning ${esc(input.reasoning_mode)}${typeof input.reasoning_budget === "number" ? ` (${input.reasoning_budget})` : ""}` : ""}</small>
          </summary>
          ${input.context ? `
            <div class="context-summary">
              <div class="ctx-row"><span>Window</span><strong>${esc(input.context.app_name)} — ${esc(input.context.title)}</strong></div>
              <div class="ctx-row"><span>Detected context</span><strong>${esc(detectedContextLabel(input.context))}</strong></div>
              ${input.context.focused_text?.selected_text ? `<div class="ctx-row ctx-selected"><span>Selected text</span><strong>${esc(input.context.focused_text.selected_text)}</strong></div>` : ""}
              ${input.context.focused_text?.text_before_cursor ? `<div class="ctx-row"><span>Before cursor</span><code>${esc(input.context.focused_text.text_before_cursor.slice(-120))}</code></div>` : ""}
              ${input.context.focused_text?.text_after_cursor ? `<div class="ctx-row"><span>After cursor</span><code>${esc(input.context.focused_text.text_after_cursor.slice(0, 80))}</code></div>` : ""}
              ${input.context.focused_text?.source ? `<div class="ctx-row"><span>Source</span><span>${esc(input.context.focused_text.source)}${input.context.focused_text.control_type ? " · " + esc(input.context.focused_text.control_type) : ""}${input.context.focused_text.class_name ? " · " + esc(input.context.focused_text.class_name) : ""}</span></div>` : ""}
              ${input.context.focused_text?.automation_id ? `<div class="ctx-row"><span>AutomationId</span><span>${esc(input.context.focused_text.automation_id)}</span></div>` : ""}
              ${input.context.focused_text?.parent_name || input.context.focused_text?.parent_control_type ? `<div class="ctx-row"><span>Parent</span><span>${esc(input.context.focused_text?.parent_name ?? "")}${input.context.focused_text?.parent_control_type ? " · " + esc(input.context.focused_text.parent_control_type) : ""}</span></div>` : ""}
            </div>
          ` : `<div class="muted" style="font-size:12px;padding:4px 0">No window context captured</div>`}
          <label>Endpoint</label>
          <pre>${esc(input.endpoint)}</pre>
          ${input.reasoning_mode ? `<label>Reasoning</label><pre>${esc(input.reasoning_mode)}${typeof input.reasoning_budget === "number" ? ` (budget ${input.reasoning_budget})` : ""}</pre>` : ""}
          ${input.audio_path ? `<label>Audio input</label><pre>${esc(input.audio_path)}</pre>` : ""}
          <label>Prompt sent</label>
          <pre>${esc(input.prompt)}</pre>
          ${input.context ? `
            <details class="ctx-json-toggle">
              <summary>Full context JSON</summary>
              <pre class="selectable">${esc(JSON.stringify(stripBase64(input.context), null, 2))}</pre>
            </details>
          ` : ""}
          ${input.context?.cursor_screenshot ? `
            <label>Image sent to model</label>
            <img
              class="model-input-image"
              alt="Cursor context image"
              width="${input.context.cursor_screenshot.width}"
              height="${input.context.cursor_screenshot.height}"
              src="data:image/png;base64,${input.context.cursor_screenshot.png_base64}"
            />
          ` : ""}
        </details>
      `).join("")}
    </div>
  `;
}

function formatMs(value?: number | null) {
  return typeof value === "number" ? `${value.toFixed(0)} ms` : "-";
}

function formatMb(value?: number | null) {
  return typeof value === "number" ? `${value.toFixed(0)} MB` : "-";
}

function formatBytes(value?: number | null) {
  if (typeof value !== "number") return "-";
  const gb = value / 1024 / 1024 / 1024;
  if (gb >= 1) return `${gb.toFixed(2)} GB`;
  return `${(value / 1024 / 1024).toFixed(0)} MB`;
}

function bestGpuMemoryMb(setup?: ModelSetupInfo | null) {
  return setup?.gpu_devices
    .map((gpu) => gpu.memory_free_mb ?? gpu.memory_total_mb ?? 0)
    .reduce((best, value) => Math.max(best, value), 0) ?? 0;
}

function gpuSummary(setup?: ModelSetupInfo | null) {
  const gpu = setup?.gpu_devices
    .slice()
    .sort((a, b) => (b.memory_total_mb ?? 0) - (a.memory_total_mb ?? 0))[0];
  if (!gpu) return "No GPU memory was detected. You can still choose an existing GGUF or download a model manually.";
  const memory = gpu.memory_free_mb ?? gpu.memory_total_mb;
  const memoryText = typeof memory === "number" ? `${formatMb(memory)} GPU memory detected` : "GPU detected";
  return `${esc(gpu.name)} - ${memoryText}`;
}

function modelChoiceList(setup: ModelSetupInfo | null) {
  if (!setup?.candidates.length) {
    return `<div class="muted">Checking available models...</div>`;
  }
  const gpuMb = bestGpuMemoryMb(setup);
  return setup.candidates.map((candidate, index) => {
    const supported = gpuMb > 0 && candidate.min_vram_mb <= gpuMb;
    const key = modelKey(candidate);
    const progress = downloadProgress[key];
    const isDownloading = modelDownloadKey === key && progress && !progress.done;
    const progressText = progressTextFor(progress);
    return `
      <div class="model-choice ${supported ? "supported" : "unavailable"}">
        <div class="model-choice-copy">
          <div class="model-choice-title">
            <strong>${esc(candidate.family)} ${esc(candidate.quant)}</strong>
            <span class="model-status ${supported ? "supported" : "unsupported"}">${supported ? "Supported" : "Probably not supported"}</span>
          </div>
          <span>${esc(candidate.size_label || formatBytes(candidate.size_bytes))} download - needs ${formatMb(candidate.min_vram_mb)} GPU memory</span>
          <small>${esc(candidate.repo)} / ${esc(candidate.file)}</small>
          ${progress ? `<div class="download-progress"><span style="width:${downloadPercent(progress)}%"></span></div><small>${esc(progressText)}</small>` : ""}
        </div>
        <button
          class="secondary"
          data-download-model="${index}"
          ${modelDownloadKey && !isDownloading ? "disabled" : ""}
        >${isDownloading ? "Downloading" : "Download"}</button>
      </div>
    `;
  }).join("");
}

function modelKey(candidate: Pick<ModelCandidate, "repo" | "file">) {
  return `${candidate.repo}/${candidate.file}`;
}

function progressKey(progress: Pick<ModelDownloadProgress, "repo" | "file">) {
  return `${progress.repo}/${progress.file}`;
}

function downloadPercent(progress?: ModelDownloadProgress) {
  if (!progress?.total_bytes) return progress?.done ? 100 : 18;
  return Math.max(1, Math.min(100, Math.round((progress.downloaded_bytes / progress.total_bytes) * 100)));
}

function progressTextFor(progress?: ModelDownloadProgress) {
  if (!progress) return "";
  if (progress.done) return "Download complete";
  if (progress.total_bytes) {
    return `${formatBytes(progress.downloaded_bytes)} of ${formatBytes(progress.total_bytes)} downloaded`;
  }
  return progress.phase === "starting" ? "Starting download..." : `${formatBytes(progress.downloaded_bytes)} downloaded`;
}

function isEditingModelSetupField() {
  const active = document.activeElement;
  return active instanceof HTMLElement && active.matches("[data-manual-model-path], [data-hf-token]");
}

function localModelList(setup: ModelSetupInfo | null) {
  const local = setup?.local_models ?? [];
  const rows = local.length
    ? local.map((model, index) => `
        <div class="local-model ${model.active ? "active" : ""}">
          <div class="local-model-copy">
            <strong>${esc(model.name)}</strong>
            <span>${formatBytes(model.size_bytes)} - ${esc(displayPath(model.path))}</span>
          </div>
          <button class="secondary" data-select-local-model="${index}" ${model.active ? "disabled" : ""}>
            ${model.active ? "Selected" : "Use"}
          </button>
        </div>
      `).join("")
    : `<div class="muted">No downloaded or remembered GGUF models found yet.</div>`;
  return `
    <div class="local-models">
      <div class="local-model-head">
        <strong>Available local models</strong>
        <button class="secondary" data-cmd="open-models-folder">Open folder</button>
      </div>
      ${rows}
    </div>
  `;
}

function manualModelPicker() {
  return `
    <label class="field manual-model-field">
      <span>GGUF file path</span>
      <div class="manual-model-row">
        <input data-manual-model-path type="text" placeholder="D:\\Models\\model.gguf" />
        <button class="secondary" data-use-manual-model>Use path</button>
      </div>
    </label>
  `;
}

function pathFileName(path: string) {
  return displayPath(path).split(/[\\/]/).filter(Boolean).pop() || displayPath(path);
}

function displayPath(path: string) {
  return path.replace(/^\\\\\?\\UNC\\/i, "\\\\").replace(/^\\\\\?\\/i, "");
}

function settingDisplayValue(settings: Settings, key: keyof Settings) {
  const value = settings[key];
  if (typeof value === "string" && String(key).endsWith("_path")) return displayPath(value);
  return String(value);
}

function currentModelSection(s: Snapshot, setup: ModelSetupInfo | null) {
  const path = s.settings.model_path || setup?.model_path || "";
  const present = setup?.model_present ?? (Boolean(path) && !s.model_status.includes("missing"));
  const active = setup?.local_models.find((model) => model.active);
  return `
    <section class="model-setup-section current-model-section">
      <div class="model-section-head">
        <div>
          <strong>Current model</strong>
          <span>${present ? "Loaded from your saved settings" : "No model selected"}</span>
        </div>
        ${present ? `<span class="model-status supported">Selected</span>` : ""}
      </div>
      ${present ? `
        <div class="current-model-card">
          <strong>${esc(active?.name || pathFileName(path))}</strong>
          <span>${esc(displayPath(path))}</span>
        </div>
      ` : `<div class="muted">Choose an existing GGUF file or download one below.</div>`}
    </section>
  `;
}

function existingModelSection(setup: ModelSetupInfo | null) {
  return `
    <section class="model-setup-section">
      <div class="model-section-head">
        <div>
          <strong>Use an existing GGUF</strong>
          <span>Select a downloaded model or paste the full path to a GGUF file.</span>
        </div>
      </div>
      ${manualModelPicker()}
      ${localModelList(setup)}
    </section>
  `;
}

function downloadModelSection(setup: ModelSetupInfo | null) {
  return `
    <section class="model-setup-section">
      <div class="model-section-head">
        <div>
          <strong>Download a GGUF</strong>
          <span>${gpuSummary(setup)}</span>
        </div>
      </div>
      <label class="field">
        <span>Hugging Face token (optional)</span>
        <input data-hf-token type="password" autocomplete="off" placeholder="hf_..." />
      </label>
      <div class="model-scroll"><div class="model-choices">${modelChoiceList(setup)}</div></div>
    </section>
  `;
}

function formatPercent(value?: number | null) {
  return typeof value === "number" ? `${value.toFixed(0)}%` : "-";
}

function formatDuration(valueMs: number) {
  if (valueMs < 1000) return `${valueMs} ms`;
  return `${(valueMs / 1000).toFixed(1)} s`;
}

function formatNumber(value?: number | null) {
  return typeof value === "number" ? value.toFixed(1) : "-";
}

function settingsView(s: Snapshot) {
  const fields: Array<[keyof Settings, string, string]> = [
    ["trigger_hold_ms", "Trigger hold ms", "number"],
    ["movement_tolerance_px", "Movement tolerance px", "number"],
    ["pre_roll_ms", "Audio pre-roll ms", "number"],
    ["rolling_history_seconds", "Rolling history seconds", "number"],
    ["vad_rms_threshold", "VAD RMS threshold", "number"],
    ["image_width", "Image width px", "number"],
    ["image_height", "Image height px", "number"],
    ["image_tokens", "Image token budget", "number"],
    ["context_length_tokens", "Model context tokens", "number"],
    ["confirm_large_text_chars", "Confirm text length", "number"],
    ["injection_delay_ms", "Injection delay ms", "number"],
    ["spoken_languages", "Spoken languages", "text"],
    ["recent_context_max_requests", "Recent context request count", "number"],
    ["recent_context_window_seconds", "Recent context window sec", "number"],
    ["recent_context_max_items", "Recent context item cap", "number"],
    ["thinking_handoff_min_chars", "Thinking handoff min chars", "number"],
    ["thinking_handoff_reasoning_budget", "Thinking reasoning budget", "number"],
    ["thinking_handoff_context_items", "Thinking context items", "number"]
  ];
  const advancedFields: Array<[keyof Settings, string, string]> = [
    ["server_url", "Server URL", "text"],
    ["llama_server_path", "Runtime path", "text"],
    ["llama_device", "GPU device override", "text"],
    ["model_path", "Model file path", "text"],
    ["mmproj_path", "Projector file path", "text"]
  ];
  const toggles: Array<[keyof Settings, string]> = [
    ["managed_server", "App-managed llama.cpp"],
    ["right_click_trigger_enabled", "Right-click hold trigger"],
    ["shortcuts_enabled", "Enable shortcuts"],
    ["context_enabled", "Context awareness"],
    ["calibration_prompt_enabled", "Show calibration prompt"],
    ["dry_run", "Dry-run mode"],
    ["recent_context_enabled", "Recent context"],
    ["thinking_handoff_enabled", "Thinking handoff"],
    ["confirm_close_shortcuts", "Confirm close shortcuts"],
    ["kill_switch_enabled", "Kill switch"]
  ];

  return `
    <section class="panel settings">
      <h2>Settings</h2>
      ${modelSetupPanel(s)}
      ${audioInputSection(s)}
      <div class="settings-grid">
        ${fields
          .map(([key, label, type]) => `
            <label class="field">
              <span>${label}</span>
              <input data-setting="${String(key)}" type="${type}" step="any" value="${esc(settingDisplayValue(s.settings, key))}" />
            </label>
          `)
          .join("")}
      </div>
      <div class="toggle-grid">
        ${toggles
          .map(([key, label]) => `
            <label class="toggle">
              <input data-setting="${String(key)}" type="checkbox" ${s.settings[key] ? "checked" : ""} />
              <span>${label}</span>
            </label>
          `)
          .join("")}
      </div>
      <label class="field common-terms-field">
        <span>Common terms (added to prompt)</span>
        <textarea data-setting="common_terms" rows="4" placeholder="e.g. My email is ashish4reading@gmail.com&#10;Company: SNT Achievement&#10;Phone: +91-9999999999">${esc(s.settings.common_terms ?? "")}</textarea>
      </label>
      <details class="advanced-settings">
        <summary>Advanced runtime paths</summary>
        <div class="settings-grid">
          ${advancedFields
            .map(([key, label, type]) => `
              <label class="field">
                <span>${label}</span>
                <input data-setting="${String(key)}" type="${type}" step="any" value="${esc(settingDisplayValue(s.settings, key))}" />
              </label>
            `)
            .join("")}
        </div>
      </details>
      <button data-cmd="save-settings">Save settings</button>
    </section>
  `;
}

function audioInputSection(s: Snapshot) {
  const selected = s.settings.microphone_device || "";
  const options = [
    `<option value="" ${selected ? "" : "selected"}>System default microphone</option>`,
    ...audioInputDevices.map((device) => `
      <option value="${esc(device.name)}" ${selected === device.name ? "selected" : ""}>
        ${esc(device.name)}${device.is_default ? " (default)" : ""}
      </option>
    `)
  ].join("");

  return `
    <section class="model-setup-section">
      <div class="model-section-head">
        <div>
          <strong>Microphone</strong>
          <span>${esc(s.audio_running ? "Audio capture is running" : "Audio capture is stopped")}</span>
        </div>
        <button class="secondary" data-cmd="refresh-audio-devices">Refresh devices</button>
      </div>
      <label class="field">
        <span>Input device</span>
        <select data-setting="microphone_device">${options}</select>
      </label>
    </section>
  `;
}

function modelSetupPanel(s: Snapshot) {
  const setup = modelSetup;
  const present = setup?.model_present ?? (Boolean(s.settings.model_path) && !s.model_status.includes("missing"));

  return `
    <div class="model-setup ${present ? "" : "needs-model"}">
      <div class="setup-head">
        <div>
          <strong>${present ? "Model configured" : "Model setup required"}</strong>
          <span>${present ? "Current model is highlighted below." : "Choose a model before local transcription can warm up."}</span>
        </div>
        <button class="secondary" data-cmd="refresh-model-setup" ${modelSetupLoading ? "disabled" : ""}>
          ${modelSetupLoading ? "Checking..." : "Check models"}
        </button>
      </div>
      ${currentModelSection(s, setup)}
      ${existingModelSection(setup)}
      ${downloadModelSection(setup)}
    </div>
  `;
}

function modelSetupPopup(_s: Snapshot) {
  const setup = modelSetup;
  const s = snapshot ?? _s;
  return `
    <div class="model-setup-backdrop">
      <section class="model-setup-popup">
      <div class="setup-head">
        <div>
          <strong>Download a local AI model</strong>
          <span>${gpuSummary(setup)}</span>
        </div>
          <div class="row">
            <button class="secondary" data-cmd="refresh-model-setup" ${modelSetupLoading ? "disabled" : ""}>
              ${modelSetupLoading ? "Checking..." : "Refresh"}
            </button>
            <button class="secondary" data-cmd="dismiss-model-setup">Close</button>
          </div>
      </div>
        ${setup?.cpu_only_warning ? `<div class="setup-warning">${esc(setup.cpu_only_warning)}</div>` : ""}
        ${currentModelSection(s, setup)}
        ${existingModelSection(setup)}
        ${downloadModelSection(setup)}
      </section>
    </div>
  `;
}

function modelSetupView(s: Snapshot) {
  const setup = modelSetup;
  const present = setup?.model_present ?? (Boolean(s.settings.model_path) && !s.model_status.includes("missing"));
  const gpuRows = setup?.gpu_devices.length
    ? setup.gpu_devices.map((gpu) => `
        <div class="gpu-row">
          <strong>${esc(gpu.id)}</strong>
          <span>${esc(gpu.name)} · ${esc(gpu.backend)} · ${formatMb(gpu.memory_free_mb)} free / ${formatMb(gpu.memory_total_mb)} total</span>
        </div>
      `).join("")
    : `<div class="setup-warning">No llama.cpp GPU device detected. CPU mode is allowed, but it will be slow.</div>`;
  const candidates = setup?.candidates.length
    ? setup.candidates.map((candidate, index) => `
        <div class="model-choice">
          <div>
            <strong>${esc(candidate.family)} ${esc(candidate.quant)}</strong>
            <span>${esc(candidate.repo)} / ${esc(candidate.file)}</span>
            <small>${formatBytes(candidate.size_bytes)} · ${esc(candidate.reason)}</small>
          </div>
          <button
            class="secondary"
            data-download-model="${index}"
            ${modelDownloadBusy ? "disabled" : ""}
          >Download</button>
        </div>
      `).join("")
    : `<div class="muted">No Hugging Face GGUF candidates loaded yet.</div>`;

  return `
    <div class="model-setup ${present ? "" : "needs-model"}">
      <div class="setup-head">
        <div>
          <strong>${present ? "Model configured" : "Model setup required"}</strong>
          <span>${present ? "Current model is highlighted below." : "Choose a GGUF model before local transcription can warm up."}</span>
        </div>
        <button class="secondary" data-cmd="refresh-model-setup" ${modelSetupLoading ? "disabled" : ""}>
          ${modelSetupLoading ? "Checking..." : "Check models"}
        </button>
      </div>
      ${currentModelSection(s, setup)}
      ${localModelList(setup)}
      <label class="field">
        <span>Hugging Face token (optional)</span>
        <input data-hf-token type="password" autocomplete="off" placeholder="hf_..." />
      </label>
      <div class="model-choices">${candidates}</div>
    </div>
  `;
}

function logs(s: Snapshot) {
  return `
    <div class="logs">
      ${s.logs.length
        ? s.logs.map((l) => `
          <div class="log ${esc(l.level)}">
            <time>${new Date(l.ts).toLocaleTimeString()}</time>
            <strong>${esc(l.level)}</strong>
            <span>${esc(l.message)}</span>
          </div>
        `).join("")
        : `<div class="muted">No log entries yet</div>`}
    </div>
  `;
}

function bind() {
  document.querySelectorAll<HTMLButtonElement>("[data-tab]").forEach((btn) => {
    btn.addEventListener("click", () => {
      activeTab = btn.dataset.tab as typeof activeTab;
      render();
    });
  });
  document.querySelectorAll<HTMLButtonElement>("[data-cmd]").forEach((btn) => {
    btn.addEventListener("click", () => runCommand(btn.dataset.cmd!));
  });
  document.querySelectorAll<HTMLButtonElement>("[data-play-recording]").forEach((btn) => {
    btn.addEventListener("click", () => playRecording(Number(btn.dataset.playRecording)));
  });
  document.querySelectorAll<HTMLButtonElement>("[data-save-correct]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const id = Number(btn.dataset.saveCorrect);
      try {
        snapshot = await invoke("save_feedback_example", { correct: true, recordingId: id, expectedOutput: null });
        showFeedbackToast("Saved as correct example");
      } catch (error) { alert(String(error)); }
    });
  });
  document.querySelectorAll<HTMLButtonElement>("[data-save-wrong]").forEach((btn) => {
    btn.addEventListener("click", () => {
      pendingNegativeId = Number(btn.dataset.saveWrong);
      render();
    });
  });
  document.querySelectorAll<HTMLButtonElement>("[data-save-wrong-confirm]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const id = Number(btn.dataset.saveWrongConfirm);
      const expected = (document.getElementById(`expected-answer-${id}`) as HTMLInputElement | null)?.value ?? "";
      pendingNegativeId = null;
      try {
        snapshot = await invoke("save_feedback_example", { correct: false, recordingId: id, expectedOutput: expected || null });
        showFeedbackToast("Saved as wrong example");
      } catch (error) { alert(String(error)); }
    });
  });
  document.querySelectorAll<HTMLButtonElement>("[data-save-cancel]").forEach((btn) => {
    btn.addEventListener("click", () => {
      pendingNegativeId = null;
      render();
    });
  });
  document.querySelectorAll<HTMLButtonElement>("[data-download-model]").forEach((btn) => {
    btn.addEventListener("click", () => downloadModel(Number(btn.dataset.downloadModel)));
  });
  document.querySelectorAll<HTMLButtonElement>("[data-select-local-model]").forEach((btn) => {
    btn.addEventListener("click", () => selectLocalModel(Number(btn.dataset.selectLocalModel)));
  });
  document.querySelectorAll<HTMLButtonElement>("[data-use-manual-model]").forEach((btn) => {
    btn.addEventListener("click", () => {
      const input = btn.closest(".manual-model-field")?.querySelector<HTMLInputElement>("[data-manual-model-path]");
      if (input?.value.trim()) selectLocalModelPath(input.value.trim());
    });
  });
}

function bindOverlay() {
  document.querySelectorAll<HTMLButtonElement>("[data-overlay-cmd]").forEach((btn) => {
    btn.addEventListener("click", () => runOverlayCommand(btn.dataset.overlayCmd!));
  });
}

async function playRecording(id: number) {
  try {
    snapshot = await invoke("play_recording", { id });
    render();
  } catch (error) {
    console.error(error);
    alert(String(error));
  }
}

async function runCommand(cmd: string) {
  try {
    let next: Snapshot | null = null;
    if (cmd === "start") next = await invoke("start_hook");
    if (cmd === "stop") next = await invoke("stop_hook");
    if (cmd === "pause") next = await invoke("set_paused", { paused: true });
    if (cmd === "resume") next = await invoke("set_paused", { paused: false });
    if (cmd === "exit-app") {
      await invoke("shutdown_app");
      return;
    }
    if (cmd === "context") next = await invoke("snapshot_context");
    if (cmd === "abort") next = await invoke("abort_injection");
    if (cmd === "confirm") next = await invoke("confirm_pending");
    if (cmd === "deny") next = await invoke("deny_pending");
    if (cmd === "calibrate-audio") next = await invoke("calibrate_audio");
    if (cmd === "skip-calibration") next = await invoke("skip_calibration");
    if (cmd === "test-audio") next = await invoke("test_audio");
    if (cmd === "test-model") next = await invoke("test_model");
    if (cmd === "test-injection") next = await invoke("test_injection");
    if (cmd === "open-dataset-folder") { await invoke("open_dataset_folder"); return; }
    if (cmd === "open-models-folder") { await invoke("open_models_folder"); return; }
    if (cmd === "refresh-model-setup") {
      await refreshModelSetup();
      return;
    }
    if (cmd === "refresh-audio-devices") {
      await refreshAudioInputDevices();
      render();
      return;
    }
    if (cmd === "dismiss-model-setup") {
      modelSetupDismissed = true;
      render();
      return;
    }
    if (cmd === "test-parser") {
      parserInput = document.querySelector<HTMLInputElement>("#parser-input")?.value ?? parserInput;
      next = await invoke("test_parsing", { input: parserInput });
    }
    if (cmd === "save-settings" && snapshot) {
      next = await invoke("save_settings_cmd", { settings: collectSettings(snapshot.settings) });
    }
    if (next) {
      snapshot = next;
      render();
    }
  } catch (error) {
    console.error(error);
    alert(String(error));
  }
}

async function refreshModelSetup() {
  modelSetupLoading = true;
  render();
  try {
    modelSetup = await invoke<ModelSetupInfo>("get_model_setup_info");
  } catch (error) {
    console.error(error);
    alert(String(error));
  } finally {
    modelSetupLoading = false;
    render();
  }
}

async function refreshAudioInputDevices() {
  try {
    audioInputDevices = await invoke<AudioInputDevice[]>("list_audio_input_devices");
  } catch (error) {
    console.error(error);
    audioInputDevices = [];
  }
}

async function downloadModel(index: number) {
  const candidate = modelSetup?.candidates[index];
  if (!candidate) return;
  const ok = window.confirm(
    `Download ${candidate.family} ${candidate.quant}?\n\nSize: ${candidate.size_label || formatBytes(candidate.size_bytes)}`
  );
  if (!ok) return;
  const hfToken = Array.from(document.querySelectorAll<HTMLInputElement>("[data-hf-token]"))
    .map((input) => input.value.trim())
    .find((value) => value.length > 0) || null;
  modelDownloadKey = modelKey(candidate);
  modelDownloadBusy = true;
  downloadProgress[modelDownloadKey] = {
    repo: candidate.repo,
    file: candidate.file,
    downloaded_bytes: 0,
    total_bytes: candidate.size_bytes ?? null,
    phase: "starting",
    done: false,
  };
  render();
  try {
    snapshot = await invoke<Snapshot>("download_model_candidate", {
      repo: candidate.repo,
      file: candidate.file,
      hfToken,
    });
    await refreshModelSetup();
  } catch (error) {
    console.error(error);
    alert(String(error));
  } finally {
    modelDownloadKey = null;
    modelDownloadBusy = false;
    render();
  }
}

async function selectLocalModel(index: number) {
  const model = modelSetup?.local_models[index];
  if (!model) return;
  await selectLocalModelPath(model.path);
}

async function selectLocalModelPath(path: string) {
  try {
    snapshot = await invoke<Snapshot>("select_local_model", { path });
    await refreshModelSetup();
  } catch (error) {
    console.error(error);
    alert(String(error));
  }
}

async function runOverlayCommand(cmd: string) {
  try {
    if (cmd === "deny") {
      snapshot = await invoke("deny_pending");
    }
    if (cmd === "confirm-text") {
      const text = document.querySelector<HTMLTextAreaElement>("#overlay-pending-text")?.value ?? "";
      snapshot = await invoke("confirm_pending_text", { text });
    }
    render();
  } catch (error) {
    console.error(error);
  }
}

function collectSettings(base: Settings): Settings {
  const next: Settings = { ...base };
  document.querySelectorAll("[data-setting]").forEach((el) => {
    const input = el as HTMLInputElement;
    const key = input.dataset.setting as keyof Settings;
    const old = next[key];
    const rawValue =
      typeof old === "string" && String(key).endsWith("_path")
        ? displayPath(input.value)
        : input.value;
    const value =
      input.type === "checkbox"
        ? input.checked
        : typeof old === "number"
          ? Number(rawValue)
          : rawValue;
    (next as Record<string, unknown>)[key] = value;
  });
  return next;
}

async function boot() {
  if (!hasTauriRuntime()) {
    app.innerHTML = `
      <main class="shell">
        <section class="panel">
          <h2>Desktop app required</h2>
          <p class="muted">This UI needs the Tauri desktop runtime. Start it with <code>npm run tauri:dev</code>.</p>
        </section>
      </main>
    `;
    return;
  }
  if (isOverlay) {
    snapshot = await invoke("get_status");
    renderOverlay();
    await listen<OverlayState>("overlay-state", (event) => {
      overlayState = event.payload;
      renderOverlay();
    });
    await listen<Snapshot>("status", (event) => {
      snapshot = event.payload;
    });
    return;
  }
  snapshot = await invoke("get_status");
  await refreshAudioInputDevices();
  await refreshModelSetup();
  if (modelSetup && !modelSetup.model_present) {
    activeTab = "settings";
  }
  render();
  await listen<Snapshot>("status", (event) => {
    snapshot = event.payload;
    // Don't rebuild while user is editing — destroys focus mid-edit
    if (activeTab === "settings") return;
    if (pendingNegativeId !== null) return;
    if (isEditingModelSetupField()) return;
    render();
  });
  await listen<boolean>("recording-overlay", (event) => {
    overlay = event.payload;
    render();
  });
  await listen<string>("transcript-popup", (event) => {
    transcriptToast = event.payload;
    if (transcriptTimer) window.clearTimeout(transcriptTimer);
    transcriptTimer = window.setTimeout(() => {
      transcriptToast = "";
      render();
    }, 5200);
    render();
  });
  await listen<ModelDownloadProgress>("model-download-progress", (event) => {
    const progress = event.payload;
    const key = progressKey(progress);
    downloadProgress[key] = progress;
    if (!progress.done) modelDownloadKey = key;
    if (!isEditingModelSetupField()) render();
  });
}

boot().catch((error) => {
  app.innerHTML = `<main class="shell"><section class="panel">Failed to start UI: ${esc(String(error))}</section></main>`;
});
