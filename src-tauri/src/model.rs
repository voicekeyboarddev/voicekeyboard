use crate::{
    model_setup,
    settings::{self, Settings},
    types::WindowContext,
};
use anyhow::{anyhow, Context};
use base64::{engine::general_purpose, Engine};
use futures_util::StreamExt;
use parking_lot::Mutex;
use reqwest::Client;
use serde_json::json;
use std::{
    fs::OpenOptions,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::AsyncReadExt,
    process::Child,
    time::{sleep, timeout},
};

#[derive(Debug, Clone)]
pub struct ModelResponse {
    pub content: String,
    pub ttft_ms: Option<f64>,
    pub tokens_per_second: Option<f64>,
    pub total_ms: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct StreamingInterpretation {
    pub response: ModelResponse,
    pub streamed_text: String,
    pub used_fallback_prompt: bool,
    pub image_attached: bool,
    pub prompt: String,
}

#[derive(Debug, Clone)]
pub struct PromptHandoffResponse {
    pub delivery: String,
    pub text: String,
    pub response: ModelResponse,
    pub used_media_fallback: bool,
}

#[derive(Debug, Clone)]
pub struct RecentTextContext {
    pub stage: String,
    pub transcript: String,
    pub output: String,
    pub age_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterpretationMode {
    Fast,
    Thinking,
}

impl InterpretationMode {
    pub fn stage_name(self) -> &'static str {
        match self {
            InterpretationMode::Fast => "interpretation",
            InterpretationMode::Thinking => "interpretation-thinking",
        }
    }
}

#[derive(Clone, Copy)]
enum StreamParseMode {
    JsonTextValue,
    PlainText,
}

#[derive(Clone)]
pub struct ModelClient {
    inner: Arc<Inner>,
}

struct Inner {
    http: Client,
    child: Mutex<Option<Child>>,
    status: Mutex<String>,
    warmed: Mutex<bool>,
    warmup_lock: tokio::sync::Mutex<()>,
}

impl ModelClient {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                http: Client::new(),
                child: Mutex::new(None),
                status: Mutex::new("cold".to_string()),
                warmed: Mutex::new(false),
                warmup_lock: tokio::sync::Mutex::new(()),
            }),
        }
    }

    pub fn status(&self) -> String {
        self.inner.status.lock().clone()
    }

    pub fn set_status(&self, status: impl Into<String>) {
        *self.inner.status.lock() = status.into();
    }

    pub fn server_pid(&self) -> Option<u32> {
        self.inner
            .child
            .lock()
            .as_ref()
            .and_then(|child| child.id())
    }

    pub async fn shutdown(&self, settings: &Settings) {
        *self.inner.status.lock() = "stopped".to_string();
        *self.inner.warmed.lock() = false;
        let child = self.inner.child.lock().take();
        if let Some(mut child) = child {
            let _ = child.kill().await;
        }
        if settings.managed_server {
            kill_server_on_configured_port(&settings.server_url);
        }
    }

    pub async fn ensure_running(&self, settings: &Settings) -> anyhow::Result<()> {
        if self.health(settings).await {
            let has_managed_child = self.inner.child.lock().is_some();
            if !settings.managed_server || has_managed_child {
                self.warm_up(settings).await?;
                *self.inner.status.lock() = "warm".to_string();
                return Ok(());
            }

            *self.inner.status.lock() = "restarting".to_string();
            kill_server_on_configured_port(&settings.server_url);
            sleep(Duration::from_millis(500)).await;
        }

        if !settings.managed_server {
            *self.inner.status.lock() = "external server unavailable".to_string();
            return Err(anyhow!("llama server is not reachable"));
        }

        if !Path::new(&settings.llama_server_path).exists() {
            *self.inner.status.lock() = "server executable missing".to_string();
            return Err(anyhow!(
                "llama server executable not found: {}",
                settings.llama_server_path
            ));
        }
        if !Path::new(&settings.model_path).exists() {
            *self.inner.status.lock() = "model file missing".to_string();
            return Err(anyhow!("model file not found: {}", settings.model_path));
        }
        if !settings.mmproj_path.trim().is_empty() && !Path::new(&settings.mmproj_path).exists() {
            *self.inner.status.lock() = "mmproj file missing".to_string();
            return Err(anyhow!("mmproj file not found: {}", settings.mmproj_path));
        }

        let needs_spawn = self.inner.child.lock().is_none();
        if needs_spawn {
            *self.inner.status.lock() = "starting".to_string();
            *self.inner.warmed.lock() = false;
            let mut command = tokio::process::Command::new(&settings.llama_server_path);
            command.arg("-m").arg(&settings.model_path);
            if !settings.mmproj_path.trim().is_empty() {
                command.arg("--mmproj").arg(&settings.mmproj_path);
            }
            if let Some(device) = select_llama_device(settings) {
                command.arg("--device").arg(device);
            }
            let port = server_port(&settings.server_url);
            let server_log = open_server_log()?;
            let server_log_err = server_log.try_clone()?;
            hide_child_window(&mut command);
            let command = command
                .arg("--host")
                .arg("127.0.0.1")
                .arg("--port")
                .arg(port);
            let child = command
                .arg("-ngl")
                .arg("all")
                .arg("--fit")
                .arg("on")
                .arg("--fit-target")
                .arg("384")
                .arg("-c")
                .arg(
                    settings
                        .context_length_tokens
                        .clamp(2048, 32768)
                        .to_string(),
                )
                .arg("--parallel")
                .arg("1")
                .arg("--no-cache-idle-slots")
                .arg("--jinja")
                .arg("--flash-attn")
                .arg("on")
                .arg("--temp")
                .arg("0")
                .arg("--reasoning")
                .arg("off")
                .arg("--image-min-tokens")
                .arg(settings::valid_image_tokens(settings.image_tokens).to_string())
                .arg("--image-max-tokens")
                .arg(settings::valid_image_tokens(settings.image_tokens).to_string())
                .arg("--metrics")
                .arg("--no-webui")
                .stdout(Stdio::from(server_log))
                .stderr(Stdio::from(server_log_err))
                .spawn()
                .with_context(|| format!("failed to launch {}", settings.llama_server_path))?;
            *self.inner.child.lock() = Some(child);
        }

        for _ in 0..240 {
            if self.health(settings).await {
                self.warm_up(settings).await?;
                *self.inner.status.lock() = "warm".to_string();
                return Ok(());
            }
            if let Some(message) = self.exited_child_message().await? {
                *self.inner.status.lock() = "start failed".to_string();
                return Err(anyhow!(message));
            }
            sleep(Duration::from_secs(1)).await;
        }

        *self.inner.status.lock() = "start timeout".to_string();
        Err(anyhow!("llama server did not become ready"))
    }

    async fn exited_child_message(&self) -> anyhow::Result<Option<String>> {
        let exited = {
            let mut guard = self.inner.child.lock();
            let Some(child) = guard.as_mut() else {
                return Ok(None);
            };
            match child
                .try_wait()
                .context("failed to inspect llama server process")?
            {
                Some(status) => {
                    let mut child = guard.take().expect("child existed");
                    Some((status, child.stdout.take(), child.stderr.take()))
                }
                None => None,
            }
        };

        let Some((status, mut stdout, mut stderr)) = exited else {
            return Ok(None);
        };
        let mut output = String::new();
        if let Some(stream) = stdout.as_mut() {
            let _ = stream.read_to_string(&mut output).await;
        }
        if let Some(stream) = stderr.as_mut() {
            let _ = stream.read_to_string(&mut output).await;
        }
        let detail = output.trim();
        let detail = if !detail.is_empty() {
            detail.lines().take(12).collect::<Vec<_>>().join("\n")
        } else if let Some(tail) = recent_server_log_tail() {
            tail
        } else {
            "no llama-server output captured".to_string()
        };
        Ok(Some(format!(
            "llama server exited during startup ({status}):\n{detail}"
        )))
    }

    async fn warm_up(&self, settings: &Settings) -> anyhow::Result<()> {
        if *self.inner.warmed.lock() {
            return Ok(());
        }
        let _guard = self.inner.warmup_lock.lock().await;
        if *self.inner.warmed.lock() {
            return Ok(());
        }

        *self.inner.status.lock() = "warming".to_string();
        let warm_prompt = transcription_prompt(None, &settings.spoken_languages);
        let body = json!({
            "model": "local",
            "stream": true,
            "temperature": 0,
            "reasoning": "off",
            "max_tokens": 12,
            "response_format": {
                "type": "json_object"
            },
            "messages": [{
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": warm_prompt
                    },
                    {
                        "type": "input_audio",
                        "input_audio": {
                            "data": silent_wav_16k_base64(1000),
                            "format": "wav"
                        }
                    }
                ]
            }]
        });

        timeout(Duration::from_secs(60), self.chat(settings, body))
            .await
            .map_err(|_| anyhow!("model warm-up request timed out"))?
            .map_err(|err| {
                if let Some(tail) = recent_server_log_tail() {
                    anyhow!(
                        "model warm-up request failed: {err:#}\n\nRecent llama-server.log:\n{tail}"
                    )
                } else {
                    anyhow!("model warm-up request failed: {err:#}")
                }
            })?;
        *self.inner.warmed.lock() = true;
        Ok(())
    }

    pub async fn health(&self, settings: &Settings) -> bool {
        let url = format!("{}/health", settings.server_url.trim_end_matches('/'));
        self.inner
            .http
            .get(url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    pub async fn transcribe(
        &self,
        settings: &Settings,
        wav_path: &Path,
        context: Option<&WindowContext>,
    ) -> anyhow::Result<ModelResponse> {
        self.ensure_running(settings).await?;
        *self.inner.status.lock() = "transcribing".to_string();
        let wav = tokio::fs::read(wav_path).await?;
        let audio = general_purpose::STANDARD.encode(wav);
        let prompt = transcription_prompt(context, &settings.spoken_languages);
        let body = json!({
            "model": "local",
            "stream": true,
            "temperature": 0,
            "reasoning": "off",
            "response_format": {
                "type": "json_object"
            },
            "messages": [{
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": prompt
                    },
                    {
                        "type": "input_audio",
                        "input_audio": {
                            "data": audio,
                            "format": "wav"
                        }
                    }
                ]
            }]
        });
        let response = match self.chat(settings, body.clone()).await {
            Ok(response) => response,
            Err(err) if err.to_string().contains("did not contain streamed content") => {
                sleep(Duration::from_millis(180)).await;
                self.chat(settings, body).await?
            }
            Err(err) => return Err(err),
        };
        let transcript = extract_transcript(&response.content)?;
        *self.inner.status.lock() = "warm".to_string();
        Ok(ModelResponse {
            content: transcript,
            ..response
        })
    }

    pub async fn interpret_streaming_text(
        &self,
        settings: &Settings,
        transcript: &str,
        context: Option<&WindowContext>,
        recent_context: &[RecentTextContext],
        mode: InterpretationMode,
        mut on_text: impl FnMut(&str) -> anyhow::Result<()>,
    ) -> anyhow::Result<StreamingInterpretation> {
        self.ensure_running(settings).await?;
        *self.inner.status.lock() = mode.stage_name().to_string();
        let mut used_fallback_prompt = false;
        let (body, mut image_attached, mut prompt) =
            self.interpret_body(settings, transcript, context, false, recent_context, mode);
        let mut interpreted = match self
            .chat_with_streaming_text(settings, body, &mut on_text, StreamParseMode::PlainText)
            .await
        {
            Ok(ok) => ok,
            Err(primary_err) => {
                if is_context_length_error_text(&primary_err.to_string()) {
                    return Err(primary_err);
                }
                let (fallback_body, fallback_image_attached, fallback_prompt) =
                    self.interpret_fallback_body(settings, transcript, context, false, mode);
                used_fallback_prompt = true;
                image_attached = fallback_image_attached;
                prompt = fallback_prompt;
                self.chat_with_streaming_text(
                    settings,
                    fallback_body,
                    &mut on_text,
                    StreamParseMode::PlainText,
                )
                .await
                .map_err(|fallback_err| {
                    anyhow!(
                        "primary interpretation failed: {primary_err}; fallback interpretation failed: {fallback_err}"
                    )
                })?
            }
        };
        if response_needs_image(&interpreted.response.content)
            && context.and_then(|c| c.cursor_screenshot.as_ref()).is_some()
        {
            let (body, retry_image_attached, retry_prompt) = if used_fallback_prompt {
                self.interpret_fallback_body(settings, transcript, context, true, mode)
            } else {
                self.interpret_body(settings, transcript, context, true, recent_context, mode)
            };
            image_attached = retry_image_attached;
            prompt = retry_prompt;
            interpreted = self
                .chat_with_streaming_text(settings, body, &mut on_text, StreamParseMode::PlainText)
                .await?;
        }
        *self.inner.status.lock() = "warm".to_string();
        Ok(StreamingInterpretation {
            used_fallback_prompt,
            image_attached,
            prompt,
            ..interpreted
        })
    }

    pub async fn prompt_handoff(
        &self,
        settings: &Settings,
        transcript: &str,
        context: Option<&WindowContext>,
        wav_path: Option<&Path>,
        recent_context: &[RecentTextContext],
        mut on_text: impl FnMut(&str) -> anyhow::Result<()>,
    ) -> anyhow::Result<PromptHandoffResponse> {
        let provider = settings.prompt_provider.trim().to_ascii_lowercase();
        let use_custom = provider == "custom" || provider == "openai";
        if !use_custom {
            self.ensure_running(settings).await?;
        }

        let prompt = prompt_handoff_prompt(transcript, context, recent_context);
        let body_with_media =
            self.prompt_handoff_body(settings, &prompt, context, wav_path, use_custom).await?;
        let url = if use_custom && !settings.prompt_endpoint_url.trim().is_empty() {
            format!(
                "{}/v1/chat/completions",
                settings.prompt_endpoint_url.trim_end_matches('/')
            )
        } else {
            format!("{}/v1/chat/completions", settings.server_url.trim_end_matches('/'))
        };
        let api_key = if use_custom && !settings.prompt_api_key.trim().is_empty() {
            Some(settings.prompt_api_key.trim())
        } else {
            None
        };

        let mut used_media_fallback = false;
        let interpreted = match self
            .chat_with_streaming_text_at(
                &url,
                api_key,
                body_with_media,
                &mut on_text,
                StreamParseMode::JsonTextValue,
            )
            .await
        {
            Ok(ok) => ok,
            Err(media_err)
                if use_custom
                    && (wav_path.is_some()
                        || context.and_then(|c| c.cursor_screenshot.as_ref()).is_some()) =>
            {
                used_media_fallback = true;
                let fallback_body =
                    self.prompt_handoff_text_only_body(settings, &prompt, use_custom);
                self.chat_with_streaming_text_at(
                    &url,
                    api_key,
                    fallback_body,
                    &mut on_text,
                    StreamParseMode::JsonTextValue,
                )
                .await
                .map_err(|fallback_err| {
                    anyhow!(
                        "prompt handoff media request failed: {media_err}; text-only retry failed: {fallback_err}"
                    )
                })?
            }
            Err(err) => return Err(err),
        };

        let content = interpreted.response.content.trim();
        let value: serde_json::Value = serde_json::from_str(content)
            .with_context(|| format!("prompt handoff did not return JSON envelope: {content}"))?;
        let delivery = value["delivery"]
            .as_str()
            .unwrap_or("ui")
            .trim()
            .to_ascii_lowercase();
        let delivery = if delivery == "keyboard" { "keyboard" } else { "ui" }.to_string();
        let text = value["text"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string();
        if text.is_empty() {
            anyhow::bail!("prompt handoff JSON did not contain non-empty text");
        }
        Ok(PromptHandoffResponse {
            delivery,
            text,
            response: interpreted.response,
            used_media_fallback,
        })
    }

    fn interpret_fallback_body(
        &self,
        settings: &Settings,
        transcript: &str,
        context: Option<&WindowContext>,
        force_image: bool,
        mode: InterpretationMode,
    ) -> (serde_json::Value, bool, String) {
        let image_attached = should_attach_image(settings, context, force_image);
        let prompt = match mode {
            InterpretationMode::Fast => legacy_interpretation_prompt(
                transcript,
                context,
                image_attached,
                &settings.common_terms,
                &settings.spoken_languages,
            ),
            InterpretationMode::Thinking => compact_thinking_prompt(
                transcript,
                context,
                image_attached,
                &settings.common_terms,
                &settings.spoken_languages,
                &[],
            ),
        };
        let mut content = vec![json!({
            "type": "text",
            "text": prompt
        })];
        if image_attached {
            if let Some(image) = context.and_then(|c| c.cursor_screenshot.as_ref()) {
                content.push(json!({
                    "type": "image_url",
                    "image_url": {
                        "url": format!("data:image/png;base64,{}", image.png_base64)
                    }
                }));
            }
        }
        let body = json!({
            "model": "local",
            "stream": true,
            "temperature": 0,
            "messages": [{
                "role": "user",
                "content": content
            }]
        });
        (body, image_attached, prompt)
    }

    fn interpret_body(
        &self,
        settings: &Settings,
        transcript: &str,
        context: Option<&WindowContext>,
        force_image: bool,
        recent_context: &[RecentTextContext],
        mode: InterpretationMode,
    ) -> (serde_json::Value, bool, String) {
        let image_attached = should_attach_image(settings, context, force_image);
        let prompt = match mode {
            InterpretationMode::Fast => interpretation_prompt(
                transcript,
                context,
                image_attached,
                &settings.common_terms,
                &settings.spoken_languages,
                recent_context,
            ),
            InterpretationMode::Thinking => thinking_interpretation_prompt(
                transcript,
                context,
                image_attached,
                &settings.common_terms,
                &settings.spoken_languages,
                recent_context,
            ),
        };
        let mut content = vec![json!({
            "type": "text",
            "text": prompt
        })];
        if image_attached {
            if let Some(image) = context.and_then(|c| c.cursor_screenshot.as_ref()) {
                content.push(json!({
                    "type": "image_url",
                    "image_url": {
                        "url": format!("data:image/png;base64,{}", image.png_base64)
                    }
                }));
            }
        }

        let body = json!({
            "model": "local",
            "stream": true,
            "temperature": 0,
            "messages": [{
                "role": "user",
                "content": content
            }],
            "reasoning": if mode == InterpretationMode::Thinking { "on" } else { "off" }
        })
        .tap_mut(|body| {
            if mode == InterpretationMode::Thinking {
                body["reasoning_budget"] = json!(settings.thinking_handoff_reasoning_budget);
            }
        });
        (body, image_attached, prompt)
    }

    async fn prompt_handoff_body(
        &self,
        settings: &Settings,
        prompt: &str,
        context: Option<&WindowContext>,
        wav_path: Option<&Path>,
        use_custom: bool,
    ) -> anyhow::Result<serde_json::Value> {
        let model = if use_custom {
            nonempty(settings.prompt_model.trim(), "gpt-4.1")
        } else {
            "local"
        };
        let mut content = vec![json!({
            "type": "text",
            "text": prompt
        })];
        if let Some(path) = wav_path {
            let wav = tokio::fs::read(path).await?;
            content.push(json!({
                "type": "input_audio",
                "input_audio": {
                    "data": general_purpose::STANDARD.encode(wav),
                    "format": "wav"
                }
            }));
        }
        if let Some(image) = context.and_then(|c| c.cursor_screenshot.as_ref()) {
            content.push(json!({
                "type": "image_url",
                "image_url": {
                    "url": format!("data:image/png;base64,{}", image.png_base64)
                }
            }));
        }
        let mut body = json!({
            "model": model,
            "stream": true,
            "temperature": 0,
            "messages": [{
                "role": "user",
                "content": content
            }]
        });
        if !use_custom {
            body["reasoning"] = json!("on");
            body["reasoning_budget"] = json!(settings.thinking_handoff_reasoning_budget);
        }
        Ok(body)
    }

    fn prompt_handoff_text_only_body(
        &self,
        settings: &Settings,
        prompt: &str,
        use_custom: bool,
    ) -> serde_json::Value {
        let model = if use_custom {
            nonempty(settings.prompt_model.trim(), "gpt-4.1")
        } else {
            "local"
        };
        let mut body = json!({
            "model": model,
            "stream": true,
            "temperature": 0,
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "text",
                    "text": prompt
                }]
            }]
        });
        if !use_custom {
            body["reasoning"] = json!("on");
            body["reasoning_budget"] = json!(settings.thinking_handoff_reasoning_budget);
        }
        body
    }

    async fn chat(
        &self,
        settings: &Settings,
        body: serde_json::Value,
    ) -> anyhow::Result<ModelResponse> {
        self.chat_with_streaming_text(
            settings,
            body,
            &mut |_| Ok(()),
            StreamParseMode::JsonTextValue,
        )
        .await
        .map(|interpreted| interpreted.response)
    }

    async fn chat_with_streaming_text(
        &self,
        settings: &Settings,
        body: serde_json::Value,
        on_text: &mut impl FnMut(&str) -> anyhow::Result<()>,
        mode: StreamParseMode,
    ) -> anyhow::Result<StreamingInterpretation> {
        let url = format!(
            "{}/v1/chat/completions",
            settings.server_url.trim_end_matches('/')
        );
        self.chat_with_streaming_text_at(&url, None, body, on_text, mode)
            .await
    }

    async fn chat_with_streaming_text_at(
        &self,
        url: &str,
        api_key: Option<&str>,
        body: serde_json::Value,
        on_text: &mut impl FnMut(&str) -> anyhow::Result<()>,
        mode: StreamParseMode,
    ) -> anyhow::Result<StreamingInterpretation> {
        let started = Instant::now();
        let mut request = self.inner.http.post(url).json(&body);
        if let Some(key) = api_key.filter(|key| !key.trim().is_empty()) {
            request = request.bearer_auth(key.trim());
        }
        let response = request.timeout(Duration::from_secs(120)).send().await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!("llama server returned {status}: {}", body.trim()));
        }

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut content = String::new();
        let mut ttft_ms = None;
        let mut tokens_per_second = None;
        let mut predicted_tokens = None;
        let mut saw_visible = false;
        let mut json_stream_parser = StreamingTextParser::default();
        let mut plain_stream_parser = PlainStreamingTextParser::default();
        let mut streamed_text = String::new();

        while let Some(chunk) = stream.next().await {
            let text = String::from_utf8_lossy(&chunk?).to_string();
            buffer.push_str(&text);
            while let Some(newline) = buffer.find('\n') {
                let line = buffer[..newline].trim().to_string();
                buffer = buffer[newline + 1..].to_string();
                if let Some(data) = line.strip_prefix("data: ") {
                    if data == "[DONE]" {
                        continue;
                    }
                    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
                        continue;
                    };
                    if let Some(delta) = value["choices"][0]["delta"]["content"].as_str() {
                        update_ttft(delta, &mut saw_visible, &mut ttft_ms, started);
                        let pieces = match mode {
                            StreamParseMode::JsonTextValue => json_stream_parser.push(delta),
                            StreamParseMode::PlainText => plain_stream_parser.push(delta),
                        };
                        for text in pieces {
                            streamed_text.push_str(&text);
                            on_text(&text)?;
                        }
                        content.push_str(delta);
                    }
                    if let Some(message) = value["choices"][0]["message"]["content"].as_str() {
                        update_ttft(message, &mut saw_visible, &mut ttft_ms, started);
                        let pieces = match mode {
                            StreamParseMode::JsonTextValue => json_stream_parser.push(message),
                            StreamParseMode::PlainText => plain_stream_parser.push(message),
                        };
                        for text in pieces {
                            streamed_text.push_str(&text);
                            on_text(&text)?;
                        }
                        content.push_str(message);
                    }
                    if let Some(tps) = value["timings"]["predicted_per_second"].as_f64() {
                        tokens_per_second = Some(tps);
                    }
                    if let Some(tokens) = value["timings"]["predicted_n"].as_f64() {
                        predicted_tokens = Some(tokens);
                    }
                }
            }
        }

        let total_ms = started.elapsed().as_secs_f64() * 1000.0;
        if tokens_per_second.is_none() {
            if let (Some(tokens), Some(first)) = (predicted_tokens, ttft_ms) {
                let decode_seconds = ((total_ms - first) / 1000.0).max(0.001);
                tokens_per_second = Some(tokens / decode_seconds);
            }
        }
        if content.trim().is_empty() {
            return Err(anyhow!("llama response did not contain streamed content"));
        }
        Ok(StreamingInterpretation {
            response: ModelResponse {
                content,
                ttft_ms,
                tokens_per_second,
                total_ms: Some(total_ms),
            },
            streamed_text,
            used_fallback_prompt: false,
            image_attached: false,
            prompt: String::new(),
        })
    }
}

fn should_attach_image(
    settings: &Settings,
    context: Option<&WindowContext>,
    force_image: bool,
) -> bool {
    context
        .map(|c| {
            c.cursor_screenshot.is_some()
                && (force_image || settings.always_send_low_res_image || c.focused_text.is_none())
        })
        .unwrap_or(false)
}

fn is_context_length_error_text(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    lowered.contains("exceeds the available context size")
        || lowered.contains("context size")
        || lowered.contains("context length")
        || lowered.contains("too many tokens")
}

trait JsonValueTapMut {
    fn tap_mut(self, f: impl FnOnce(&mut serde_json::Value)) -> serde_json::Value;
}

impl JsonValueTapMut for serde_json::Value {
    fn tap_mut(mut self, f: impl FnOnce(&mut serde_json::Value)) -> serde_json::Value {
        f(&mut self);
        self
    }
}

pub fn transcription_prompt(context: Option<&WindowContext>, spoken_languages: &str) -> String {
    let context_hint = context
        .map(|c| {
            let kind = match detect_context_kind(Some(c)) {
                ContextKind::CmdPrompt => "a Windows Command Prompt",
                ContextKind::PowerShell => "a PowerShell terminal",
                ContextKind::BrowserAddressBar => "a browser address/search bar",
                ContextKind::SearchBox => "a search box",
                ContextKind::Generic => "",
            };
            if kind.is_empty() {
                String::new()
            } else {
                format!("\nContext hint: the user is dictating into {kind}; the audio may include short shell commands, URLs, search queries, or shortcut names like 'control c'.")
            }
        })
        .unwrap_or_default();
    let lang = spoken_languages.trim();
    let lang_hint = if lang.is_empty() {
        String::new()
    } else {
        format!(
            "\nSpoken language hint: the user may speak {lang}. Prefer only these languages when decoding ambiguous audio. Keep the original spoken language exactly as spoken, do not translate, do not paraphrase, do not romanize, and do not drift into another language unless the audio clearly does so."
        )
    };
    format!(
        "Transcribe exactly what was said. Return only a JSON object: {{\"transcript\":\"...\"}}.{lang_hint}{context_hint}"
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContextKind {
    CmdPrompt,
    PowerShell,
    BrowserAddressBar,
    SearchBox,
    Generic,
}

fn context_extra_instructions(kind: ContextKind) -> String {
    match kind {
        ContextKind::CmdPrompt => terminal_context_prompt("cmd"),
        ContextKind::PowerShell => terminal_context_prompt("powershell"),
        ContextKind::BrowserAddressBar => browser_address_context_prompt(),
        ContextKind::SearchBox => search_box_extra_instructions(),
        ContextKind::Generic => String::new(),
    }
}

pub fn context_kind_label(context: Option<&WindowContext>) -> &'static str {
    match detect_context_kind(context) {
        ContextKind::CmdPrompt => "Windows Command Prompt",
        ContextKind::PowerShell => "PowerShell terminal",
        ContextKind::BrowserAddressBar => "browser address/search bar",
        ContextKind::SearchBox => "search box",
        ContextKind::Generic => "generic",
    }
}

fn detect_context_kind(context: Option<&WindowContext>) -> ContextKind {
    let Some(ctx) = context else {
        return ContextKind::Generic;
    };
    let app = ctx.app_name.to_ascii_lowercase();
    let title = ctx.title.to_ascii_lowercase();

    if app.contains("cmd.exe") || title.contains("command prompt") {
        return ContextKind::CmdPrompt;
    }
    if app.contains("powershell") || app.contains("pwsh") || title.contains("powershell") {
        return ContextKind::PowerShell;
    }
    if app.contains("windowsterminal") || app.contains("windows terminal") {
        return if title.contains("cmd") || title.contains("command prompt") {
            ContextKind::CmdPrompt
        } else {
            ContextKind::PowerShell
        };
    }

    let is_browser = app.contains("chrome")
        || app.contains("msedge")
        || app.contains("firefox")
        || app.contains("brave")
        || app.contains("opera")
        || app.contains("vivaldi");

    if let Some(ft) = &ctx.focused_text {
        let class = ft.class_name.as_deref().unwrap_or("").to_ascii_lowercase();
        let name = ft
            .element_name
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase();
        let aid = ft
            .automation_id
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase();
        let p_name = ft.parent_name.as_deref().unwrap_or("").to_ascii_lowercase();
        let p_class = ft
            .parent_class
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase();
        let ctype = ft
            .control_type
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase();

        // Browser address / omnibox / urlbar — broader heuristics now using AutomationId + parent
        if is_browser {
            let is_address = class.contains("omnibox")
                || class.contains("urlbar")
                || aid.contains("urlbar")
                || aid.contains("address")
                || aid.contains("omnibox")
                || aid == "urlinput"
                || name.contains("address")
                || name.contains("location")
                || name.contains("address and search")
                || p_name.contains("address")
                || p_name.contains("navigation")
                || p_class.contains("omnibox")
                || p_class.contains("urlbar");
            if is_address {
                return ContextKind::BrowserAddressBar;
            }
        }

        // In-page or app search boxes (Amazon search, YouTube search, file explorer search, etc.)
        let looks_like_search =
            (ctype.contains("edit") || ctype.contains("text") || ctype.is_empty())
                && (name.contains("search")
                    || aid.contains("search")
                    || p_name.contains("search")
                    || p_class.contains("search"))
                && !name.contains("search results");
        if looks_like_search {
            return ContextKind::SearchBox;
        }
    }

    ContextKind::Generic
}

#[allow(dead_code)]
fn terminal_extra_instructions(shell: &str) -> String {
    let is_ps = shell == "powershell";
    let (list, list_all, cd, search, del, show, clear) = if is_ps {
        (
            "Get-ChildItem",
            "Get-ChildItem -Force",
            "Set-Location",
            "Get-ChildItem -Recurse -Filter *pattern*",
            "Remove-Item",
            "Get-Content",
            "Clear-Host",
        )
    } else {
        (
            "dir",
            "dir /a",
            "cd",
            "dir /s /b *pattern*",
            "del",
            "type",
            "cls",
        )
    };
    format!(
        "\n\
=== {SH} TERMINAL — ACTIVE ===\n\
Output ONLY the shell command followed by {{{{Enter}}}}. Never output plain English.\n\
Infer the most likely short shell command from natural wording instead of typing the request literally.\n\
Common voice-to-command mappings ({SH}):\n\
  list files / what is here / show contents    -> {list}{{{{Enter}}}}\n\
  list all including hidden                    -> {list_all}{{{{Enter}}}}\n\
  go to / navigate to / open folder X         -> {cd} X{{{{Enter}}}}\n\
  go up / parent folder                        -> cd ..{{{{Enter}}}}\n\
  search for files named / matching X         -> {search}{{{{Enter}}}}\n\
  delete / remove file X                       -> {del} X{{{{Enter}}}}\n\
  show / print / read file X                  -> {show} X{{{{Enter}}}}\n\
  clear screen                                 -> {clear}{{{{Enter}}}}\n\
  create folder X                              -> mkdir X{{{{Enter}}}}\n\
  open file X in VS Code                       -> code X{{{{Enter}}}}\n\
  run Python script X                          -> python X{{{{Enter}}}}\n\
  git status                                   -> git status{{{{Enter}}}}\n\
  git add all / stage all                      -> git add .{{{{Enter}}}}\n\
  git commit with message X                   -> git commit -m \"X\"{{{{Enter}}}}\n\
  git push                                     -> git push{{{{Enter}}}}\n\
  npm install                                  -> npm install{{{{Enter}}}}\n\
  npm build / build                            -> npm run build{{{{Enter}}}}\n\
  npm dev / run dev server                     -> npm run dev{{{{Enter}}}}\n\
  cargo check / build                          -> cargo check{{{{Enter}}}}\n\
  show / list ip address                       -> ipconfig{{{{Enter}}}}\n\
=== END TERMINAL ===\n",
        SH = if is_ps { "POWERSHELL" } else { "CMD" },
        list = list, list_all = list_all, cd = cd,
        search = search, del = del, show = show, clear = clear,
    )
}

#[allow(dead_code)]
fn browser_address_extra_instructions() -> String {
    "\n\
=== BROWSER ADDRESS BAR — ACTIVE ===\n\
Prefer a complete URL or search query followed by {{Enter}} when navigation intent is strong.\n\
If the transcript is clearly partial inline editing, or the destination is already fully present in the field, do not append duplicate text or an unnecessary {{Enter}}.\n\
Infer common sites and searches from natural phrasing; do not require an exact memorized example.\n\
Rules: spaces in queries become +. Replace X with the actual spoken content.\n\
  search for X / google X                     -> https://www.google.com/search?q=X{{Enter}}\n\
  search images of X                           -> https://www.google.com/search?q=X&tbm=isch{{Enter}}\n\
  search Amazon for X / buy X                 -> https://www.amazon.in/s?k=X{{Enter}}\n\
  search YouTube for X / YouTube X            -> https://www.youtube.com/results?search_query=X{{Enter}}\n\
  Wikipedia X / what is X                     -> https://en.wikipedia.org/wiki/X{{Enter}}\n\
  GitHub X / find repo X                      -> https://github.com/search?q=X{{Enter}}\n\
  Stack Overflow X                             -> https://stackoverflow.com/search?q=X{{Enter}}\n\
  maps to X / directions to X                 -> https://maps.google.com/maps?q=X{{Enter}}\n\
  Flipkart X / buy X on Flipkart             -> https://www.flipkart.com/search?q=X{{Enter}}\n\
  news about X                                 -> https://news.google.com/search?q=X{{Enter}}\n\
  open Gmail                                   -> https://mail.google.com{{Enter}}\n\
  open YouTube                                 -> https://www.youtube.com{{Enter}}\n\
  open Netflix                                 -> https://www.netflix.com{{Enter}}\n\
  open Facebook                                -> https://www.facebook.com{{Enter}}\n\
  open Twitter / open X                       -> https://www.twitter.com{{Enter}}\n\
  open LinkedIn                                -> https://www.linkedin.com{{Enter}}\n\
  open WhatsApp web                            -> https://web.whatsapp.com{{Enter}}\n\
  open GitHub                                  -> https://www.github.com{{Enter}}\n\
  open Amazon                                  -> https://www.amazon.in{{Enter}}\n\
  open Google Drive                            -> https://drive.google.com{{Enter}}\n\
  open Google Docs                             -> https://docs.google.com{{Enter}}\n\
  open Google Sheets                           -> https://sheets.google.com{{Enter}}\n\
  open ChatGPT                                 -> https://chat.openai.com{{Enter}}\n\
For a domain or brand name alone: add https:// prefix and {{Enter}}.\n\
=== END ADDRESS BAR ===\n".to_string()
}

fn terminal_context_prompt(shell: &str) -> String {
    let is_ps = shell == "powershell";
    let (name, list, list_all, cd, find, delete, show, clear) = if is_ps {
        (
            "POWERSHELL",
            "Get-ChildItem",
            "Get-ChildItem -Force",
            "Set-Location",
            "Get-ChildItem -Recurse -Filter *X*",
            "Remove-Item",
            "Get-Content",
            "Clear-Host",
        )
    } else {
        (
            "CMD",
            "dir",
            "dir /a",
            "cd",
            "dir /s /b *X*",
            "del",
            "type",
            "cls",
        )
    };
    format!(
        "\n\
=== {name} PROMPT ACTIVE ===\n\
Output one short {name} command, then {{{{Enter}}}}. Do not explain.\n\
Map obvious speech to commands: list files -> {list}{{{{Enter}}}} | list hidden -> {list_all}{{{{Enter}}}} | go to X -> {cd} X{{{{Enter}}}} | go up -> cd ..{{{{Enter}}}}.\n\
Find file X -> {find}{{{{Enter}}}} | show X -> {show} X{{{{Enter}}}} | delete X -> {delete} X{{{{Enter}}}} | clear -> {clear}{{{{Enter}}}}.\n\
Also allow direct commands like git status, npm run dev, npm run build, cargo check, python script.py, code ., ipconfig, mkdir X. Always append {{{{Enter}}}}.\n\
=== END TERMINAL ===\n",
        name = name,
        list = list,
        list_all = list_all,
        cd = cd,
        find = find,
        delete = delete,
        show = show,
        clear = clear,
    )
}

fn browser_address_context_prompt() -> String {
    "\n\
=== BROWSER ADDRESS BAR ACTIVE ===\n\
For any complete URL, domain, site name, navigation request, or web search, output the destination followed by {{Enter}}.\n\
Skip {{Enter}} only for an explicit partial edit/continuation, or when the current field already fully contains the destination.\n\
Use direct URLs for popular searches. Encode query spaces as +:\n\
Google X -> https://www.google.com/search?q=X{{Enter}} | images X -> https://www.google.com/search?q=X&tbm=isch{{Enter}} | news X -> https://news.google.com/search?q=X{{Enter}}\n\
YouTube X -> https://www.youtube.com/results?search_query=X{{Enter}} | Amazon X -> https://www.amazon.in/s?k=X{{Enter}} | Flipkart X -> https://www.flipkart.com/search?q=X{{Enter}}\n\
Wikipedia X -> https://en.wikipedia.org/wiki/X{{Enter}} | GitHub X -> https://github.com/search?q=X{{Enter}} | Stack Overflow X -> https://stackoverflow.com/search?q=X{{Enter}} | maps X -> https://maps.google.com/maps?q=X{{Enter}}\n\
Open common sites directly: Gmail, YouTube, Netflix, Facebook, X/Twitter, LinkedIn, WhatsApp Web, GitHub, Amazon, Drive, Docs, Sheets, ChatGPT. Always append {{Enter}}.\n\
=== END ADDRESS BAR ===\n"
        .to_string()
}

fn search_box_extra_instructions() -> String {
    "\n\
=== SEARCH BOX ACTIVE ===\n\
Output only the final search query text followed by {{Enter}}. Do not create a URL unless this is also a browser address bar.\n\
Example: search robot videos -> robot videos{{Enter}}.\n\
=== END SEARCH BOX ===\n"
        .to_string()
}

pub fn interpretation_prompt(
    transcript: &str,
    context: Option<&WindowContext>,
    image_attached: bool,
    common_terms: &str,
    spoken_languages: &str,
    recent_context: &[RecentTextContext],
) -> String {
    few_shot_interpretation_prompt(
        transcript,
        context,
        image_attached,
        common_terms,
        spoken_languages,
        recent_context,
    )
}

pub fn thinking_interpretation_prompt(
    transcript: &str,
    context: Option<&WindowContext>,
    image_attached: bool,
    common_terms: &str,
    spoken_languages: &str,
    recent_context: &[RecentTextContext],
) -> String {
    compact_thinking_prompt(
        transcript,
        context,
        image_attached,
        common_terms,
        spoken_languages,
        recent_context,
    )
}

pub fn prompt_handoff_prompt(
    transcript: &str,
    context: Option<&WindowContext>,
    recent_context: &[RecentTextContext],
) -> String {
    let context_text = format_window_context(context, true);
    let recent_context_section = format_recent_context(recent_context);
    format!(
        "You are the second-level Prompt agent for Voice Keyboard.\n\
The local interpreter handed this request to you because it needs a stronger answer, rewrite, summary, translation, or longer composition.\n\
\n\
Return exactly one JSON object and no Markdown/code fence/explanation outside it:\n\
{{\"delivery\":\"ui\"|\"keyboard\",\"text\":\"...\"}}\n\
\n\
Delivery rules:\n\
- Use \"ui\" for direct questions, explanations, summaries to read, and answers the user likely wants shown in the popup.\n\
- Use \"keyboard\" for text that should be inserted into the active field or replace selected text.\n\
- The app will decide insert vs replace from the captured selection; do not include labels like Insert or Replace in the text.\n\
- Preserve the selected text language unless the user asks to translate.\n\
- Return only the final useful content in text.\n\
\n\
{recent_context_section}{context_text}\n\
Transcript/request: {transcript}\n\
JSON:",
        recent_context_section = recent_context_section,
        context_text = context_text,
        transcript = transcript,
    )
}

fn few_shot_interpretation_prompt(
    transcript: &str,
    context: Option<&WindowContext>,
    image_attached: bool,
    common_terms: &str,
    spoken_languages: &str,
    recent_context: &[RecentTextContext],
) -> String {
    let context_text = format_window_context(context, image_attached);
    let common_terms_section = if common_terms.trim().is_empty() {
        String::new()
    } else {
        format!(
            "Common terms / preferred spellings:\n{}\nUse these as high-priority hints for ambiguous ASR, especially names, emails, companies, products, and repeated personal terms. Prefer these exact spellings when they fit the transcript.\n\n",
            common_terms.trim()
        )
    };
    let spoken_language_section = if spoken_languages.trim().is_empty() {
        String::new()
    } else {
        format!(
            "Spoken languages: {}. Prefer only these languages for ambiguous words. Keep the output in the same language as the spoken request or selected text unless the user clearly asks to translate. Do not switch to English by default.\n\n",
            spoken_languages.trim()
        )
    };
    let recent_context_section = format_recent_context(recent_context);
    let extra = context_extra_instructions(detect_context_kind(context));
    let image_instruction = image_context_instruction(image_attached);
    format!(
        "You are a voice keyboard interpreter.\n\
Return only the exact text to type, or shortcut tokens like {{{{Enter}}}}, {{{{Ctrl+Z}}}}, {{{{Ctrl+A}}}}.\n\
Do not return JSON. Do not explain. Do not quote the answer.\n\
\n\
Default behaviour: just type the user's words as keystrokes. This is true regardless of which app is focused. The user dictates so their words land in whatever field is active; do not second-guess them. The handoff tools below are reserved for rare, exceptional cases — the vast majority of utterances must pass through as text or a known shortcut.\n\
\n\
Questions, requests, instructions, and chat-style sentences are NOT a reason to use a handoff tool. If the user said something that could plausibly just be typed into the current field, type it. Examples that must be typed verbatim, not handed off: 'why is the build still failing', 'what is the capital of France', 'can you check the logs', 'tell me about the new feature', 'how do I undo this commit'. The user can decide for themselves whether to send those words to a person, an LLM, a search bar, or a chat field — your job is only to type them.\n\
\n\
Handoff tools (use only when the user clearly asks Voice Keyboard itself to author or transform content right now, not when they are typing a message that asks someone else to do it):\n\
- Output exactly {{{{Prompt}}}} ONLY when the user explicitly asks the local assistant to AUTHOR or REWRITE content that spans multiple sentences. The handoff is for cases where the generated/rewritten output is clearly more than one sentence — e.g. drafting an email, a paragraph, a multi-line message, a summary of selected text, a translation of a paragraph, or rewriting a multi-sentence selection in a different tone. Required triggers (at least one must be present): explicit verbs like 'write', 'draft', 'compose', 'rewrite', 'summarize', 'summarise', 'translate', 'polish', 'proofread', 'paraphrase', 'expand', 'shorten', AND either (a) selected text of more than one sentence to transform, or (b) a clearly multi-sentence authoring request. Single-sentence dictation, short factual questions, short imperative sentences, normal chat-style sentences, requests phrased to a person, single-sentence rewrites of a single-sentence selection, and anything the user could reasonably want typed verbatim are NOT handoffs.\n\
- Output exactly {{{{agentic}}}} ONLY when the user explicitly requests multi-step computer-use work that requires file/clipboard/notes/project access, such as 'save this to my notes', 'put this on the clipboard', 'save to the project folder', 'open my notes and append', 'apply this patch'. The app will show a placeholder; do not attempt the steps yourself.\n\
- If you are not certain a handoff is needed, do not use one. Prefer typing over routing. Routing is wrong if the user just wanted their words typed.\n\
- Do not use handoff tools for simple dictation, questions phrased conversationally, browser navigation, search/address bar text, terminal commands, or direct key presses.\n\
\n\
Core rules:\n\
- If the whole transcript is a verbal shortcut such as 'press enter', 'undo', 'redo', 'copy', 'paste', 'select all', 'tab', 'escape', 'backspace', 'delete', or arrow keys, output only the shortcut token.\n\
- Treat common ASR confusions like 'and do' as undo, and phrases like 'do control c' as control c, when the intent is clearly a shortcut.\n\
- If selected text is present, output the replacement text only.\n\
- If the current field state already contains the destination or query, do not repeat it.\n\
- In a browser address bar, append {{{{Enter}}}} after complete domains, URLs, site names, navigation requests, and web searches.\n\
- For partial inline edits inside an address bar or existing text, output only the missing text and skip {{{{Enter}}}}.\n\
- In a terminal, infer the likely shell command from natural language and end it with {{{{Enter}}}}.\n\
- Use text before and after the cursor to decide spacing and capitalization.\n\
- At the start of a sentence or empty field, capitalize naturally.\n\
- If the cursor is next to an existing word and the dictated text starts a new word, include one leading space.\n\
- If the cursor is in the middle of an email, URL, or word, continue inline without adding a space.\n\
- Phrases like 'on the next line type ...' or 'new paragraph ...' mean insert {{{{Enter}}}} or {{{{Enter}}}}{{{{Enter}}}} before the remaining text.\n\
- Avoid duplicate spaces and duplicate punctuation.\n\
- {image_instruction}\n\
\n\
Few-shot examples:\n\
Transcript: press enter\n\
Output: {{{{Enter}}}}\n\
\n\
Transcript: undo\n\
Output: {{{{Ctrl+Z}}}}\n\
\n\
Transcript: and do\n\
Output: {{{{Ctrl+Z}}}}\n\
\n\
Transcript: select all\n\
Output: {{{{Ctrl+A}}}}\n\
\n\
Transcript: do control c\n\
Output: {{{{Ctrl+C}}}}\n\
\n\
Transcript: what is the capital of the US\n\
Output: what is the capital of the US\n\
\n\
Transcript: why is it still taking so long to get the diagnostics\n\
Output: Why is it still taking so long to get the diagnostics?\n\
\n\
Transcript: check whether the changes were updated on the usb\n\
Output: check whether the changes were updated on the USB\n\
\n\
Transcript: hey voice keyboard, write a long email apologising for the delay\n\
Output: {{{{Prompt}}}}\n\
\n\
Field state: <<<SELECTED:I will not be able to attend tomorrow's meeting because of personal reasons. Please let me know if there is anything I should hand over before I am away.>>>\n\
Transcript: rewrite this in a more formal tone\n\
Output: {{{{Prompt}}}}\n\
\n\
Field state: <<<SELECTED:see you soon>>>\n\
Transcript: rewrite this more formally\n\
Output: I look forward to seeing you soon.\n\
\n\
Transcript: save this to my notes\n\
Output: {{{{agentic}}}}\n\
\n\
Transcript: put my address on the clipboard\n\
Output: {{{{agentic}}}}\n\
\n\
Transcript: delete this\n\
Output: {{{{Delete}}}}\n\
\n\
Field state: gmail.com<<<CURSOR>>>\n\
Transcript: press enter\n\
Output: {{{{Enter}}}}\n\
\n\
Field state: name.surname@<<<CURSOR>>>\n\
Transcript: gmail dot com\n\
Output: gmail.com\n\
\n\
Address bar is active.\n\
Transcript: gmail dot com\n\
Output: gmail.com{{{{Enter}}}}\n\
\n\
Address bar is active.\n\
Transcript: open youtube\n\
Output: https://www.youtube.com{{{{Enter}}}}\n\
\n\
Address bar is active.\n\
Transcript: search amazon for laptops\n\
Output: https://www.amazon.in/s?k=laptops{{{{Enter}}}}\n\
\n\
Search box is active.\n\
Transcript: search robot videos\n\
Output: robot videos{{{{Enter}}}}\n\
\n\
Terminal is active.\n\
Transcript: list machine ip address\n\
Output: ipconfig{{{{Enter}}}}\n\
\n\
Field state: Hello<<<CURSOR>>>\n\
Transcript: world\n\
Output:  world\n\
\n\
Field state: Hello<<<CURSOR>>>\n\
Transcript: on the next line type thank you for your help\n\
Output: {{{{Enter}}}}Thank you for your help.\n\
\n\
Field state: Hello. <<<CURSOR>>>\n\
Transcript: how are you\n\
Output: How are you?\n\
\n\
Field state: <<<SELECTED:this are bad sentence.>>>\n\
Transcript: fix the grammar\n\
Output: This is a bad sentence.\n\
\n\
Field state: <<<SELECTED:good morning team>>>\n\
Transcript: translate this also into spanish\n\
Output: Buenos dias, equipo.\n\
\n\
{spoken_language_section}{common_terms_section}{recent_context_section}{extra}{context_text}\n\
Transcript: {transcript}\n\
Output:",
        spoken_language_section = spoken_language_section,
        common_terms_section = common_terms_section,
        recent_context_section = recent_context_section,
        extra = extra,
        image_instruction = image_instruction,
        context_text = context_text,
        transcript = transcript,
    )
}

fn compact_thinking_prompt(
    transcript: &str,
    context: Option<&WindowContext>,
    image_attached: bool,
    common_terms: &str,
    spoken_languages: &str,
    recent_context: &[RecentTextContext],
) -> String {
    let context_text = format_window_context(context, image_attached);
    let common_terms_section = if common_terms.trim().is_empty() {
        String::new()
    } else {
        format!(
            "Common terms / preferred spellings:\n{}\nUse these as high-priority hints for ambiguous ASR, especially names, emails, companies, products, and repeated personal terms. Prefer these exact spellings when they fit the transcript.\n\n",
            common_terms.trim()
        )
    };
    let spoken_language_section = if spoken_languages.trim().is_empty() {
        String::new()
    } else {
        format!(
            "Spoken languages: {}. Preserve the current language and script unless the transcript explicitly asks for translation. If the user asks to translate selected text, return only the translated text in the requested language. If a large amount of text is selected and no translation was requested, keep the selected text in its original language.\n\n",
            spoken_languages.trim()
        )
    };
    let recent_context_section = format_recent_context(recent_context);
    let extra = context_extra_instructions(detect_context_kind(context));
    let image_instruction = image_context_instruction(image_attached);
    format!(
        "You are the rewrite handoff for a voice keyboard.\n\
Return only the final text to paste, or a shortcut token if the user explicitly asks for a key press.\n\
Do not explain. Do not return JSON. Do not add notes.\n\
\n\
If the task needs a stronger second-stage answer or deep transformation, output exactly {{{{Prompt}}}} instead of doing it here.\n\
If the task asks for coding mode, computer use, clipboard changes, saving to project folder, or saving to notes, output exactly {{{{agentic}}}}.\n\
\n\
Use the transcript as the instruction. Prefer transforming selected text when it exists.\n\
Look at the cursor context to preserve spacing, punctuation, and capitalization.\n\
Do not repeat stale text from recent history unless it is clearly needed.\n\
When selected text exists, focus on that selected text and only the nearby before/after snippets.\n\
For long selected passages, preserve the original language and script unless the transcript explicitly asks for translation.\n\
{image_instruction}\n\
\n\
Few-shot examples:\n\
Selected text: this are bad sentence.\n\
Transcript: fix the grammar\n\
Output: This is a bad sentence.\n\
\n\
Selected text: we done the work yesterday and it are fine\n\
Transcript: rewrite this professionally\n\
Output: We completed the work yesterday, and it is in good shape.\n\
\n\
Selected text: buenos dias equipo\n\
Transcript: translate to english\n\
Output: Good morning, team.\n\
\n\
Selected text: good morning team\n\
Transcript: translate this also into spanish\n\
Output: Buenos dias, equipo.\n\
\n\
Selected text: the launch is tomorrow. please make it shorter.\n\
Transcript: shorten this\n\
Output: Launch is tomorrow.\n\
\n\
Transcript: press enter\n\
Output: {{{{Enter}}}}\n\
\n\
{spoken_language_section}{common_terms_section}{recent_context_section}{extra}{context_text}\n\
Transcript: {transcript}\n\
Final output:",
        spoken_language_section = spoken_language_section,
        common_terms_section = common_terms_section,
        recent_context_section = recent_context_section,
        extra = extra,
        image_instruction = image_instruction,
        context_text = context_text,
        transcript = transcript,
    )
}

fn legacy_interpretation_prompt(
    transcript: &str,
    context: Option<&WindowContext>,
    image_attached: bool,
    common_terms: &str,
    spoken_languages: &str,
) -> String {
    build_interpretation_prompt(
        transcript,
        context,
        image_attached,
        common_terms,
        spoken_languages,
        &[],
        InterpretationMode::Fast,
    )
}

fn build_interpretation_prompt(
    transcript: &str,
    context: Option<&WindowContext>,
    image_attached: bool,
    common_terms: &str,
    spoken_languages: &str,
    recent_context: &[RecentTextContext],
    mode: InterpretationMode,
) -> String {
    let context_text = format_window_context(context, image_attached);
    let common_terms_section = if common_terms.trim().is_empty() {
        String::new()
    } else {
        format!(
            "Common terms / preferred spellings for this user:\n{}\nUse these as high-priority hints for ambiguous ASR, especially names, emails, companies, products, and repeated personal terms. Prefer these exact spellings when they fit the transcript.\n",
            common_terms.trim()
        )
    };
    let spoken_language_section = if spoken_languages.trim().is_empty() {
        String::new()
    } else {
        format!(
            "Spoken language context:\nThe user's dictated audio may be in {}. Respect these languages when resolving ambiguous words. Do not \"correct\" into another language unless the transcript clearly requires it.\n\n",
            spoken_languages.trim()
        )
    };
    let recent_context_section = format_recent_context(recent_context);
    let extra = context_extra_instructions(detect_context_kind(context));
    let mode_prefix = match mode {
        InterpretationMode::Fast => "You are a safe voice keyboard interpreter. Read the transcript carefully first, then use context only to resolve ambiguity.",
        InterpretationMode::Thinking => "You are a careful rewrite and editing assistant inside a voice keyboard. Think through the requested transformation, but return only the final pasteable result with no explanation.",
    };
    let mode_rules = match mode {
        InterpretationMode::Fast => String::new(),
        InterpretationMode::Thinking => "\nTHINKING HANDOFF MODE:\n- Use the transcript as an instruction applied to the selected text or nearby field content.\n- Prefer rewriting, polishing, summarizing, translating, or grammar correction over shortcut execution unless the request is clearly just a key press.\n- Return only the final text to paste, or a shortcut token if the user explicitly asked for a key press.\n- Preserve the meaning of the source text unless the transcript explicitly asks for a stronger transformation.\n".to_string(),
    };
    format!(
        "{mode_prefix}\n\
Return plain text only. No JSON, Markdown, explanations, code fences, labels, or quotes. Output only what should be typed or the shortcut token.\n\
\n\
{mode_rules}\n\
{spoken_language_section}\
\n\
CRITICAL RULE — DO NOT ECHO EXISTING FIELD CONTENT:\n\
The 'Current field state' shows a marker <<<CURSOR>>> or <<<SELECTED:...>>> with text around it.\n\
ALL text shown around those markers is ALREADY TYPED in the field. The user can already see it.\n\
Your job is to output ONLY what should be ADDED at the cursor, or what KEY should be PRESSED, or what should REPLACE the selection.\n\
NEVER re-output text that already appears in the field state. NEVER re-construct a URL or query that the field already contains.\n\
Examples:\n\
  Field state: 'gmail.com<<<CURSOR>>>' + transcript 'press enter' → output {{{{Enter}}}} (NOT 'gmail.com{{{{Enter}}}}').\n\
  Field state: 'Robotics Group NIT<<<CURSOR>>>' + transcript 'press enter' → output {{{{Enter}}}}.\n\
  Field state: 'name.surname@<<<CURSOR>>>' + transcript 'gmail dot com' → output gmail.com (cursor is mid-text, no Enter).\n\
  Field state: '<<<SELECTED:hello world>>>' + transcript 'goodbye' → output goodbye (replaces the selection).\n\
  Field state: '(empty)' + transcript 'open YouTube' → output https://www.youtube.com{{{{Enter}}}}.\n\
  Field state: '(empty)' + transcript 'hello there' → output Hello there.\n\
  Field state: 'Hello<<<CURSOR>>>' + transcript 'world' → output  world.\n\
  Field state: 'Hello <<<CURSOR>>>' + transcript 'world' → output world.\n\
  Field state: 'Hello. <<<CURSOR>>>' + transcript 'how are you' → output How are you?\n\
\n\
\n\
STEP 1 — DIRECT KEY PRESS / VOICE COMMAND (HIGHEST PRIORITY — when the WHOLE transcript matches one of these, output ONLY the shortcut token, ignore field content, address bar, search box, terminal modes, everything):\n\
DIRECT KEY PRESSES (transcript is just the key name, possibly preceded by 'press' or 'hit'):\n\
- 'enter' / 'press enter' / 'hit enter' / 'submit' / 'go' / 'return' → {{{{Enter}}}}.\n\
- 'tab' / 'press tab' → {{{{Tab}}}}.\n\
- 'escape' / 'esc' / 'press escape' → {{{{Escape}}}}.\n\
- 'backspace' / 'press backspace' → {{{{Backspace}}}}.\n\
- 'space' / 'press space' / 'space bar' → {{{{Space}}}}.\n\
- 'up' / 'press up' / 'arrow up' → {{{{Up}}}}.\n\
- 'down' / 'press down' / 'arrow down' → {{{{Down}}}}.\n\
- 'left' / 'press left' / 'arrow left' → {{{{Left}}}}.\n\
- 'right' / 'press right' / 'arrow right' → {{{{Right}}}}.\n\
- 'home' / 'press home' → {{{{Home}}}}. 'end' / 'press end' → {{{{End}}}}.\n\
- 'page up' / 'pageup' → {{{{PageUp}}}}. 'page down' / 'pagedown' → {{{{PageDown}}}}.\n\
\n\
CTRL / ALT SHORTCUTS (transcript is the modifier+letter, no other content):\n\
- 'control c' / 'ctrl c' / 'ctrl see' / 'control see' / 'copy' / 'copy this' / 'copy that' → {{{{Ctrl+C}}}}.\n\
- 'control x' / 'ctrl x' / 'cut' / 'cut this' → {{{{Ctrl+X}}}}.\n\
- 'control v' / 'ctrl v' / 'paste' → {{{{Ctrl+V}}}}.\n\
- 'control z' / 'ctrl z' / 'undo' → {{{{Ctrl+Z}}}}.\n\
- 'control y' / 'ctrl y' / 'redo' → {{{{Ctrl+Y}}}}.\n\
- 'control a' / 'ctrl a' / 'select all' → {{{{Ctrl+A}}}}.\n\
- 'control s' / 'ctrl s' / 'save' → {{{{Ctrl+S}}}}.\n\
- 'control f' / 'ctrl f' / 'find' / 'search this page' → {{{{Ctrl+F}}}}.\n\
- 'control t' / 'ctrl t' / 'new tab' → {{{{Ctrl+T}}}}.\n\
- 'control w' / 'ctrl w' / 'close tab' → {{{{Ctrl+W}}}}.\n\
- 'alt tab' → {{{{Alt+Tab}}}}.\n\
- A spelled-out 'control + LETTER' / 'ctrl + LETTER' → {{{{Ctrl+LETTER}}}}.\n\
\n\
DELETE / CLEAR INTENT:\n\
- 'delete this' / 'delete selected' / 'delete that' / 'remove this' / 'erase this' / 'clear this' → {{{{Delete}}}}.\n\
\n\
WHEN STEP 1 MATCHES, STOP. Output only the shortcut token. Do NOT echo any field content.\n\
\n\
STEP 2 — WRAPPING IN BRACKETS (look for the phrase 'in brackets', 'in square brackets', 'in curly braces'):\n\
- The content BEFORE 'in brackets' / 'in square brackets' is wrapped: 'hello world in brackets' → [hello world]; 'write hello in brackets' → write [hello].\n\
- 'in brackets' after a preposition or verb applies to the following content: 'write in brackets if that is there' → write [if that is there].\n\
- 'in curly braces': 'hello in curly braces' → {{hello}}.\n\
\n\
STEP 3 — GENERAL DICTATION:\n\
- For key presses embed shortcut tokens: {{{{Enter}}}}, {{{{Tab}}}}, {{{{Escape}}}}, {{{{Backspace}}}}, {{{{Delete}}}}.\n\
- Numeric values: 'number five' → 5; 'five hundred' → 500.\n\
- Normalize spoken email addresses: 'jane the number four doe at gmail dot com' → jane4doe@gmail.com.\n\
- Normalize spoken domains: 'gmail dot com' → gmail.com{{{{Enter}}}}; 'amazon dot in' → amazon.in{{{{Enter}}}}. Brand in address bar: 'Netflix' → netflix.com{{{{Enter}}}}.\n\
- 'next line' / 'new line' → {{{{Enter}}}}. 'next paragraph' → {{{{Enter}}}}{{{{Enter}}}}.\n\
- 'press enter' / 'submit' / 'go' / 'search' (alone) → {{{{Enter}}}}.\n\
- 'tab' alone / 'press tab' → {{{{Tab}}}}.\n\
- Shell/terminal commands MUST always end with {{{{Enter}}}}: 'list files' → dir{{{{Enter}}}} or ls{{{{Enter}}}}.\n\
- In a browser address bar, append {{{{Enter}}}} after any complete domain, URL, site name, navigation request, or web search. Skip it only for partial inline edits or when the current field already fully contains the intended destination.\n\
- Do not spell key names as ordinary text when a key press is intended.\n\
- Do not add padding spaces or repeat output already visible in context.\n\
\n\
STEP 4 — PUNCTUATION AND CAPITALISATION (use text_before_cursor / text_after_cursor):\n\
- Cursor mid-document (text_after_cursor non-empty) → no terminal punctuation unless dictated.\n\
- At the beginning of an empty field or right after sentence-ending punctuation, capitalise the first word unless the dictated content is intentionally lowercase.\n\
- If text_before_cursor already ends with a space, do not add another leading space.\n\
- If text_before_cursor ends in a word character and the dictated output starts a new word, insert one leading space.\n\
- If text_before_cursor ends mid-word or with connector punctuation like @ . / - _, continue inline with no extra space.\n\
- Preserve surrounding punctuation style and avoid duplicate spaces or duplicate punctuation.\n\
- Match the punctuation style of surrounding text.\n\
\n\
{image_instruction}\n\
If image context is required and no image is attached, output {{{{NEEDS_IMAGE}}}}.\n\
{common_terms_section}{recent_context_section}{extra}{context_text}\n\
Transcript: {transcript}",
        mode_prefix = mode_prefix,
        mode_rules = mode_rules,
        spoken_language_section = spoken_language_section,
        common_terms_section = common_terms_section,
        recent_context_section = recent_context_section,
        extra = extra,
        image_instruction = image_context_instruction(image_attached),
        context_text = context_text,
        transcript = transcript,
    )
}

fn image_context_instruction(image_attached: bool) -> &'static str {
    if image_attached {
        "If an attached image is present, use it as visual context for what the user wants, especially the UI state near the red cursor marker. Do not describe the image; use it only to decide the correct text or shortcut."
    } else {
        "No image is attached in this pass; rely on text context unless visual context is required."
    }
}

fn format_recent_context(recent_context: &[RecentTextContext]) -> String {
    if recent_context.is_empty() {
        return String::new();
    }
    let items = recent_context
        .iter()
        .map(|item| {
            format!(
                "- {} from {}s ago | transcript: {} | output: {}",
                item.stage,
                item.age_seconds,
                clip_prompt_text(&item.transcript.replace('\n', " "), 140),
                clip_prompt_text(&item.output.replace('\n', " "), 140)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Prior recent text interactions:\n{}\nUse these only for continuity. Do not repeat stale text unless the new transcript clearly asks for it.\n\n",
        items
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FocusedTextContext;

    #[test]
    fn transcription_prompt_mentions_spoken_languages() {
        let prompt = transcription_prompt(None, "English, Hindi");
        assert!(prompt.contains("English, Hindi"));
        assert!(prompt.contains("do not translate"));
    }

    #[test]
    fn interpretation_prompt_includes_recent_context() {
        let prompt = interpretation_prompt(
            "rewrite this",
            None,
            false,
            "",
            "English",
            &[RecentTextContext {
                stage: "interpretation".to_string(),
                transcript: "rewrite this paragraph".to_string(),
                output: "A polished paragraph".to_string(),
                age_seconds: 8,
            }],
        );
        assert!(prompt.contains("Prior recent text interactions"));
        assert!(prompt.contains("rewrite this paragraph"));
    }

    #[test]
    fn interpretation_prompt_documents_prompt_and_agentic_handoffs() {
        let prompt = interpretation_prompt(
            "what is the capital of the US",
            None,
            false,
            "",
            "English",
            &[],
        );
        assert!(prompt.contains("{{Prompt}}"));
        assert!(prompt.contains("{{agentic}}"));
        assert!(prompt.contains("what is the capital of the US"));
    }

    #[test]
    fn interpretation_prompt_emphasises_multi_sentence_handoff() {
        let prompt = interpretation_prompt(
            "anything",
            None,
            false,
            "",
            "English",
            &[],
        );
        // The rule must mention multi-sentence as the bar for {{Prompt}}.
        assert!(prompt.contains("spans multiple sentences"));
        // Single-sentence rewrites must NOT route.
        assert!(prompt.contains("single-sentence rewrites of a single-sentence selection"));
        // Negative-rewrite few-shot: a short selection rewritten inline, not handed off.
        assert!(prompt.contains("Output: I look forward to seeing you soon."));
    }

    #[test]
    fn interpretation_prompt_flags_handoff_tools_as_exceptional() {
        let prompt = interpretation_prompt(
            "anything",
            None,
            false,
            "",
            "English",
            &[],
        );
        // Default behaviour preamble: just type.
        assert!(
            prompt.contains("rare, exceptional cases"),
            "missing 'rare, exceptional cases' framing: {}",
            prompt
        );
        // Tightened {{Prompt}} rule names required verbs.
        assert!(prompt.contains("explicit verbs"));
        assert!(prompt.contains("'rewrite'"));
        assert!(prompt.contains("'summarize'"));
        // Negative few-shots: questions should pass through as text, not {{Prompt}}.
        assert!(prompt.contains("Output: what is the capital of the US"));
        assert!(prompt.contains("Output: Why is it still taking so long to get the diagnostics?"));
    }

    #[test]
    fn interpretation_prompt_treats_questions_as_typed_text_regardless_of_app() {
        // Same explicit guidance must appear regardless of focused app. No app-list
        // hard-coding: the prompt itself tells the model not to route ordinary
        // questions or chat-style sentences anywhere.
        for ctx in [
            None,
            Some(WindowContext {
                app_name: "claude.exe".to_string(),
                title: "Claude".to_string(),
                cursor_x: 0,
                cursor_y: 0,
                focused_text: None,
                cursor_screenshot: None,
            }),
            Some(WindowContext {
                app_name: "msedge.exe".to_string(),
                title: "ChatGPT - New chat".to_string(),
                cursor_x: 0,
                cursor_y: 0,
                focused_text: None,
                cursor_screenshot: None,
            }),
            Some(WindowContext {
                app_name: "notepad.exe".to_string(),
                title: "Untitled - Notepad".to_string(),
                cursor_x: 0,
                cursor_y: 0,
                focused_text: None,
                cursor_screenshot: None,
            }),
        ] {
            let prompt = interpretation_prompt(
                "anything",
                ctx.as_ref(),
                false,
                "",
                "English",
                &[],
            );
            assert!(
                prompt.contains("Questions, requests, instructions, and chat-style sentences are NOT a reason to use a handoff tool"),
                "generic 'questions are not handoffs' rule missing: {}",
                prompt
            );
            assert!(
                prompt.contains("regardless of which app is focused"),
                "app-agnostic default-type rule missing: {}",
                prompt
            );
        }
    }

    #[test]
    fn prompt_handoff_prompt_requires_delivery_envelope() {
        let prompt = prompt_handoff_prompt("write a reply", None, &[]);
        assert!(prompt.contains("\"delivery\":\"ui\"|\"keyboard\""));
        assert!(prompt.contains("\"text\":\"...\""));
        assert!(prompt.contains("write a reply"));
    }

    #[test]
    fn thinking_prompt_contains_handoff_mode() {
        let prompt =
            thinking_interpretation_prompt("fix the grammar", None, false, "", "English", &[]);
        assert!(prompt.contains("rewrite handoff"));
        assert!(prompt.contains("Final output:"));
        assert!(prompt.contains("fix the grammar"));
    }

    #[test]
    fn address_bar_prompt_requires_enter_and_direct_links() {
        let context = WindowContext {
            title: "New Tab - Chrome".to_string(),
            app_name: "chrome.exe".to_string(),
            cursor_x: 0,
            cursor_y: 0,
            focused_text: Some(FocusedTextContext {
                source: "test".to_string(),
                element_name: Some("Address and search bar".to_string()),
                control_type: Some("Edit".to_string()),
                class_name: Some("OmniboxViewViews".to_string()),
                automation_id: Some("urlInput".to_string()),
                parent_name: Some("Address bar".to_string()),
                parent_class: None,
                parent_control_type: None,
                text_before_cursor: Some(String::new()),
                selected_text: None,
                text_after_cursor: None,
                full_text: None,
                truncated: false,
                cursor_known: true,
                element_bounds: None,
            }),
            cursor_screenshot: None,
        };
        let prompt =
            interpretation_prompt("youtube music", Some(&context), false, "", "English", &[]);
        assert!(prompt.contains("BROWSER ADDRESS BAR ACTIVE"));
        assert!(prompt.contains("followed by {{Enter}}"));
        assert!(prompt.contains("https://www.youtube.com/results?search_query=X{{Enter}}"));
    }

    #[test]
    fn thinking_prompt_gets_terminal_appendix() {
        let context = WindowContext {
            title: "PowerShell".to_string(),
            app_name: "pwsh.exe".to_string(),
            cursor_x: 0,
            cursor_y: 0,
            focused_text: None,
            cursor_screenshot: None,
        };
        let prompt =
            thinking_interpretation_prompt("list files", Some(&context), false, "", "English", &[]);
        assert!(prompt.contains("POWERSHELL PROMPT ACTIVE"));
        assert!(prompt.contains("Always append {{Enter}}"));
    }

    #[test]
    fn common_terms_are_preferred_spellings() {
        let prompt = interpretation_prompt("write amit", None, false, "Amith", "English", &[]);
        assert!(prompt.contains("Common terms / preferred spellings"));
        assert!(prompt.contains("high-priority hints"));
        assert!(prompt.contains("Amith"));
    }

    #[test]
    fn image_prompt_has_separate_visual_instruction() {
        let without_image = interpretation_prompt("what is here", None, false, "", "English", &[]);
        let with_image = interpretation_prompt("what is here", None, true, "", "English", &[]);
        assert!(without_image.contains("No image is attached in this pass"));
        assert!(with_image.contains("use it as visual context"));
    }

    #[test]
    fn managed_llama_launch_does_not_force_image_ubatch() {
        let mut settings = Settings::default();
        settings.image_tokens = 70;
        assert_eq!(settings::valid_image_tokens(settings.image_tokens), 70);
        settings.image_tokens = 560;
        assert_eq!(settings::valid_image_tokens(settings.image_tokens), 560);
        settings.image_tokens = 1120;
        assert_eq!(settings::valid_image_tokens(settings.image_tokens), 1120);
    }
}

fn response_needs_image(content: &str) -> bool {
    if content.contains("{{NEEDS_IMAGE}}") || content.contains("{NEEDS_IMAGE}") {
        return true;
    }
    serde_json::from_str::<serde_json::Value>(content.trim())
        .ok()
        .and_then(|value| value["needs_image"].as_bool())
        .unwrap_or(false)
}

fn update_ttft(piece: &str, saw_visible: &mut bool, ttft_ms: &mut Option<f64>, started: Instant) {
    if *saw_visible || ttft_ms.is_some() {
        return;
    }

    for ch in piece.chars() {
        if ch.is_whitespace() {
            continue;
        }
        *saw_visible = true;
        if ch == '{' {
            *ttft_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
        }
        break;
    }
}

fn clip_prompt_text(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        text.trim().to_string()
    } else {
        format!(
            "{}...",
            text.chars().take(max_chars).collect::<String>().trim_end()
        )
    }
}

fn select_llama_device(settings: &Settings) -> Option<String> {
    let devices = model_setup::detect_llama_devices(&settings.llama_server_path).ok()?;
    if devices.is_empty() {
        return None;
    }
    let configured = settings.llama_device.trim();
    if !configured.is_empty()
        && devices
            .iter()
            .any(|device| device.id.eq_ignore_ascii_case(configured))
    {
        return Some(configured.to_string());
    }
    model_setup::preferred_gpus(&devices)
        .first()
        .map(|device| device.id.clone())
}

fn server_port(server_url: &str) -> String {
    server_url
        .trim()
        .trim_end_matches('/')
        .rsplit(':')
        .next()
        .filter(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or("8099")
        .to_string()
}

fn server_log_path() -> PathBuf {
    settings::config_dir().join("logs").join("llama-server.log")
}

fn open_server_log() -> anyhow::Result<std::fs::File> {
    let path = server_log_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("failed to open {}", path.display()))
}

pub(crate) fn recent_server_log_tail() -> Option<String> {
    let text = std::fs::read_to_string(server_log_path()).ok()?;
    let tail = text
        .lines()
        .rev()
        .take(12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    if tail.trim().is_empty() {
        None
    } else {
        Some(tail)
    }
}

#[cfg(windows)]
fn hide_child_window(command: &mut tokio::process::Command) {
    command.creation_flags(0x08000000);
}

#[cfg(not(windows))]
fn hide_child_window(_command: &mut tokio::process::Command) {}

fn tail_chars(text: &str, max_chars: usize) -> &str {
    let total = text.chars().count();
    if total <= max_chars {
        text
    } else {
        let skip = total - max_chars;
        let start = text
            .char_indices()
            .nth(skip)
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        &text[start..]
    }
}

fn head_chars(text: &str, max_chars: usize) -> &str {
    if text.chars().count() <= max_chars {
        text
    } else {
        let end = text
            .char_indices()
            .nth(max_chars)
            .map(|(idx, _)| idx)
            .unwrap_or(text.len());
        &text[..end]
    }
}

fn format_window_context(context: Option<&WindowContext>, image_attached: bool) -> String {
    context
        .map(|c| {
            let focused = c
                .focused_text
                .as_ref()
                .map(|text| {
                    let mut parts = vec![format!(
                        "\nFocused element context from {}{}{}{}{}{}{}{}:",
                        text.source,
                        text.element_name
                            .as_ref()
                            .map(|name| format!("\nElement name: {name}"))
                            .unwrap_or_default(),
                        text.control_type
                            .as_ref()
                            .map(|kind| format!("\nControl type: {kind}"))
                            .unwrap_or_default(),
                        text.class_name
                            .as_ref()
                            .map(|c| format!("\nClass name: {c}"))
                            .unwrap_or_default(),
                        text.automation_id
                            .as_ref()
                            .map(|a| format!("\nAutomationId: {a}"))
                            .unwrap_or_default(),
                        text.parent_name
                            .as_ref()
                            .map(|p| format!("\nParent element name: {p}"))
                            .unwrap_or_default(),
                        text.parent_control_type
                            .as_ref()
                            .map(|p| format!("\nParent control type: {p}"))
                            .unwrap_or_default(),
                        text.element_bounds
                            .map(|b| format!(
                                "\nElement bounds: left={}, top={}, right={}, bottom={}",
                                b[0], b[1], b[2], b[3]
                            ))
                            .unwrap_or_default()
                    )];
                    if text.cursor_known {
                        parts.push("UIA caret/insertion position was available.".to_string());
                    } else {
                        parts.push("UIA caret position was not available; use the provided surrounding/full field text and mouse cursor coordinates.".to_string());
                    }

                    // Build a single visual line that shows EXACTLY what is already in the field
                    // and where the cursor / selection sits. This is the most important context
                    // for the model — the model must NOT re-emit any of this text; it is already
                    // typed.
                    let before = text.text_before_cursor.as_deref().unwrap_or("");
                    let selected = text.selected_text.as_deref().unwrap_or("");
                    let after = text.text_after_cursor.as_deref().unwrap_or("");
                    let local_before = if selected.is_empty() {
                        before
                    } else {
                        tail_chars(before, 120)
                    };
                    let local_after = if selected.is_empty() {
                        after
                    } else {
                        head_chars(after, 120)
                    };
                    let has_any = !before.is_empty() || !selected.is_empty() || !after.is_empty();
                    if has_any {
                        let visual = if !selected.is_empty() {
                            format!(
                                "{local_before}<<<SELECTED:{selected}>>>{local_after}"
                            )
                        } else {
                            format!("{before}<<<CURSOR>>>{after}")
                        };
                        parts.push(format!(
                            "Current field state (the text below is ALREADY typed in the field; do NOT re-type it):\n{visual}"
                        ));
                        if !selected.is_empty() {
                            parts.push(format!(
                                "Currently selected text (highlighted, will be replaced if you output text): {selected}"
                            ));
                        }
                        if !local_before.is_empty() {
                            parts.push(format!("Already-typed text before cursor: {local_before}"));
                        }
                        if !local_after.is_empty() {
                            parts.push(format!("Already-typed text after cursor: {local_after}"));
                        }
                    } else {
                        parts.push("Current field state: (empty — no text typed yet)".to_string());
                    }
                    if selected.is_empty() {
                        if let Some(full) = text.full_text.as_ref().filter(|s| !s.is_empty()) {
                            parts.push(format!(
                                "Focused field full text{}:\n{}",
                                if text.truncated { " (truncated)" } else { "" },
                                clip_prompt_text(full, 500)
                            ));
                        }
                    } else {
                        parts.push(
                            "Selection-focused context: selected text is the primary replacement target. Use only the nearby before/after snippets shown above for local grammar and spacing."
                                .to_string(),
                        );
                    }
                    parts.join("\n")
                })
                .unwrap_or_else(|| "\nNo UI Automation text context was captured.".to_string());
            let screenshot = c
                .cursor_screenshot
                .as_ref()
                .map(|image| {
                    if image_attached {
                        format!(
                        "\nAttached image: {}x{} screenshot from the target app near the cursor. The red marker shows the cursor/insertion area.",
                        image.width, image.height
                        )
                    } else {
                        "\nA cursor screenshot is available for a follow-up image request, but it is not attached in this pass.".to_string()
                    }
                })
                .unwrap_or_else(|| "\nNo cursor screenshot was captured.".to_string());
            format!(
                "Foreground window title: {}\nApplication: {}\nMouse cursor position: x={}, y={}{}{}",
                c.title, c.app_name, c.cursor_x, c.cursor_y, focused, screenshot
            )
        })
        .unwrap_or_else(|| "No foreground window context available.".to_string())
}

#[derive(Default)]
struct StreamingTextParser {
    raw: String,
    saw_text_type: bool,
    value_start: Option<usize>,
    scan_index: usize,
    escape: bool,
    complete: bool,
}

#[derive(Default)]
struct PlainStreamingTextParser {
    pending: String,
    in_token: bool,
}

impl PlainStreamingTextParser {
    fn push(&mut self, piece: &str) -> Vec<String> {
        self.pending.push_str(piece);
        let mut emitted = String::new();
        let mut index = 0;
        while index < self.pending.len() {
            let rest = &self.pending[index..];
            if self.in_token {
                if let Some(end) = rest.find("}}") {
                    index += end + 2;
                    self.in_token = false;
                } else {
                    self.pending = rest.to_string();
                    return emit_if_nonempty(emitted);
                }
            } else if rest.starts_with("{{") {
                self.in_token = true;
                index += 2;
            } else if let Some(next_token) = rest.find("{{") {
                emitted.push_str(&rest[..next_token]);
                index += next_token;
            } else if rest.ends_with('{') {
                emitted.push_str(&rest[..rest.len() - 1]);
                self.pending = "{".to_string();
                return emit_if_nonempty(emitted);
            } else {
                emitted.push_str(rest);
                index = self.pending.len();
            }
        }
        self.pending.clear();
        emit_if_nonempty(emitted)
    }
}

fn emit_if_nonempty(text: String) -> Vec<String> {
    if text.is_empty() {
        Vec::new()
    } else {
        vec![text]
    }
}

impl StreamingTextParser {
    fn push(&mut self, piece: &str) -> Vec<String> {
        self.raw.push_str(piece);
        if self.complete {
            return Vec::new();
        }
        if !self.saw_text_type {
            self.saw_text_type = self.raw.contains("\"type\"") && self.raw.contains("\"text\"")
                || self.raw.contains("\"text\"");
        }
        if self.saw_text_type && self.value_start.is_none() {
            if let Some(start) = find_json_string_value_start(&self.raw, "\"value\"")
                .or_else(|| find_json_string_value_start(&self.raw, "\"text\""))
            {
                self.value_start = Some(start);
                self.scan_index = start;
            }
        }

        let Some(_) = self.value_start else {
            return Vec::new();
        };

        let mut emitted = String::new();
        while self.scan_index < self.raw.len() {
            let slice = &self.raw[self.scan_index..];
            let Some(ch) = slice.chars().next() else {
                break;
            };
            let len = ch.len_utf8();

            if self.escape {
                match ch {
                    '"' => emitted.push('"'),
                    '\\' => emitted.push('\\'),
                    '/' => emitted.push('/'),
                    'b' => emitted.push('\u{0008}'),
                    'f' => emitted.push('\u{000c}'),
                    'n' => emitted.push('\n'),
                    'r' => emitted.push('\r'),
                    't' => emitted.push('\t'),
                    'u' => {
                        if self.scan_index + len + 4 > self.raw.len() {
                            break;
                        }
                        let hex_start = self.scan_index + len;
                        let hex_end = hex_start + 4;
                        if let Ok(code) = u16::from_str_radix(&self.raw[hex_start..hex_end], 16) {
                            if let Some(decoded) = char::from_u32(code as u32) {
                                emitted.push(decoded);
                            }
                        }
                        self.scan_index = hex_end;
                        self.escape = false;
                        continue;
                    }
                    other => emitted.push(other),
                }
                self.escape = false;
            } else if ch == '\\' {
                self.escape = true;
            } else if ch == '"' {
                self.complete = true;
                self.scan_index += len;
                break;
            } else {
                emitted.push(ch);
            }

            self.scan_index += len;
        }

        if emitted.is_empty() {
            Vec::new()
        } else {
            vec![emitted]
        }
    }
}

fn find_json_string_value_start(raw: &str, field: &str) -> Option<usize> {
    let field_index = raw.find(field)?;
    let after_field = &raw[field_index + field.len()..];
    let colon_offset = after_field.find(':')?;
    let after_colon_index = field_index + field.len() + colon_offset + 1;
    let after_colon = &raw[after_colon_index..];
    let quote_offset = after_colon.find('"')?;
    Some(after_colon_index + quote_offset + 1)
}

fn nonempty<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value.trim()
    }
}

fn extract_transcript(content: &str) -> anyhow::Result<String> {
    let value = serde_json::from_str::<serde_json::Value>(content.trim())
        .with_context(|| format!("transcription was not valid JSON: {}", content.trim()))?;
    value["transcript"]
        .as_str()
        .map(|text| text.trim().to_string())
        .ok_or_else(|| anyhow!("transcription JSON did not contain a string transcript"))
}

fn silent_wav_16k_base64(duration_ms: u32) -> String {
    let samples = ((16_000_u32 * duration_ms) / 1000).max(1);
    let data_bytes = samples * 2;
    let mut wav = Vec::with_capacity(44 + data_bytes as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&16_000_u32.to_le_bytes());
    wav.extend_from_slice(&32_000_u32.to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_bytes.to_le_bytes());
    wav.resize(44 + data_bytes as usize, 0);
    general_purpose::STANDARD.encode(wav)
}

#[cfg(windows)]
fn kill_server_on_configured_port(server_url: &str) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    let Some(port) = server_url
        .trim_end_matches('/')
        .rsplit(':')
        .next()
        .and_then(|part| part.parse::<u16>().ok())
    else {
        return;
    };

    let script = format!(
        "$pids = Get-NetTCPConnection -LocalPort {port} -State Listen -ErrorAction SilentlyContinue | Select-Object -ExpandProperty OwningProcess -Unique; foreach ($pid in $pids) {{ Stop-Process -Id $pid -Force -ErrorAction SilentlyContinue }}"
    );
    let _ = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

#[cfg(not(windows))]
fn kill_server_on_configured_port(_server_url: &str) {}
