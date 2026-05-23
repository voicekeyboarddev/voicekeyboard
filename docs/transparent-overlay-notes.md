# Transparent Overlay Notes

The small transparent UI is the Tauri window named `overlay`, loaded from
`index.html?overlay`.

Key files:

- Frontend rendering and commands: `src/main.ts`
- Overlay styles: `src/styles.css`
- Overlay window state, size, pointer behavior: `src-tauri/src/lib.rs`

Rules for future changes:

- Keep normal recording/status states click-through. The overlay should not steal focus while the app is recording, transcribing, interpreting, or injecting.
- Only interactive overlay states should disable click-through:
  - `confirm`
  - `prompt-panel`
- Do not make the post-injection `done` status overlay clickable unless it has been proven not to take foreground focus. A clickable always-on-top overlay can steal the target field and make later `SendInput` typing unreliable.
- If a future small-overlay feedback button is required, prefer a separate non-activating control/window or a delayed explicit feedback panel that cannot become the keyboard target before injection completes.
- If an overlay control is intentionally clickable, do not hide the overlay from the global mouse hook before the frontend click can finish. The hook should ignore the click for recording, but the frontend still needs to receive button clicks.
- Be careful with `window.blur` in the overlay. Feedback panels must not auto-collapse on blur because users need to type expected behavior/output.
- If a feedback panel opens from the overlay, focus `.expected-feedback` after render so the user can type immediately.
- If a new overlay control is added, verify both paths:
  - the button click reaches `runOverlayCommand` in `src/main.ts`
  - the matching Tauri command is registered in `tauri::generate_handler!`
- Any change to `overlay_dimensions` must be checked visually. Resizing the overlay can make the transparent UI appear to jump.
- After successful injection, avoid long-lived interactive overlay surfaces unless the user explicitly opened a panel. Interactive overlays can become the active window and make later injection targets unreliable.

Manual checks:

- Dictate into a text editor and confirm the text lands in the editor.
- If a small-overlay feedback entry point is reintroduced, click it and confirm a text area appears.
- Type expected behavior, save, and confirm a negative feedback example is written.
- Dismiss the overlay panel, click back into the target app, and confirm the next injection still lands there for several consecutive dictations.
