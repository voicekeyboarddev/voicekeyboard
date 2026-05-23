# Prompt/Agentic Handoff And Wrong-Output Feedback Requirements

Use this file as the implementation prompt when restarting from an earlier branch.
It summarizes the feature work from the local commits after
`67119b2 Add safe model download and vision diagnostics controls`, including the
later correction that restored reliable injection after a transparent-overlay
focus regression.

Do not treat the current branch as the desired final implementation in every
detail. It contains one reverted experiment: a clickable `Wrong` button directly
inside the small transparent `Done` overlay. That approach must not be repeated
unless it is made non-focus-stealing and proven reliable.

## High-Level Goal

Add a two-level model handoff system and a wrong-output dataset feedback flow to
Voice Keyboard.

The first/local interpreter should stay fast and conservative. When it sees a
request that needs a stronger model, deeper transformation, or multi-step agentic
behavior, it should return a special token instead of trying to type literal
text. The app then handles that token without injecting it into the target app.

Also replace the old dataset save controls with a user-facing "Wrong Output"
flow that saves the model input, model output, audio, context, actions, and
optional expected behavior as a negative feedback example.

## Previous-Day Work To Preserve

This section covers the 2026-05-22 handoffs. These requirements are separate
from the Prompt/Agentic work and must be preserved when restarting from a
previous branch.

### Image Context Defaults And llama.cpp Launch

Preserve the image-context changes:

- Default Gemma image token budget is `140`.
- Portable/default config should also use `image_tokens: 140`.
- Valid image token choices must include at least `70`, `140`, `280`, `560`, and
  `1120`.
- Managed llama.cpp launch must not automatically add `--ubatch-size`.
- High image token budgets, especially `560` and `1120`, must remain selectable.
- Selecting high image token budgets must show a warning that custom/external
  llama.cpp launches may require a safe manual ubatch setting, for example
  `--ubatch-size 1024`.
- Selecting image dimensions above the practical low-res range must show a
  warning that larger resolutions can increase TTFT and may hit image-processing
  limits on some GPU/model combinations.
- Changing image token settings should restart or warm the managed backend
  correctly and preserve the selected token budget.
- Add/keep tests proving managed llama launch does not force image ubatch and
  that valid token budgets remain accepted.

### Image Context Prompt Behavior

Preserve the image retry and prompt behavior:

- If text/context-only interpretation cannot decide and image context is needed,
  the model may output `{{NEEDS_IMAGE}}`.
- When `{{NEEDS_IMAGE}}` is returned and a screenshot is available, retry
  interpretation with the marked cursor screenshot attached.
- Image-attached prompts must instruct the model to use the image as visual
  context near the red cursor marker.
- The model must not describe the image; it should use image context only to
  choose the correct text or shortcut.
- No-image text flows must remain fast and should not always pay the image cost
  unless settings require low-res images.

### Ctrl+C Probing And Copy Safety

Preserve the clipboard-selection probing behavior from 2026-05-22, but keep the
later injection-reliability caution in mind:

- Hidden clipboard probing may be used to recover selected text when UI
  Automation misses selection in browsers, Notepad, VS Code, Office, or similar
  text-capable apps.
- The probe writes a sentinel, sends `Ctrl+C`, reads clipboard contents, and
  restores the original clipboard.
- Clipboard probing must be skipped in terminal contexts:
  - `cmd.exe`
  - PowerShell
  - Windows Terminal
- Clipboard probing must be skipped in canvas/non-text contexts such as Paint.
- Skip reasons should be logged, for example "skipped clipboard selection probe:
  terminal context" or "focused control is not text-capable".
- If the user says "copy", "copy this", or "control C" in an unknown non-text app
  with no readable selection, require confirmation instead of immediately
  sending `Ctrl+C`.
- In text-capable apps, copy commands may send `{{Ctrl+C}}` unless another
  safety rule requires confirmation.
- Future changes must test this carefully because any pre-injection keyboard
  shortcut can affect fragile apps.

### UI Mouse Holds And Recording Overlay

Preserve the small UI/recording fixes:

- Mouse holds inside the Voice Keyboard UI must be ignored for recording so users
  can select/copy Diagnostics text without triggering voice capture.
- The small recording mini overlay must appear while listening/recording.
- Showing the overlay while the mouse is down must not cancel target app
  selection capture.
- Short overlay error text should remain concise and should direct users to
  Diagnostics for full details.

### Shortcut And Navigation Improvements

Preserve the shortcut and prompt improvements:

- Add/keep Enter phrase variants:
  - "enter"
  - "press enter"
  - "hit enter"
  - "shortcut enter"
  - "press the enter key"
  - "return"
  - "submit"
  - "go"
- "on the next line type ..." should prepend `{{Enter}}`.
- "new paragraph ..." should prepend `{{Enter}}{{Enter}}`.
- Browser address/search bar behavior:
  - Complete domains, URLs, site names, navigation requests, and web searches
    should end with `{{Enter}}`.
  - Partial inline edits should not append duplicate text or unnecessary Enter.
  - Search boxes outside address bars should output the search query plus
    `{{Enter}}`, not a constructed URL unless it is actually an address bar.
- Terminal behavior:
  - Natural-language terminal commands should map to shell commands and append
    `{{Enter}}`.
  - Examples: list files, hidden files, go to folder, go up, git status,
    npm run build, cargo check, ipconfig.

### Runtime Reset, Settings, And Diagnostics

Preserve runtime controls and Diagnostics UX:

- Add/keep a Reset llama.cpp backend button.
- Reset should work for app-managed server mode:
  - kill app-managed server
  - clear/release the port
  - restart/warm model
  - return status to ready
  - log failures clearly
- In external server mode, reset should report that it is unavailable rather than
  trying to kill an external process.
- Save Settings with non-model changes should show inline "saved" feedback
  without unnecessary backend restart.
- Save Settings with model/runtime/image-token changes should restart or warm
  backend and log failures.
- Calibrate Mic belongs under the Microphone section in Settings.
- VAD speech detection remains active; users should calibrate mic sensitivity if
  silence/noise passes the threshold.
- Diagnostics must expose full error messages, expandable/copyable logs, request
  logs, model inputs, prompts, audio paths, context, screenshots, raw model
  output, and parsed actions.
- Regression checklist must include the image-token/ubatch warnings, Ctrl+C
  probing, reset behavior, overlay errors, and Diagnostics copyability.

## Handoff Tokens

Add two exact model-callable actions:

- `{{Prompt}}`
- `{{agentic}}`

Parser requirements:

- Exact `{{Prompt}}`, case-insensitive, parses to `Action::Prompt`.
- Exact `{{agentic}}`, case-insensitive, parses to `Action::Agentic`.
- These actions must never be typed literally.
- These actions must survive shortcut filtering even when shortcuts are disabled.
- Existing text and shortcut parsing must remain unchanged.
- `Action::Prompt` and `Action::Agentic` are no-op actions in low-level injection
  if they ever reach that layer.

Interpreter routing requirements:

- Return exactly `{{Prompt}}` for:
  - Direct assistant Q&A, for example "what is the capital of the US?"
  - Long writing requests.
  - Rewrite, summarize, translate, polish, proofread, grammar correction, or deep
    selected-text transformation.
  - Requests that need a stronger second-stage answer but are still a one-shot
    text or UI answer.
- Return exactly `{{agentic}}` for:
  - Coding mode.
  - Computer use.
  - Changing clipboard content as a task.
  - Saving to project folder.
  - Saving to notes.
  - Any multi-step task that should be handed to an agent rather than typed.
- Do not use either handoff token for:
  - Simple dictation.
  - Browser navigation.
  - Address bar or search box text.
  - Direct key commands such as Enter, undo, redo, copy, paste, select all,
    arrows, Tab, Escape, Backspace, Delete.

## Prompt Mode

`{{Prompt}}` must be implemented in v1.

Prompt mode behavior:

- Start a second-stage model call using the original transcript/request.
- Include available context:
  - Current window/app.
  - Focused text context.
  - Selected text and nearby text when available.
  - Recent context if enabled.
  - Original WAV audio when supported.
  - Cursor screenshot/image context when supported.
- Use local thinking mode by default.
- Allow a custom OpenAI-compatible endpoint.
- Require the second-stage model to return exactly one JSON object:

```json
{"delivery":"ui","text":"..."}
```

or:

```json
{"delivery":"keyboard","text":"..."}
```

Prompt response rules:

- `delivery: "ui"` shows the result in the popup and does not type it.
- `delivery: "keyboard"` means the result should be inserted into the target
  field or replace the captured selection.
- The app decides insert vs replace based on captured selection/focus context.
  The model must not include labels like "Insert:" or "Replace:".
- If the custom endpoint rejects audio/image media, retry once with text-only
  context.
- If the JSON envelope is invalid or `text` is empty, show an error in the
  popup and log it.

Provider settings:

- `prompt_provider`: default `local`, accepts `custom` or `openai` for custom
  OpenAI-compatible endpoints.
- `prompt_endpoint_url`: base URL for the custom endpoint. The app should call
  `/v1/chat/completions`.
- `prompt_api_key`: optional bearer token for custom endpoint.
- `prompt_model`: model name for custom endpoint, default `gpt-4.1`.
- `prompt_auto_inject_keyboard`: default true.
- Local Prompt mode should use reasoning/thinking enabled and the configured
  thinking reasoning budget.

Keyboard delivery safety:

- Inject Prompt keyboard delivery only if the original target still appears
  focused.
- If focus changed, do not type into the wrong app. Keep the result in the popup
  and show a clear message such as "Target focus changed, so the result was not
  typed."
- For selected text replacement, use preserved selection context where available.

Prompt popup UI:

- Attach to the existing transparent overlay system.
- Stream Prompt output into the popup as it arrives.
- Include transcript/request and result text.
- Include copy button.
- Include collapse/restore behavior.
- If dismissed/collapsed, provide a small restore/expand handle.
- No follow-up chat is required in v1. Add follow-up conversations to TODO.

## Agentic Mode

`{{agentic}}` is recognized in v1 but not implemented.

Agentic behavior:

- Do not execute clipboard changes.
- Do not run coding tasks.
- Do not write files.
- Do not save to notes.
- Do not use computer control.
- Show a placeholder popup with:
  - Transcript/request.
  - Source model output.
  - Copyable explanatory text that agentic mode is not implemented yet.
- Mark this as a TODO for later implementation.

## Wrong-Output Feedback

Remove or replace old dataset save controls:

- Remove current positive/correct dataset save controls from the main workflow.
- Remove old wrong-save controls that do not collect expected behavior.
- Keep Diagnostics recent recording history for playback/debugging.

Desired wrong-output flow:

- After a successful injection, the user must have a clear way to mark the latest
  result as wrong.
- The main/big UI must show a persistent top feedback bar:
  - If no completed injection exists, show an inactive "Dataset feedback" state.
  - If a latest recording exists, show transcript summary and a `Wrong Output`
    button.
  - Clicking `Wrong Output` opens an expected behavior/output text area inline.
  - Expected behavior is optional.
  - `Save Wrong Output` writes a negative feedback example.
  - Include Cancel.
- The Diagnostics latest recording row must also offer a `Wrong`/save path.
- After saving, show a visible toast/status like "Saved as wrong example".

Small transparent UI requirement:

- The user wants wrong-output feedback available from the small transparent UI.
- Do not implement this by simply making the normal post-injection `Done`
  overlay clickable/focusable. That was tried and caused injection reliability
  regressions because the always-on-top overlay could become the foreground
  surface and steal focus from target fields.
- If implementing a small transparent UI entry point, use a design that does not
  disturb the target field during normal injection. Acceptable designs include:
  - A separate non-activating overlay/control window if it can receive clicks
    without taking foreground focus.
  - A tray/menu or keyboard-accessible feedback command that opens the feedback
    panel only after the user explicitly leaves the target flow.
  - A delayed explicit feedback panel that cannot become the keyboard target
    before injection completes.
- Before accepting any small-overlay feedback design, manually test several
  consecutive dictations into VS Code, browser fields, and at least one app where
  focus is fragile. The target field must not lose focus and injection must land
  reliably.

Feedback data requirements:

- Save to the app dataset folder under the config directory.
- Append JSONL to `dataset/feedback.jsonl`.
- Copy paired WAV audio into `dataset/audio`.
- Extract any embedded screenshots from model inputs/window context into
  `dataset/screenshots` rather than storing large base64 blobs inline.
- Store at least:
  - timestamp
  - label: `negative`
  - audio path
  - transcript
  - raw model output
  - optional expected output/behavior
  - parsed actions
  - model input snapshots
  - audio duration
  - transcription timing
  - interpretation timing
  - window/context

## Overlay And Focus Rules

The small transparent UI is the Tauri window named `overlay`, loaded via
`index.html?overlay`.

Key rules:

- Normal overlay states must be click-through:
  - idle
  - recording
  - processing
  - transcript
  - injecting
  - done
  - blocked/error unless explicitly interactive
- Interactive states may disable click-through:
  - confirm
  - prompt-panel
- The overlay must not steal focus while recording, interpreting, injecting, or
  immediately after injection.
- The overlay dimensions must remain stable for each state. Avoid changes that
  shift the overlay position unexpectedly.
- Do not place large feedback UI inside the compact status overlay if it changes
  dimensions or pushes the overlay around.
- If an overlay button is introduced, verify:
  - The frontend click reaches `runOverlayCommand`.
  - The Tauri command is registered in `tauri::generate_handler!`.
  - The global mouse hook does not hide the overlay before the frontend click
    completes.
  - The overlay does not become the target of later `SendInput` text injection.
- Be careful with overlay `window.blur`. Feedback panels with text fields should
  not auto-collapse before the user can type expected behavior.

## Injection Reliability

Injection must remain the top priority.

Requirements:

- Normal dictation must type into the target field reliably.
- Shortcut injection such as `{{Enter}}`, `{{Ctrl+Z}}`, and `{{Ctrl+V}}` must
  remain unchanged.
- Prompt/wrong-output UI must not make the target app lose focus before
  `SendInput` runs.
- Do not add pre-typing focus changes or active-window switches.
- If using UI Automation preserved-selection replacement, only do so when the
  focused element still matches the captured context.
- If selected text replacement succeeds through UI Automation, only inject any
  remaining non-text shortcut actions.
- If replacement cannot be done safely, fall back to normal action injection.

## Recent Context And Thinking Handoff

Keep the existing thinking-handoff behavior:

- `thinking_handoff_enabled`: default true.
- `thinking_handoff_min_chars`: default 250.
- `thinking_handoff_reasoning_budget`: default 64.
- `thinking_handoff_context_items`: default 3.
- Long selected-text rewrite/translate/summarize tasks should use thinking mode
  or Prompt handoff as appropriate.
- Navigation commands, direct shortcuts, address bar searches, and simple
  dictation must not be routed to thinking/Prompt unnecessarily.
- Recent context should be included only when useful and should not pollute
  selected-text transformations.

## Settings UI

Add settings for Prompt mode:

- Prompt provider, local/custom.
- Prompt endpoint URL.
- Prompt API key.
- Prompt model.
- Prompt keyboard auto-inject toggle.
- Thinking handoff settings remain visible:
  - enabled
  - min chars
  - reasoning budget
  - context items

Settings UX requirements:

- Save Settings should show inline feedback.
- Non-model changes should not unnecessarily restart the backend.
- Model/runtime/image-token changes may restart/warm the backend and must log
  failures.

## Diagnostics UI

Diagnostics should show:

- Recent recordings.
- Transcript.
- Model output.
- Parsed actions, including Prompt/Agentic handoff labels.
- Audio playback.
- Latest-row wrong-output feedback entry.
- Request logs and model input snapshots.
- Full errors in Diagnostics; overlay errors should remain short and direct the
  user to Diagnostics.

## Parser And Prompt Tests

Add or preserve tests for:

- `{{Prompt}}` parses to Prompt action.
- `{{agentic}}` parses to Agentic action.
- Prompt/Agentic actions are not dropped by shortcut filtering.
- Existing plain text parsing still works.
- Existing shortcut parsing still works.
- JSON action parsing still works.
- Navigation text plus Enter still works.
- Interpretation prompt documents Prompt and Agentic handoff routing.
- Prompt handoff prompt requires the JSON delivery envelope.
- Thinking handoff triggers for rewrite/translation with selection.
- Thinking handoff skips navigation commands.

## Manual Test Scenarios

Run these before considering the feature complete:

- "What is the capital of the US?" routes to `{{Prompt}}`, streams in popup, and
  displays a UI result with copy.
- Selected text plus "rewrite this professionally" routes to Prompt or thinking
  handoff and replaces selection only when focus remains safe.
- Prompt keyboard delivery with focus lost shows popup only and does not type
  into the wrong window.
- `{{agentic}}` shows placeholder and performs no action.
- Normal dictation into VS Code works repeatedly.
- Normal dictation into a browser text field works repeatedly.
- Address/search bar navigation still appends Enter when appropriate.
- `{{Enter}}`, undo, paste, and other shortcut commands still inject correctly.
- Wrong Output in the main UI saves a negative feedback example with optional
  expected behavior.
- Wrong Output in Diagnostics latest row saves the same kind of example.
- If a small transparent UI feedback entry point is implemented, verify it does
  not reduce injection reliability and opens a usable expected-behavior text
  field.
- Overlay position does not jump during normal recording/status updates.

## Validation Commands

Run:

```powershell
npm run build
cd src-tauri
cargo check
cargo test
cd ..
npm run tauri:build
```

After building, restart the release executable and test manually:

```powershell
src-tauri\target\release\voice-keyboard.exe
```

## Handoff And Packaging

When implementation is complete:

- Commit all local changes.
- Do not push to GitHub from this machine unless explicitly instructed.
- Rebuild the installer.
- Copy the latest installer to:
  - `I:\VoiceKeyboardGitHubHandoff\latest-installer\Voice Keyboard_0.1.0_x64-setup.exe`
- Copy the clean source handoff to a dated folder under:
  - `I:\VoiceKeyboardGitHubHandoff`
- Update:
  - `I:\VoiceKeyboardGitHubHandoff\LATEST.txt`
  - `I:\VoiceKeyboardGitHubHandoff\<dated-folder>\AGENT_INSTRUCTIONS.md`
- Mention validation run results and known TODOs.

## Known TODOs

- Follow-up conversations inside Prompt mode popup.
- Real Agentic mode for clipboard/code/filesystem/notes/computer-use tasks.
- A safe small-transparent-UI wrong-output entry point that does not steal focus.
- Richer custom provider compatibility and non-OpenAI streaming variants.
- More manual testing across monitor scaling, fragile UI fields, browser fields,
  VS Code, Office apps, terminals, and selected-text workflows.
