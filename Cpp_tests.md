# Llama.cpp Gemma 4 Audio Handoff

## Summary

- Tested Gemma 4 E4B with llama.cpp on RTX 4050 Laptop GPU via Vulkan.
- Best working model so far: `gemma-4-E4B-it.Q4_K_M.gguf`.
- Audio test file: `C:\Users\ashis\Pictures\test_audio.wav`.
- Correct transcript returned consistently: `What's the weather like?`
- 10 warm sequential server requests after model load:
  - Avg TTFT requests 2-10: `59.10 ms`
  - Avg TPS requests 2-10: `53.95 tokens/s`
  - Max GPU util observed: `96%`
  - Max VRAM used: `4519 MiB`

## Local Files

- llama.cpp: `C:\Users\ashis\AppData\Local\Microsoft\WinGet\Packages\ggml.llamacpp_Microsoft.Winget.Source_8wekyb3d8bbwe`
- Model: `D:\AppsMade2\CppGemma\models\gemma-4-E4B-it-Q4_K_M\gemma-4-E4B-it.Q4_K_M.gguf`
- MM projector: `D:\AppsMade2\CppGemma\models\gemma-4-E4B-it-Q2\gemma-4-E4B-it.mmproj-Q8_0.gguf`
- Latest result: `D:\AppsMade2\CppGemma\llama-q4-server-10req-nvml-result.json`
- Per-request CSV: `D:\AppsMade2\CppGemma\llama-q4-server-10req-nvml.csv`

## Reproduce

Start the server after cold load:

```powershell
& "C:\Users\ashis\AppData\Local\Microsoft\WinGet\Packages\ggml.llamacpp_Microsoft.Winget.Source_8wekyb3d8bbwe\llama-server.exe" `
  -m "D:\AppsMade2\CppGemma\models\gemma-4-E4B-it-Q4_K_M\gemma-4-E4B-it.Q4_K_M.gguf" `
  --mmproj "D:\AppsMade2\CppGemma\models\gemma-4-E4B-it-Q2\gemma-4-E4B-it.mmproj-Q8_0.gguf" `
  --host 127.0.0.1 --port 8099 `
  --device Vulkan1 -ngl all --fit on --fit-target 384 `
  -c 2048 -np 1 --jinja --metrics --no-webui
```

Send audio via OpenAI-compatible `/v1/chat/completions` using an `input_audio` content block:

- Base64 encode `C:\Users\ashis\Pictures\test_audio.wav`.
- Use prompt: `Transcribe this audio exactly. Return only the words spoken in the audio.`
- Set `stream: true` and measure time from request start to first streamed `delta.content` as TTFT.
- Use response `timings.predicted_per_second` for server TPS.

## Notes

- Request 1 after server start may be slower because the prompt/audio cache is cold.
- Requests 2-10 are the meaningful steady-state numbers.
- Q2_K was faster but produced garbled transcription; Q4_K_M produced the correct transcript.
- Server was stopped after benchmarking, so restart it before reproducing.
  Empty hepatotah.
