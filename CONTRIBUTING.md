# Contributing to Voice Keyboard

Thanks for your interest in helping out! This project is in early-alpha, so
fixes, ideas, and feedback are all genuinely useful.

## Code of Conduct

Participation is governed by the [Code of Conduct](CODE_OF_CONDUCT.md).
Be kind, give people the benefit of the doubt, and please flag anything that
crosses the line by opening a private security advisory or emailing the
maintainers.

## Ways to contribute

- **Report bugs** with the issue template — minimal repro steps make a huge
  difference.
- **Suggest new shortcut tokens or prompt examples** — open an issue with the
  speech transcript, the field context, and the output you expected vs. got.
- **Improve prompt quality** — `src-tauri/src/model.rs` contains the prompt
  text. Small wording changes can have big effects; PRs that include before /
  after test transcripts are easiest to review.
- **Port to Linux / macOS** — currently Windows-only because of UI Automation
  (`context.rs`) and `SendInput` (`injection.rs`). A platform-trait refactor
  would be a great first big PR.
- **Better diagnostics & dataset tooling** in the frontend (`src/main.ts`).

## Development setup

Prerequisites:

- Node.js LTS
- Rust stable (`rustup install stable`)
- Visual Studio Build Tools with **Desktop development with C++**
- A Vulkan-capable GPU is recommended for end-to-end testing.

```powershell
git clone https://github.com/voicekeyboarddev/voicekeyboard.git
cd voicekeyboard
npm install
npm run tauri:dev
```

The dev build hot-reloads frontend changes; Rust changes trigger a recompile.

## Where things live

```
src/                        TypeScript frontend (Vite + Tauri IPC)
  main.ts                     UI, Diagnostics, settings, dataset capture
  styles.css                  All styling

src-tauri/                  Rust backend (Tauri shell)
  src/
    lib.rs                    app state, gesture pipeline, Tauri commands
    gesture.rs                global mouse-hold trigger (rdev)
    audio.rs                  cpal capture, rolling history, pre-roll
    context.rs                active window + UI Automation focused text
    model.rs                  llama-server lifecycle, prompts, streaming
    model_setup.rs            model discovery / first-run flow
    parser.rs                 plain-text + {{shortcut}} token parser
    injection.rs              SendInput text & shortcut delivery
    safety.rs                 dry-run, large-text confirm, kill switch
    settings.rs               settings struct + on-disk persistence
    clipboard.rs              clipboard read/write helpers
    logging.rs / metrics.rs   structured logs, perf counters
    types.rs                  shared serde types

scripts/                    PowerShell helpers (installer build, model dl)
docs/                       Long-form docs + demo GIFs
```

## Verify your change

```powershell
npm run build                      # tsc + vite build
cd src-tauri; cargo check          # type-check the Rust side
cd src-tauri; cargo test           # unit tests, esp. parser.rs
cd ..; npm run tauri:build         # produce a full installer
```

The parser has the densest unit tests — if you touch shortcut grammar, please
add a case there.

## Pull request guidelines

- One logical change per PR. Refactors and feature changes in separate PRs.
- Run `cargo check` and the parser tests before pushing.
- Update the README / docs if you change user-visible behaviour.
- For prompt changes, include 2–3 transcript → output examples in the PR body
  showing the change is an improvement.
- By submitting a PR you agree your contribution is licensed under the
  project's [PolyForm Noncommercial License 1.0.0](LICENSE).

## Reporting security issues

Please **do not** open a public issue for anything you think might be a
security problem (e.g. a way to bypass `kill_switch_enabled`, an injection
issue with shortcut parsing, or a way to make the local server misbehave).
Instead, use GitHub's "Report a vulnerability" flow on the Security tab.

## Questions

Open a Discussion or an issue with the `question` label. Don't worry about
the question being too basic — getting the bar to entry low is part of the
goal.
