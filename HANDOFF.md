# Voice Keyboard Handoff

## Project
Windows Tauri/Rust app in `D:\AppsMade2\CppGemma` named Voice Keyboard. It runs a managed `llama-server.exe` on `http://127.0.0.1:8099`, records audio from mouse-hold gestures, transcribes with Gemma audio, interprets into typed text plus shortcut tokens, and injects into the active app.

## Current User Priorities
- Low-latency warm transcription with Gemma/llama.cpp.
- Reliable gesture behavior around text selection.
- Avoid JSON for interpretation; use plain text with embedded shortcut tokens like `{{Enter}}`, `{{Ctrl+C}}`.
- Diagnostics must show exact model input: prompt, context, image/screenshot if present, audio path/duration, output.
- Save dataset examples explicitly from Diagnostics, not via hidden keyboard shortcuts.

## Current Behavior Implemented
- Left-click hold remains the primary activation.
- Right-click hold trigger exists but is now disabled by default and in current persisted settings.
- Audio segment starts at button-down. Processing happens on release only if the pointer was stationary for the final `250 ms` before release.
- Physical button watcher starts immediately at button-down, so missed rdev releases during selection/drag should still be noticed.
- If selected text is captured by UIA and Windows de-highlights it during hold, app can replace original selection through UIA `ValuePattern` before injecting remaining shortcuts.
- Interpretation prompt asks for plain text only, no JSON. Key presses should be embedded as `{{Enter}}`, `{{Tab}}`, `{{Ctrl+C}}`, etc.
- Parser still supports JSON as a fallback, but normal interpretation should not request JSON.
- Diagnostics includes Exact Model Inputs with endpoint, prompt, context JSON, audio path/duration/format, and screenshot preview when present.
- Dataset feedback is now via Diagnostics buttons: Save correct example / Save wrong example. It writes JSONL and copied WAVs under app config `dataset` folder.

## Important Files
- `src-tauri/src/lib.rs`: app state, gesture handling, processing pipeline, feedback saving, commands.
- `src-tauri/src/model.rs`: llama server management, transcription request, plain-text interpretation prompt, streaming parsing.
- `src-tauri/src/parser.rs`: parses plain text and shortcut tokens, also legacy JSON fallback.
- `src-tauri/src/context.rs`: active window, UI Automation focused text, screenshots, preserved selection replacement.
- `src-tauri/src/injection.rs`: SendInput text/shortcut injection, context-menu cleanup.
- `src-tauri/src/settings.rs`: defaults. Right-click trigger default is now `false`.
- `src/main.ts`: Tauri frontend, Diagnostics, model input display, dataset buttons.
- `src/styles.css`: app and overlay styling.

## Recent Notable Changes
- Removed hidden Ctrl/Alt feedback shortcut handling from `gesture.rs`.
- Added explicit `save_feedback_example(correct: bool)` Tauri command.
- Feedback examples include `model_inputs` snapshots along with audio/transcript/output/actions/context.
- Current persisted settings file was updated to include `right_click_trigger_enabled: false`.
- Prompt examples added for spoken emails/domains/numbers:
  - `Ashish the number four reading at gmail dot com` -> `ashish4reading@gmail.com`
  - `aashish at s n t achievement dot com` -> `aashish@sntachievement.com`
  - `gmail dot com` -> `gmail.com{{Enter}}`

## Known Issues / Watchlist
- Some prior logs show model repetition or stale output in interpretation. Plain-text prompt helps, but verify with fresh runs after latest build.
- TTFT for first transcription after warmup can still spike occasionally; inspect exact model inputs and llama logs if recurring.
- Right-click context-menu suppression is conservative: only sends Escape if foreground/cursor window class looks menu/popup. Since right-click trigger is disabled by default, this is lower priority.
- UIA `ValuePattern` replacement only works in controls exposing writable ValuePattern. Custom editors may fall back to normal injection.
- Exact model input display stores screenshot base64 in context. This is useful but can make UI/dataset entries large.

## Build / Verify Commands
Run from repo root unless noted:
- `npm run build`
- `cd src-tauri; cargo check`
- `cd src-tauri; cargo test parser -- --nocapture`
- `npm run tauri:build`

## Relaunch Pattern
After build:
```powershell
Get-Process | Where-Object { $_.ProcessName -match 'voice-keyboard|VoiceKeyboard' } | Stop-Process -Force
Start-Sleep -Seconds 1
Copy-Item -LiteralPath "D:\AppsMade2\CppGemma\src-tauri\target\release\voice-keyboard.exe" -Destination "D:\AppsMade2\CppGemma\VoiceKeyboard.exe" -Force
Start-Process -FilePath "D:\AppsMade2\CppGemma\VoiceKeyboard.exe" -WorkingDirectory "D:\AppsMade2\CppGemma"
```
Health check:
```powershell
Invoke-WebRequest -UseBasicParsing -Uri http://127.0.0.1:8099/health -TimeoutSec 5
```

## Logs / Settings
- Logs: `%APPDATA%\CppGemma\VoiceKeyboard\config\logs\audit.jsonl`
- Settings: `%APPDATA%\CppGemma\VoiceKeyboard\config\settings.json`
- Dataset: `%APPDATA%\CppGemma\VoiceKeyboard\config\dataset\feedback.jsonl` and `dataset\audio\*.wav`

## Suggested Next Work
1. Run fresh manual tests for:
   - selected text -> hold still -> replace selection;
   - drag selection -> release while moving -> no recording;
   - address bar: `gmail dot com`, `amazon dot in`, `Netflix`;
   - spoken email: `Ashish the number four reading at gmail dot com`.
2. Inspect Diagnostics Exact Model Inputs after each failed run and tune prompt examples.
3. Consider adding a dedicated keyboard hotkey trigger for users who dislike mouse-hold selection interactions.
4. Consider limiting screenshot base64 in dataset or storing image files beside WAVs if dataset size grows.
