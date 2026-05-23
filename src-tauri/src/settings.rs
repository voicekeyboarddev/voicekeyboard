use anyhow::Context;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub audio_chunk_ms: u64,
    pub rolling_history_seconds: u64,
    pub pre_roll_ms: u64,
    pub trigger_hold_ms: u64,
    pub right_click_trigger_enabled: bool,
    pub movement_tolerance_px: f64,
    pub vad_rms_threshold: f32,
    pub vad_calibrated: bool,
    pub microphone_device: String,
    pub calibration_prompt_enabled: bool,
    pub shortcuts_enabled: bool,
    pub context_enabled: bool,
    pub dry_run: bool,
    pub confirm_large_text_chars: usize,
    pub confirm_close_shortcuts: bool,
    pub kill_switch_enabled: bool,
    pub injection_delay_ms: u64,
    pub managed_server: bool,
    pub server_url: String,
    pub llama_server_path: String,
    pub llama_device: String,
    pub model_path: String,
    pub mmproj_path: String,
    pub model_download_dir: String,
    pub always_send_low_res_image: bool,
    pub image_width: u32,
    pub image_height: u32,
    pub image_tokens: u32,
    pub context_length_tokens: u32,
    pub recent_model_paths: Vec<String>,
    pub log_retention_bytes: u64,
    pub common_terms: String,
    #[serde(alias = "languages")]
    pub spoken_languages: String,
    pub recent_context_enabled: bool,
    pub recent_context_max_requests: usize,
    pub recent_context_window_seconds: u64,
    pub recent_context_max_items: usize,
    pub thinking_handoff_enabled: bool,
    pub thinking_handoff_min_chars: usize,
    pub thinking_handoff_reasoning_budget: i32,
    pub thinking_handoff_context_items: usize,
    pub prompt_provider: String,
    pub prompt_endpoint_url: String,
    pub prompt_api_key: String,
    pub prompt_model: String,
    pub prompt_auto_inject_keyboard: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            audio_chunk_ms: 500,
            rolling_history_seconds: 30,
            pre_roll_ms: 2000,
            trigger_hold_ms: 450,
            right_click_trigger_enabled: false,
            movement_tolerance_px: 12.0,
            vad_rms_threshold: 0.008,
            vad_calibrated: false,
            microphone_device: String::new(),
            calibration_prompt_enabled: true,
            shortcuts_enabled: true,
            context_enabled: true,
            dry_run: false,
            confirm_large_text_chars: 800,
            confirm_close_shortcuts: true,
            kill_switch_enabled: true,
            injection_delay_ms: 20,
            managed_server: true,
            server_url: "http://127.0.0.1:8099".to_string(),
            llama_server_path: r"runtime\llama-server.exe".to_string(),
            llama_device: "Vulkan0".to_string(),
            model_path: r"models\gemma-4-E4B-it-Q4_K_M\gemma-4-E4B-it.Q4_K_M.gguf".to_string(),
            mmproj_path: r"models\gemma-4-E4B-it-Q2\gemma-4-E4B-it.mmproj-Q8_0.gguf".to_string(),
            model_download_dir: String::new(),
            always_send_low_res_image: false,
            image_width: 160,
            image_height: 100,
            image_tokens: 140,
            context_length_tokens: 4096,
            recent_model_paths: Vec::new(),
            log_retention_bytes: 5 * 1024 * 1024,
            common_terms: String::new(),
            spoken_languages: "English".to_string(),
            recent_context_enabled: true,
            recent_context_max_requests: 5,
            recent_context_window_seconds: 60,
            recent_context_max_items: 5,
            thinking_handoff_enabled: true,
            thinking_handoff_min_chars: 250,
            thinking_handoff_reasoning_budget: 64,
            thinking_handoff_context_items: 3,
            prompt_provider: "local".to_string(),
            prompt_endpoint_url: String::new(),
            prompt_api_key: String::new(),
            prompt_model: "gpt-4.1".to_string(),
            prompt_auto_inject_keyboard: true,
        }
    }
}

pub fn config_dir() -> PathBuf {
    ProjectDirs::from("local", "VoiceKeyboard", "VoiceKeyboard")
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

fn executable_dir() -> Option<PathBuf> {
    env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
}

fn bundled_resource_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(exe_dir) = executable_dir() {
        dirs.push(exe_dir.clone());
        dirs.push(exe_dir.join("resources"));
        if let Some(parent) = exe_dir.parent() {
            dirs.push(parent.join("resources"));
        }
    }
    if let Ok(current_dir) = env::current_dir() {
        dirs.push(current_dir.join("src-tauri").join("resources"));
        dirs.push(current_dir.join("resources"));
    }
    dirs.push(config_dir());
    dirs
}

pub fn default_models_dir() -> PathBuf {
    config_dir().join("models")
}

pub fn models_dir(settings: &Settings) -> PathBuf {
    let trimmed = settings.model_download_dir.trim();
    if trimmed.is_empty() {
        default_models_dir()
    } else {
        PathBuf::from(trimmed)
    }
}

pub const GEMMA4_IMAGE_TOKEN_BUDGETS: &[u32] = &[70, 140, 280, 560, 1120];

pub fn valid_image_tokens(tokens: u32) -> u32 {
    GEMMA4_IMAGE_TOKEN_BUDGETS
        .iter()
        .copied()
        .min_by_key(|valid| valid.abs_diff(tokens))
        .unwrap_or(280)
}

pub fn runtime_dir() -> PathBuf {
    config_dir().join("runtime")
}

pub fn installed_llama_server_path() -> PathBuf {
    runtime_dir().join("llama-server.exe")
}

pub fn ensure_bundled_llama_runtime() -> anyhow::Result<PathBuf> {
    let source = bundled_runtime_dir()
        .ok_or_else(|| anyhow::anyhow!("bundled llama.cpp runtime folder was not found"))?;
    let destination = runtime_dir();
    fs::create_dir_all(&destination)?;
    copy_runtime_tree(&source, &destination)?;
    let server = installed_llama_server_path();
    if !server.exists() {
        anyhow::bail!("bundled llama.cpp runtime did not install llama-server.exe");
    }
    Ok(server)
}

fn bundled_runtime_dir() -> Option<PathBuf> {
    let destination = runtime_dir();
    bundled_resource_dirs()
        .into_iter()
        .map(|base| base.join("runtime"))
        .find(|path| path.exists() && !same_path(path, &destination))
}

fn same_path(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn copy_runtime_tree(source: &Path, destination: &Path) -> anyhow::Result<()> {
    for entry in
        fs::read_dir(source).with_context(|| format!("failed to read {}", source.display()))?
    {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            fs::create_dir_all(&destination_path)?;
            copy_runtime_tree(&source_path, &destination_path)?;
        } else if should_copy_file(&source_path, &destination_path)? {
            fs::copy(&source_path, &destination_path).with_context(|| {
                format!(
                    "failed to install llama.cpp runtime file {}",
                    source_path.display()
                )
            })?;
        }
    }
    Ok(())
}

fn should_copy_file(source: &Path, destination: &Path) -> anyhow::Result<bool> {
    let Ok(destination_metadata) = fs::metadata(destination) else {
        return Ok(true);
    };
    let source_metadata = fs::metadata(source)?;
    if source_metadata.len() != destination_metadata.len() {
        return Ok(true);
    }
    let source_modified = source_metadata.modified().unwrap_or(UNIX_EPOCH);
    let destination_modified = destination_metadata.modified().unwrap_or(UNIX_EPOCH);
    Ok(source_modified > destination_modified)
}

fn portable_config_dir() -> Option<PathBuf> {
    executable_dir().map(|dir| dir.join("portable-config"))
}

fn preferred_settings_path() -> PathBuf {
    if let Some(portable_dir) = portable_config_dir() {
        if portable_dir.exists() {
            return portable_dir.join("settings.json");
        }
    }
    config_dir().join("settings.json")
}

fn readable_settings_paths() -> Vec<PathBuf> {
    let mut paths = vec![preferred_settings_path()];
    for base in bundled_resource_dirs() {
        let resource_settings = base.join("portable-config").join("settings.json");
        paths.push(resource_settings);
    }
    paths
}

fn resolve_runtime_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let raw = PathBuf::from(trimmed);
    if raw.is_absolute() {
        trimmed.to_string()
    } else {
        for base in bundled_resource_dirs() {
            let candidate = base.join(&raw);
            if candidate.exists() {
                return candidate.to_string_lossy().to_string();
            }
        }
        if let Some(base) = executable_dir() {
            base.join(raw).to_string_lossy().to_string()
        } else {
            trimmed.to_string()
        }
    }
}

fn resolve_settings_paths(mut settings: Settings) -> Settings {
    settings.llama_server_path = resolve_llama_server_path(&settings.llama_server_path);
    settings.model_path =
        normalize_windows_extended_path(&resolve_runtime_path(&settings.model_path));
    settings.mmproj_path =
        normalize_windows_extended_path(&resolve_runtime_path(&settings.mmproj_path));
    settings.model_download_dir =
        normalize_windows_extended_path(settings.model_download_dir.trim());
    settings.image_tokens = valid_image_tokens(settings.image_tokens);
    restore_lightweight_image_defaults(&mut settings);
    settings.recent_model_paths = settings
        .recent_model_paths
        .into_iter()
        .map(|path| normalize_windows_extended_path(&path))
        .collect();
    if settings.model_path.trim().is_empty() || !PathBuf::from(&settings.model_path).exists() {
        if let Some(model) = find_model_file(&settings, false) {
            settings.model_path = model.to_string_lossy().to_string();
        }
    }
    if settings.mmproj_path.trim().is_empty() || !PathBuf::from(&settings.mmproj_path).exists() {
        if let Some(mmproj) = find_model_file(&settings, true) {
            settings.mmproj_path = mmproj.to_string_lossy().to_string();
        }
    }
    settings
}

fn restore_lightweight_image_defaults(settings: &mut Settings) {
    if settings.always_send_low_res_image {
        return;
    }
    let auto_tuned_values = [
        (512, 384, 70),
        (768, 512, 140),
        (960, 720, 280),
        (1280, 1024, 560),
        (1600, 1600, 1120),
    ];
    if auto_tuned_values.iter().any(|&(width, height, tokens)| {
        settings.image_width == width
            && settings.image_height == height
            && settings.image_tokens == tokens
    }) {
        settings.image_width = 160;
        settings.image_height = 100;
        settings.image_tokens = 140;
    }
}

pub fn normalize_windows_extended_path(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        path.to_string()
    }
}

fn resolve_llama_server_path(path: &str) -> String {
    match ensure_bundled_llama_runtime() {
        Ok(installed) => {
            let trimmed = path.trim();
            if trimmed.is_empty()
                || trimmed.eq_ignore_ascii_case(r"runtime\llama-server.exe")
                || trimmed.eq_ignore_ascii_case("runtime/llama-server.exe")
                || !PathBuf::from(trimmed).exists()
            {
                return installed.to_string_lossy().to_string();
            }
            resolve_runtime_path(trimmed)
        }
        Err(_) => resolve_runtime_path(path),
    }
}

fn find_model_file(settings: &Settings, mmproj: bool) -> Option<PathBuf> {
    let mut roots = vec![models_dir(settings), default_models_dir()];
    for base in bundled_resource_dirs() {
        roots.push(base.join("models"));
    }
    for root in roots {
        if let Some(path) = find_gguf_in_dir(&root, mmproj) {
            return Some(path);
        }
    }
    None
}

fn find_gguf_in_dir(root: &Path, mmproj: bool) -> Option<PathBuf> {
    let entries = fs::read_dir(root).ok()?;
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_gguf_in_dir(&path, mmproj) {
                return Some(found);
            }
        } else if path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("gguf"))
            .unwrap_or(false)
        {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if name.contains("mmproj") == mmproj {
                files.push(path);
            }
        }
    }
    files.sort();
    files.into_iter().next()
}

pub fn load_settings() -> Settings {
    for path in readable_settings_paths() {
        if let Ok(text) = fs::read_to_string(path) {
            return resolve_settings_paths(serde_json::from_str(&text).unwrap_or_default());
        }
    }
    resolve_settings_paths(Settings::default())
}

pub fn save_settings(settings: &Settings) -> anyhow::Result<()> {
    let path = preferred_settings_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut settings = settings.clone();
    settings.model_path = normalize_windows_extended_path(&settings.model_path);
    settings.mmproj_path = normalize_windows_extended_path(&settings.mmproj_path);
    settings.model_download_dir =
        normalize_windows_extended_path(settings.model_download_dir.trim());
    settings.image_tokens = valid_image_tokens(settings.image_tokens);
    settings.recent_model_paths = settings
        .recent_model_paths
        .into_iter()
        .map(|path| normalize_windows_extended_path(&path))
        .collect();
    let text = serde_json::to_string_pretty(&settings)?;
    fs::write(&path, text).with_context(|| format!("failed to write {}", path.display()))
}
