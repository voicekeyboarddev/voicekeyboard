use crate::settings::{self, Settings};
use anyhow::{anyhow, Context};
use futures_util::StreamExt;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuDevice {
    pub id: String,
    pub name: String,
    pub backend: String,
    pub memory_total_mb: Option<u64>,
    pub memory_free_mb: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCandidate {
    pub repo: String,
    pub file: String,
    pub size_bytes: Option<u64>,
    pub size_label: String,
    pub family: String,
    pub quant: String,
    pub min_vram_mb: u64,
    pub recommended: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalModelFile {
    pub path: String,
    pub name: String,
    pub size_bytes: Option<u64>,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDownloadProgress {
    pub repo: String,
    pub file: String,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub phase: String,
    pub done: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSetupInfo {
    pub model_present: bool,
    pub model_path: String,
    pub mmproj_path: String,
    pub models_dir: String,
    pub gpu_devices: Vec<GpuDevice>,
    pub cpu_only_warning: Option<String>,
    pub candidates: Vec<ModelCandidate>,
    pub local_models: Vec<LocalModelFile>,
}

#[derive(Debug, Clone, Copy)]
struct KnownGemmaGguf {
    repo: &'static str,
    file: &'static str,
    mmproj: &'static str,
    family: &'static str,
    quant: &'static str,
    min_vram_mb: u64,
    size_gb: f64,
}

const KNOWN_GEMMA_GGUFS: &[KnownGemmaGguf] = &[
    KnownGemmaGguf {
        repo: "unsloth/gemma-4-E2B-it-GGUF",
        file: "gemma-4-E2B-it-Q4_K_M.gguf",
        mmproj: "mmproj-BF16.gguf",
        family: "Gemma 4 E2B",
        quant: "Q4_K_M",
        min_vram_mb: 4 * 1024,
        size_gb: 3.11,
    },
    KnownGemmaGguf {
        repo: "unsloth/gemma-4-E2B-it-GGUF",
        file: "gemma-4-E2B-it-Q5_K_M.gguf",
        mmproj: "mmproj-BF16.gguf",
        family: "Gemma 4 E2B",
        quant: "Q5_K_M",
        min_vram_mb: 5 * 1024,
        size_gb: 3.36,
    },
    KnownGemmaGguf {
        repo: "unsloth/gemma-4-E2B-it-GGUF",
        file: "gemma-4-E2B-it-Q6_K.gguf",
        mmproj: "mmproj-BF16.gguf",
        family: "Gemma 4 E2B",
        quant: "Q6_K",
        min_vram_mb: 6 * 1024,
        size_gb: 4.50,
    },
    KnownGemmaGguf {
        repo: "unsloth/gemma-4-E2B-it-GGUF",
        file: "gemma-4-E2B-it-Q8_0.gguf",
        mmproj: "mmproj-BF16.gguf",
        family: "Gemma 4 E2B",
        quant: "Q8_0",
        min_vram_mb: 8 * 1024,
        size_gb: 5.05,
    },
    KnownGemmaGguf {
        repo: "unsloth/gemma-4-E2B-it-GGUF",
        file: "gemma-4-E2B-it-BF16.gguf",
        mmproj: "mmproj-BF16.gguf",
        family: "Gemma 4 E2B",
        quant: "BF16",
        min_vram_mb: 12 * 1024,
        size_gb: 9.31,
    },
    KnownGemmaGguf {
        repo: "unsloth/gemma-4-E4B-it-GGUF",
        file: "gemma-4-E4B-it-Q4_K_M.gguf",
        mmproj: "mmproj-BF16.gguf",
        family: "Gemma 4 E4B",
        quant: "Q4_K_M",
        min_vram_mb: 6 * 1024,
        size_gb: 4.98,
    },
    KnownGemmaGguf {
        repo: "unsloth/gemma-4-E4B-it-GGUF",
        file: "gemma-4-E4B-it-Q5_K_M.gguf",
        mmproj: "mmproj-BF16.gguf",
        family: "Gemma 4 E4B",
        quant: "Q5_K_M",
        min_vram_mb: 8 * 1024,
        size_gb: 5.48,
    },
    KnownGemmaGguf {
        repo: "unsloth/gemma-4-E4B-it-GGUF",
        file: "gemma-4-E4B-it-Q6_K.gguf",
        mmproj: "mmproj-BF16.gguf",
        family: "Gemma 4 E4B",
        quant: "Q6_K",
        min_vram_mb: 10 * 1024,
        size_gb: 7.07,
    },
    KnownGemmaGguf {
        repo: "unsloth/gemma-4-E4B-it-GGUF",
        file: "gemma-4-E4B-it-Q8_0.gguf",
        mmproj: "mmproj-BF16.gguf",
        family: "Gemma 4 E4B",
        quant: "Q8_0",
        min_vram_mb: 14 * 1024,
        size_gb: 8.19,
    },
    KnownGemmaGguf {
        repo: "unsloth/gemma-4-E4B-it-GGUF",
        file: "gemma-4-E4B-it-BF16.gguf",
        mmproj: "mmproj-BF16.gguf",
        family: "Gemma 4 E4B",
        quant: "BF16",
        min_vram_mb: 20 * 1024,
        size_gb: 15.10,
    },
];

pub async fn setup_info(settings: &Settings) -> anyhow::Result<ModelSetupInfo> {
    let (llama_server_path, runtime_warning) = match settings::ensure_bundled_llama_runtime() {
        Ok(path) => (path.to_string_lossy().to_string(), None),
        Err(err) => (
            settings.llama_server_path.clone(),
            Some(format!(
                "Could not install bundled llama.cpp runtime: {err}"
            )),
        ),
    };
    let (gpu_devices, gpu_warning) = match detect_llama_devices(&llama_server_path) {
        Ok(devices) => (devices, None),
        Err(err) => (
            Vec::new(),
            Some(format!("Could not check GPU support with llama.cpp: {err}")),
        ),
    };
    let candidates = discover_gemma_ggufs(&gpu_devices).await.unwrap_or_default();
    let local_models = discover_local_models(settings);
    let model_present =
        !settings.model_path.trim().is_empty() && Path::new(&settings.model_path).exists();
    let cpu_only_warning = runtime_warning
        .or(gpu_warning)
        .or_else(|| {
            if gpu_devices.is_empty() {
                Some(
                    "No llama.cpp GPU device was detected. The app can still run on CPU, but model loading and responses will be slow."
                        .to_string(),
                )
            } else {
                None
            }
        });

    Ok(ModelSetupInfo {
        model_present,
        model_path: settings.model_path.clone(),
        mmproj_path: settings.mmproj_path.clone(),
        models_dir: settings::models_dir(settings).to_string_lossy().to_string(),
        gpu_devices,
        cpu_only_warning,
        candidates,
        local_models,
    })
}

pub async fn download_model<F>(
    settings: &Settings,
    repo: &str,
    file: &str,
    hf_token: Option<String>,
    mut progress: F,
) -> anyhow::Result<Settings>
where
    F: FnMut(ModelDownloadProgress) + Send,
{
    let known = known_file(repo, file)
        .ok_or_else(|| anyhow!("unsupported model selection: {repo}/{file}"))?;
    let models_dir = settings::models_dir(settings);
    let repo_dir = models_dir.join(repo.replace('/', "__"));
    fs::create_dir_all(&repo_dir)?;
    let destination = repo_dir.join(
        Path::new(file)
            .file_name()
            .ok_or_else(|| anyhow!("invalid model file name"))?,
    );

    download_hf_file(repo, file, &destination, hf_token.as_deref(), &mut progress).await?;

    let mut next = settings.clone();
    next.model_path = settings::normalize_windows_extended_path(&destination.to_string_lossy());
    if known.mmproj.trim().is_empty() {
        next.mmproj_path = String::new();
        settings::save_settings(&next)?;
        return Ok(next);
    }
    let mmproj_dest = repo_dir.join(
        Path::new(known.mmproj)
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("mmproj.gguf")),
    );
    if !mmproj_dest.exists() {
        download_hf_file(
            repo,
            known.mmproj,
            &mmproj_dest,
            hf_token.as_deref(),
            &mut progress,
        )
        .await
        .with_context(|| {
            format!(
                "failed to download required projector/adaptor {}",
                known.mmproj
            )
        })?;
    }
    if mmproj_dest.exists() {
        next.mmproj_path =
            settings::normalize_windows_extended_path(&mmproj_dest.to_string_lossy());
    } else {
        anyhow::bail!(
            "required projector/adaptor was not downloaded: {}",
            known.mmproj
        );
    }
    settings::save_settings(&next)?;
    Ok(next)
}

pub fn select_local_model(settings: &Settings, path: &str) -> anyhow::Result<Settings> {
    let model_path = Path::new(path);
    if !model_path.exists() {
        return Err(anyhow!("model file not found: {path}"));
    }
    if !model_path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("gguf"))
        .unwrap_or(false)
    {
        return Err(anyhow!("selected file is not a GGUF model: {path}"));
    }
    let name = model_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if name.contains("mmproj") {
        return Err(anyhow!("select the main model GGUF, not an mmproj file"));
    }

    let mut next = settings.clone();
    let canonical_model_path = model_path
        .canonicalize()
        .unwrap_or_else(|_| model_path.to_path_buf());
    next.model_path =
        settings::normalize_windows_extended_path(&canonical_model_path.to_string_lossy());
    next.mmproj_path = find_sibling_mmproj(&canonical_model_path)
        .map(|path| settings::normalize_windows_extended_path(&path.to_string_lossy()))
        .unwrap_or_default();
    remember_recent_model_path(&mut next, &canonical_model_path);
    settings::save_settings(&next)?;
    Ok(next)
}

async fn discover_gemma_ggufs(gpus: &[GpuDevice]) -> anyhow::Result<Vec<ModelCandidate>> {
    let mut candidates = KNOWN_GEMMA_GGUFS
        .iter()
        .map(|known| {
            let (recommended, reason) = recommendation(known, gpus);
            ModelCandidate {
                repo: known.repo.to_string(),
                file: known.file.to_string(),
                size_bytes: Some((known.size_gb * 1024.0 * 1024.0 * 1024.0) as u64),
                size_label: format!("{:.2} GB", known.size_gb),
                family: known.family.to_string(),
                quant: known.quant.to_string(),
                min_vram_mb: known.min_vram_mb,
                recommended,
                reason,
            }
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| {
        family_rank(&b.family)
            .cmp(&family_rank(&a.family))
            .then(quant_rank(&b.quant).cmp(&quant_rank(&a.quant)))
    });
    Ok(candidates)
}

fn discover_local_models(settings: &Settings) -> Vec<LocalModelFile> {
    let mut models = Vec::new();
    collect_local_models(&settings::models_dir(settings), settings, &mut models);
    let current = Path::new(&settings.model_path);
    if current.exists() {
        push_local_model(current, settings, &mut models);
    }
    for recent in &settings.recent_model_paths {
        let path = Path::new(recent);
        if path.exists() {
            push_local_model(path, settings, &mut models);
        }
    }
    models.sort_by(|a, b| b.active.cmp(&a.active).then(a.name.cmp(&b.name)));
    models
}

fn collect_local_models(root: &Path, settings: &Settings, models: &mut Vec<LocalModelFile>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_local_models(&path, settings, models);
        } else {
            push_local_model(&path, settings, models);
        }
    }
}

fn push_local_model(path: &Path, settings: &Settings, models: &mut Vec<LocalModelFile>) {
    let is_gguf = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("gguf"))
        .unwrap_or(false);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if !is_gguf || name.to_ascii_lowercase().contains("mmproj") {
        return;
    }
    let path_key = comparable_path(path);
    if models
        .iter()
        .any(|model| comparable_path(Path::new(&model.path)) == path_key)
    {
        return;
    }
    let active = comparable_path(Path::new(&settings.model_path)) == path_key;
    models.push(LocalModelFile {
        path: path.to_string_lossy().to_string(),
        name: name.to_string(),
        size_bytes: path.metadata().ok().map(|metadata| metadata.len()),
        active,
    });
}

fn remember_recent_model_path(settings: &mut Settings, path: &Path) {
    if is_in_app_models_dir(settings, path) {
        return;
    }
    let path_text = settings::normalize_windows_extended_path(&path.to_string_lossy());
    let path_key = comparable_path(path);
    settings
        .recent_model_paths
        .retain(|existing| comparable_path(Path::new(existing)) != path_key);
    settings.recent_model_paths.insert(0, path_text);
    settings.recent_model_paths.truncate(12);
}

fn is_in_app_models_dir(settings: &Settings, path: &Path) -> bool {
    let Ok(model_path) = path.canonicalize() else {
        return false;
    };
    let Ok(models_dir) = settings::models_dir(settings).canonicalize() else {
        return false;
    };
    model_path.starts_with(models_dir)
}

fn comparable_path(path: &Path) -> String {
    let normalized = path.canonicalize().unwrap_or_else(|_| PathBuf::from(path));
    let text = normalized.to_string_lossy().to_string();
    if cfg!(windows) {
        text.to_ascii_lowercase()
    } else {
        text
    }
}

fn find_sibling_mmproj(model_path: &Path) -> Option<std::path::PathBuf> {
    let dir = model_path.parent()?;
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("gguf"))
            .unwrap_or(false)
            && name.contains("mmproj")
        {
            return Some(path);
        }
    }
    None
}

fn known_file(repo: &str, file: &str) -> Option<&'static KnownGemmaGguf> {
    KNOWN_GEMMA_GGUFS
        .iter()
        .find(|known| known.repo == repo && known.file == file)
}

fn recommendation(known: &KnownGemmaGguf, gpus: &[GpuDevice]) -> (bool, String) {
    let preferred = preferred_gpus(gpus);
    let best_free = preferred.iter().filter_map(|g| g.memory_free_mb).max();
    let best_total = preferred.iter().filter_map(|g| g.memory_total_mb).max();
    let available = best_free.or(best_total);
    match available {
        Some(vram) if vram >= known.min_vram_mb => {
            let recommended = recommended_for_vram(vram, known);
            let reason = format!("Supported with detected GPU memory ({vram} MiB).");
            (recommended, reason)
        }
        Some(vram) => (
            false,
            format!(
                "Needs at least {} MiB GPU memory; detected {vram} MiB.",
                known.min_vram_mb
            ),
        ),
        None if gpus.is_empty() => (
            false,
            "GPU memory was not detected; choose manually or use an existing GGUF.".to_string(),
        ),
        None => (
            false,
            format!(
                "GPU was detected, but memory was not reported. Requires {} MiB.",
                known.min_vram_mb
            ),
        ),
    }
}

fn preferred_gpus(gpus: &[GpuDevice]) -> Vec<&GpuDevice> {
    let discrete = gpus
        .iter()
        .filter(|gpu| !is_integrated_gpu_name(&gpu.name))
        .collect::<Vec<_>>();
    if discrete.is_empty() {
        gpus.iter().collect()
    } else {
        discrete
    }
}

fn is_integrated_gpu_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("intel") || lower.contains("uhd graphics") || lower.contains("iris")
}

fn recommended_for_vram(vram_mb: u64, known: &KnownGemmaGguf) -> bool {
    match vram_mb {
        0..=4095 => known.family.contains("E2B") && known.quant == "Q4_K_M",
        4096..=5119 => known.family.contains("E2B") && known.quant == "Q4_K_M",
        5120..=6143 => known.family.contains("E2B") && known.quant == "Q5_K_M",
        6144..=8191 => known.family.contains("E4B") && known.quant == "Q4_K_M",
        8192..=10239 => known.family.contains("E4B") && known.quant == "Q5_K_M",
        10240..=14335 => known.family.contains("E4B") && known.quant == "Q6_K",
        14336..=20479 => known.family.contains("E4B") && known.quant == "Q8_0",
        _ => known.family.contains("E4B") && known.quant == "BF16",
    }
}

fn family_rank(family: &str) -> u8 {
    if family.contains("E4B") {
        2
    } else if family.contains("E2B") {
        1
    } else {
        0
    }
}

fn quant_rank(quant: &str) -> u8 {
    match quant {
        "BF16" => 5,
        "Q8_0" => 4,
        "Q6_K" => 3,
        "Q5_K_M" => 2,
        "Q4_K_M" => 1,
        _ => 0,
    }
}

async fn download_hf_file(
    repo: &str,
    file: &str,
    destination: &Path,
    hf_token: Option<&str>,
    progress: &mut (impl FnMut(ModelDownloadProgress) + Send),
) -> anyhow::Result<()> {
    if destination.exists() {
        let downloaded_bytes = destination
            .metadata()
            .ok()
            .map(|meta| meta.len())
            .unwrap_or(0);
        progress(ModelDownloadProgress {
            repo: repo.to_string(),
            file: file.to_string(),
            downloaded_bytes,
            total_bytes: Some(downloaded_bytes),
            phase: "already-present".to_string(),
            done: true,
        });
        return Ok(());
    }

    let client = Client::builder()
        .timeout(Duration::from_secs(60 * 60))
        .build()?;
    let mut url = Url::parse(&format!("https://huggingface.co/{repo}/resolve/main"))?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| anyhow!("failed to build Hugging Face download URL"))?;
        for part in file.split('/') {
            segments.push(part);
        }
    }
    url.query_pairs_mut().append_pair("download", "true");
    let mut request = client.get(url);
    if let Some(token) = hf_token.filter(|t| !t.trim().is_empty()) {
        request = request.bearer_auth(token.trim());
    }
    progress(ModelDownloadProgress {
        repo: repo.to_string(),
        file: file.to_string(),
        downloaded_bytes: 0,
        total_bytes: None,
        phase: "starting".to_string(),
        done: false,
    });
    let response = request.send().await?.error_for_status()?;
    let total_bytes = response.content_length();
    let tmp = destination.with_extension("gguf.part");
    let mut file_out = tokio::fs::File::create(&tmp).await?;
    let mut stream = response.bytes_stream();
    let mut downloaded_bytes = 0u64;
    let mut last_progress = Instant::now();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        downloaded_bytes += bytes.len() as u64;
        tokio::io::AsyncWriteExt::write_all(&mut file_out, &bytes).await?;
        if last_progress.elapsed() >= Duration::from_millis(250) {
            progress(ModelDownloadProgress {
                repo: repo.to_string(),
                file: file.to_string(),
                downloaded_bytes,
                total_bytes,
                phase: "downloading".to_string(),
                done: false,
            });
            last_progress = Instant::now();
        }
    }
    tokio::io::AsyncWriteExt::flush(&mut file_out).await?;
    drop(file_out);
    tokio::fs::rename(&tmp, destination)
        .await
        .with_context(|| format!("failed to save {}", destination.display()))?;
    progress(ModelDownloadProgress {
        repo: repo.to_string(),
        file: file.to_string(),
        downloaded_bytes,
        total_bytes,
        phase: "finished".to_string(),
        done: true,
    });
    Ok(())
}

pub fn detect_llama_devices(llama_server_path: &str) -> anyhow::Result<Vec<GpuDevice>> {
    let output = hidden_command(llama_server_path, &["--list-devices"])?;
    let mut devices = parse_llama_devices(&output);
    merge_windows_gpu_memory(&mut devices);
    Ok(devices)
}

fn parse_llama_devices(output: &str) -> Vec<GpuDevice> {
    output
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            let (id, rest) = trimmed.split_once(':')?;
            if id.is_empty() || rest.is_empty() || id.eq_ignore_ascii_case("available devices") {
                return None;
            }
            let backend = id
                .chars()
                .take_while(|c| c.is_alphabetic())
                .collect::<String>();
            let (name, memory_total_mb, memory_free_mb) = parse_device_detail(rest.trim());
            Some(GpuDevice {
                id: id.to_string(),
                name,
                backend,
                memory_total_mb,
                memory_free_mb,
            })
        })
        .collect()
}

fn parse_device_detail(detail: &str) -> (String, Option<u64>, Option<u64>) {
    let Some((name, memory)) = detail.rsplit_once('(') else {
        return (detail.to_string(), None, None);
    };
    let mut nums = memory
        .trim_end_matches(')')
        .split(',')
        .filter_map(|part| part.split_whitespace().next()?.parse::<u64>().ok());
    let total = nums.next();
    let free = nums.next();
    (name.trim().to_string(), total, free)
}

#[cfg(windows)]
fn merge_windows_gpu_memory(devices: &mut Vec<GpuDevice>) {
    let mut adapters = Vec::new();
    if let Ok(output) = hidden_command(
        "nvidia-smi.exe",
        &[
            "--query-gpu=name,memory.total",
            "--format=csv,noheader,nounits",
        ],
    ) {
        adapters.extend(output.lines().filter_map(|line| {
            let (name, mb) = line.split_once(',')?;
            let mb = mb.trim().parse::<u64>().ok()?;
            Some((name.trim().to_ascii_lowercase(), mb))
        }));
    }
    if let Ok(output) = hidden_command(
        "powershell.exe",
        &[
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_VideoController | Where-Object {$_.AdapterRAM -gt 0} | ForEach-Object { \"$($_.Name)|$($_.AdapterRAM)\" }",
        ],
    ) {
        adapters.extend(output.lines().filter_map(|line| {
            let (name, bytes) = line.split_once('|')?;
            let mb = bytes.trim().parse::<u64>().ok()? / 1024 / 1024;
            if mb == 0 {
                None
            } else {
                Some((name.trim().to_ascii_lowercase(), mb))
            }
        }));
    }
    if devices.is_empty() {
        for (index, (name, mb)) in adapters.into_iter().enumerate() {
            devices.push(GpuDevice {
                id: format!("GPU{index}"),
                name,
                backend: "Windows".to_string(),
                memory_total_mb: Some(mb),
                memory_free_mb: Some(mb),
            });
        }
        return;
    }
    for device in devices {
        if device.memory_total_mb.is_some() {
            continue;
        }
        let lower = device.name.to_ascii_lowercase();
        if let Some((_, mb)) = adapters
            .iter()
            .find(|(name, _)| lower.contains(name.as_str()) || name.contains(lower.as_str()))
        {
            device.memory_total_mb = Some(*mb);
            device.memory_free_mb = Some(*mb);
        }
    }
}

#[cfg(not(windows))]
fn merge_windows_gpu_memory(_devices: &mut Vec<GpuDevice>) {}

fn hidden_command(program: &str, args: &[&str]) -> anyhow::Result<String> {
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_window(&mut command);
    let output = command
        .output()
        .with_context(|| format!("failed to run {program}"))?;
    Ok(format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

#[cfg(windows)]
fn hide_window(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_window(_command: &mut std::process::Command) {}
