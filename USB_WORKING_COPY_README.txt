CppGemma / Voice Keyboard USB working copy

This folder is intentionally source-focused and model-free.

On another Windows device:

1. Install prerequisites if they are not already present:
   - Node.js LTS
   - Rust stable
   - Microsoft Visual Studio Build Tools with the Desktop C++ workload

2. Restore downloadable dependencies:
   npm install

3. Download the recommended model files after installing or before running the app:
   powershell -ExecutionPolicy Bypass -File scripts\download-gemma4-models.ps1

   By default this places GGUF files in:
   %APPDATA%\CppGemma\VoiceKeyboard\config\models

4. Run or build:
   npm run tauri:dev
   npm run tauri:build

What was not copied:
- node_modules
- src-tauri\target
- dist
- local GGUF model files
- benchmark logs and local test drops

The llama.cpp runtime DLL/EXE files are included under src-tauri\resources\runtime
because the app needs them for the managed local server.
