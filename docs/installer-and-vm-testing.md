# Voice Keyboard Installer and Clean VM Testing

This guide covers building the Windows installer and testing it in a clean Windows environment with only the installer plus GGUF model files copied in.

## Recommended Test Environment

Use a real Windows desktop VM. The best options are:

- **Windows Sandbox**: fastest disposable test, close to a Docker-like workflow.
- **Hyper-V VM**: best persistent clean-machine test.
- **VMware/VirtualBox VM**: also fine.

Docker Desktop is not a good fit for this app. Windows containers do not provide a normal interactive desktop session, global mouse/keyboard hooks, installer UI testing, microphone access, or a realistic tray/window environment.

## Build Installer With Backend, No Models

This packages:

- Tauri frontend
- `llama-server.exe`
- llama.cpp DLL runtime
- default config

It does **not** package large GGUF model files.

```powershell
powershell -ExecutionPolicy Bypass -File scripts\build-installer.ps1 -CleanResources -NoModels
```

Installer output will be in:

```text
src-tauri\target\release\bundle\
```

Usually you want one of these:

```text
src-tauri\target\release\bundle\nsis\Voice Keyboard_0.1.0_x64-setup.exe
src-tauri\target\release\bundle\msi\Voice Keyboard_0.1.0_x64_en-US.msi
```

The NSIS `.exe` installer is usually easier for manual VM testing.

## Build Installer With Models Bundled

Use this only when you want the installer itself to contain the GGUF files:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\build-installer.ps1 `
  -CleanResources `
  -ModelDirs "models\gemma-4-E4B-it-Q4_K_M","models\gemma-4-E4B-it-Q2"
```

The installer will be much larger.

## Build Installer After Downloading From Hugging Face

Set a Hugging Face token only if the repo requires one:

```powershell
$env:HF_TOKEN = "hf_your_token_here"

powershell -ExecutionPolicy Bypass -File scripts\build-installer.ps1 `
  -CleanResources `
  -HfRepo "ORG_OR_USER/REPO" `
  -HfFiles "model.gguf","mmproj.gguf" `
  -ModelSubdir "my-model"
```

Do not commit tokens or generated model files.

## Clean Test: Installer Plus GGUF Files Only

Goal: verify that the installer contains everything except the model files.

Copy only these into the VM:

- The generated installer from `src-tauri\target\release\bundle\...`
- The main model `.gguf`
- The optional `mmproj` `.gguf`, if using a multimodal/audio model

Do not copy:

- Source repo
- `node_modules`
- `src-tauri\target`
- Existing portable export
- Local config from `%APPDATA%` or `%LOCALAPPDATA%`

After installing in the VM:

1. Launch **Voice Keyboard**.
2. Open **Diagnostics**.
3. Click **Open models folder**.
4. Copy the GGUF files into that folder.
5. Restart the app.
6. Click **Test model response**.

The app will auto-detect GGUF files in the user models folder. It chooses a non-`mmproj` GGUF as the main model and a file containing `mmproj` as the projector file.

## Windows Sandbox Test

Windows Sandbox is disposable. When you close it, everything inside disappears.

Create a local folder like:

```text
D:\VoiceKeyboardSandboxDrop
```

Put this inside it:

```text
Voice Keyboard_0.1.0_x64-setup.exe
gemma-4-E4B-it.Q4_K_M.gguf
gemma-4-E4B-it.mmproj-Q8_0.gguf
```

Create `VoiceKeyboardSandbox.wsb` somewhere on the host:

```xml
<Configuration>
  <MappedFolders>
    <MappedFolder>
      <HostFolder>D:\VoiceKeyboardSandboxDrop</HostFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
  </MappedFolders>
  <Networking>Enable</Networking>
  <ClipboardRedirection>Enable</ClipboardRedirection>
  <MemoryInMB>16384</MemoryInMB>
</Configuration>
```

Double-click the `.wsb` file. Inside Sandbox, open the mapped desktop folder, run the installer, then copy the GGUF files into the app's models folder using **Diagnostics > Open models folder**.

## Hyper-V Test

Use Hyper-V if you want a repeatable VM with snapshots.

Recommended baseline:

- Windows 11 VM
- 16 GB RAM minimum, more for large models
- Enhanced Session enabled if testing microphone behavior
- A checkpoint before installing Voice Keyboard

Test flow:

1. Revert to clean checkpoint.
2. Copy installer plus GGUF files into the VM.
3. Install Voice Keyboard.
4. Launch the app.
5. Put GGUF files into **Diagnostics > Open models folder**.
6. Restart app.
7. Run **Test model response**.
8. Test actual voice trigger if microphone passthrough is enabled.

## What To Verify

Confirm these on the clean machine:

- App launches without the source repo present.
- `llama-server.exe` starts from installed resources.
- No path points back to the development checkout.
- GGUF files copied after install are detected.
- `Test model response` succeeds.
- Audio test succeeds.
- Global hook starts and stops.
- App can be uninstalled normally.

## Troubleshooting

If the model test fails:

- Check that the main `.gguf` file is present in the models folder.
- Check that the projector file includes `mmproj` in its filename if one is required.
- Restart the app after copying model files.
- Try running the app once as Administrator only to rule out permission issues.
- Make sure Vulkan-capable GPU drivers exist in the VM. If not, the current server args may need a CPU fallback profile.

If the installer runs but the backend does not start, verify that these files were bundled:

```text
src-tauri\resources\runtime\llama-server.exe
src-tauri\resources\runtime\llama.dll
src-tauri\resources\runtime\ggml*.dll
```

Rebuild with:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\download-llama-runtime.ps1
powershell -ExecutionPolicy Bypass -File scripts\build-installer.ps1 -CleanResources -NoModels
```

