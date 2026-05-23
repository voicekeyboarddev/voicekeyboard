# Voice Keyboard Regression Checklist

Use this checklist for agent or human QA when changing context capture, prompts,
shortcut parsing, llama.cpp launch settings, or diagnostics UI. Add new cases when
an app-specific failure is found.

## Ctrl+C Probing And Shortcuts

- Paint or another canvas app focused: hold the trigger over a selected canvas
  region; hidden clipboard probing is skipped and the app receives no synthetic
  Ctrl+C.
- cmd.exe, PowerShell, and Windows Terminal focused: hidden clipboard probing is
  skipped.
- Browser, Notepad, VS Code, or Office text selection active: if UI Automation
  misses selected text, hidden clipboard probing may capture it and restore the
  original clipboard.
- Unknown non-text app focused with no readable selection: saying "copy" or
  "control C" requires confirmation instead of sending Ctrl+C immediately.
- Text-capable app focused: saying "copy" or "control C" sends `{{Ctrl+C}}` or
  asks for confirmation only when another safety rule requires it.

## Text Context And Rewrites

- Browser address/search bar: navigation and searches append `{{Enter}}`; partial
  inline edits do not duplicate existing text.
- Search box outside an address bar: output is the search query plus `{{Enter}}`,
  not a constructed URL.
- Selected text rewrite: "fix the grammar", "shorten this", and "translate this"
  replace only the selected text and preserve nearby spacing.
- Long selected text: thinking handoff is used when configured and output remains
  pasteable text only.
- Direct assistant request, e.g. "what is the capital of the US": local
  interpretation returns `{{Prompt}}`; Prompt handoff streams in the bottom
  overlay and ends as a copyable UI result unless the second-stage response asks
  for keyboard delivery.
- Multi-step request, e.g. coding mode or save to notes: local interpretation
  returns `{{agentic}}`; the app shows the placeholder and performs no external
  action.
- Common terms: names, emails, company names, and product names from Settings are
  preferred for ambiguous ASR spellings.

## Image Context

- No text context but screenshot available: first text-only interpretation can
  request `{{NEEDS_IMAGE}}`, then retry with image attached.
- Image-attached prompt: model input says to use the image as visual context near
  the red cursor marker, not to describe the image.
- Image token setting changes restart the managed backend and preserve the chosen
  token budget.
- Default image token budget is 140 and the managed llama.cpp launch does not
  force `--ubatch-size`.
- Selecting high image token budgets such as 560 or 1120 shows a warning that a
  custom/external llama.cpp launch may need a safe `--ubatch-size`, for example
  1024.
- Selecting large image dimensions above the practical low-res range shows a
  warning about TTFT and image-processing limit risk.

## Runtime Reset And Errors

- Save Settings with a non-model change: inline Settings feedback reports saved
  without unnecessary backend restart.
- After a successful injection: Wrong Output is available from the overlay and
  latest Diagnostics row; saving writes a negative example with optional expected
  behavior, model input/output, context, actions, and audio.
- Save Settings with model/runtime/image-token changes: backend restarts or warms,
  and failures are logged.
- Reset llama.cpp backend: app-managed server is killed, port is cleared, model
  warms again, and status returns to ready.
- External server mode: reset reports that it is unavailable for non-managed
  servers.
- Transparent overlay: errors are short and direct users to Diagnostics.
- Main Diagnostics tab: full error messages are visible, individually expandable,
  and selectable/copyable.

## Baseline Commands

- `cargo test` from `src-tauri`
- `npm run build` from the repo root
