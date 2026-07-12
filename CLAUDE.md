# CLAUDE.md

This file provides guidance to AI coding assistants (Claude Code, Cursor, Copilot, etc.) when working with this repository.

## Project Overview

IMCA Client — a RoboMaster custom ground station client (Tauri 2 + Vue 3 + Rust). Receives UDP H.265 video from robots, GPU-accelerates decode via FFmpeg (VAAPI/QSV/CUDA), displays as MJPEG stream. Communicates over MQTT with Protobuf protocol.

## Build Commands

```bash
# Development (hot reload)
npm run tauri dev

# Release build (produces binary + deb + AppImage)
npm run tauri build

# Generate Protobuf JSON schema from .proto (required before build if proto changed)
npm run generate:mqtt-proto

# Measure MJPEG output FPS (while app is running)
./measure-fps.sh
```

## Architecture

### Frontend (Vue 3 + Vite)
- `src/views/Dashboard.vue` — main 6695-line component; all video/config/HUD UI
- `src/components/Dashboard/` — sub-components (ConfigPanels, VideoSourceManager, MiniMap, etc.)
- `src/api-shim.ts` — single bridge file; all Tauri `invoke()` calls go through `window.api`
- `src/store/modules/dashboard.ts` — Pinia store, central state management
- `src/components/Dashboard/types.ts` & `constants.ts` — TypeScript interfaces and defaults
- Vite dev server on port 1420; HMR on 1421

### Backend (Rust/Tauri)
- `src-tauri/src/udp_bridge.rs` — UDP receive loop, H.265 decode orchestration, MJPEG publish
- `src-tauri/src/udp_bridge/assembly.rs` — UDP fragment reassembly engine
- `src-tauri/src/udp_bridge/codec_detect.rs` — Annex B NAL parsing, keyframe detection
- `src-tauri/src/udp_bridge/mjpeg.rs` — MJPEG HTTP stream server
- `src-tauri/src/video_decoder/gpu.rs` — FFmpeg subprocess GPU decode (VAAPI/QSV/CUDA)
- `src-tauri/src/video_decoder.rs` — SmartDecoder + MjpegDecoder wrappers
- `src-tauri/src/mqtt_client.rs` — MQTT client (rumqttc)
- `src-tauri/src/shm_bridge.rs` — Unix Domain Socket / Named Pipe IPC for AI detection
- `src-tauri/src/config.rs` — persistent settings (Cache/settings.json)
- `src-tauri/build.rs` — compiles vendored libde265 + optional FFmpeg static libs

### Key Patterns

**Video pipeline**: UDP → assembly → keyframe gate → FFmpeg subprocess (H.265 decode → scale → MJPEG encode, all GPU) → JPEG SOI/EOI reader → MJPEG HTTP server → `<img>` tag

**MQTT protocol**: Protobuf v3 over MQTT, server at 192.168.12.1:3333. Proto file: `UDP-MQTT Server/proto/messages.proto`. Frontend uses generated JSON schema.

**Hardware decode order**: QSV (Intel best) → VAAPI (Intel/AMD) → CUDA (NVIDIA) → auto. MJPEG encoder follows: mjpeg_qsv / mjpeg_vaapi / mjpeg (software).

**Runtime tuning** (env vars, no rebuild needed):
- `SHARK_VAAPI_SCALE_HEIGHT` — output resolution (480/720/1080/0=raw)
- `SHARK_JPEG_QUALITY` — JPEG quality 1-100
- `SHARK_MAX_FPS` — frame rate cap
- `SHARK_DECODE_QUEUE` — decode queue depth (1=lowest latency)

**Frontend↔Backend**: All communication via `window.api` (api-shim.ts). Never call `invoke()` directly from Vue components.

## Dependencies

- Node.js 20+, Rust stable, FFmpeg (system), cmake (for turbojpeg)
- Tauri system libs: libgtk-3-dev, libwebkit2gtk-4.1-dev, libssl-dev, etc.
- GPU drivers: intel-media-va-driver (Intel), mesa-va-drivers (AMD), nvidia-driver (NVIDIA)

## Project Conventions

- Cargo package name uses hyphens (`imca-client-v1-5`), binary/lib use underscores (`IMCA_Client_v1_5`)
- Release profile: opt-level=3, LTO, codegen-units=1, panic=abort
- Resources (configs, images, protos) live in `resources/` and are bundled via tauri.conf.json
- AI detection is an external Python process (SharkVisionLiteServer), not embedded

## Protocol Status (V2.0.0)

`UDP-MQTT Server/proto/messages.proto` is aligned with RoboMaster protocol **V2.0.0** (2026.06.26): `GameStatus` includes its result fields, map-click traffic is split into `MapClickInfo` and `MapClickCmd`, `SentryStatusSync` includes `is_powered`, and the Sentry/AirSupport command values are updated. Run `npm run generate:mqtt-proto && npm run verify:mqtt-proto` after future protocol changes.
