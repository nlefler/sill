# Sill client for Linux, in Rust: implementation plan

Status: M1 in progress (2026-11-05). Written from `Sources/StreamProtocol` (StreamMessage, Input,
Switcher, Viewport, RemoteTLS, Pairing, Remote), which is the wire format's source of truth.

## Scope

- Stream the whole host desktop, or one host window at a time, in a client window of its own.
- Mouse and keyboard events go to the host.
- Audio is v2: the hello omits `audio`, so a host never sends kind 43.
- The host streams one source at a time (`StreamCoordinator.active` is host-global) and multi-window is
  out of scope (docs/BRIEF.md). So the client shows the Desktop or one window, with a switcher, as the
  iOS app does. Showing several host windows at once is not a goal and needs no host change.
- No camera QR scanning, no USB-cable auto-pair, no `ask` flow in v1.

## Decisions (Noah, 2026-11-05)

- The client lives in this repo, in `LinuxClient/`, a Cargo workspace of its own (it does not touch
  `Package.swift` or the Xcode project).
- Super maps to Command and Alt to Option by default, configurable.
- Hardware decode baseline: Mesa VAAPI on Intel (the dev machine has Intel graphics). Software decode
  stays as the fallback and the CI path.

## Workspace

| Crate | Responsibility |
|---|---|
| `sill-proto` | Wire types, no I/O: the 14-byte header (kind u8, timestamp f64, keyframe u8, length u32, big-endian), all kinds, serde JSON payloads, `ParameterSets`, size caps, `ViewportBounds`. |
| `sill-net` | Discovery, TLS, pairing, session state machine, reader and writer tasks. |
| `sill-media` | HEVC decode and frame hand-off. |
| `sill-input` | winit events to `InputEvent`, the key-code table. |
| `sill-app` | Window, rendering, connect screen, settings. |
| `sill-mockhost` | A Rust test host, used by tests only. |

Crates: `tokio`, `rustls` (ring), `rcgen` or `p256`, `mdns-sd`, `serde`, `winit`, `wgpu`, `ffmpeg-next`.

## Milestones (one PR each, with its tests in the same PR)

### M1: `sill-proto` and a conformance harness

- Every payload the client touches: `WindowList`, `WindowInfo`, `StreamSource`, `LaunchApp`,
  `WindowCommand`, `Hello`, `Viewport`, `ClientStats`, `HostSettingsState`, `HostSettingsChange`,
  `Goodbye`, `MacInfo`, `PairRequest`, `PairResult`, `CursorShapeBlob`, `ImageBlob`, `InputEvent`.
- Unknown kinds are skipped, never fatal; unknown JSON keys are ignored; new fields are optional.
- The caps: 1 MiB for any client message, 4096 bytes for a pairing message, 32 MiB for a frame, 4 MiB for
  any other host message.
- `ViewportBounds`: each side 1 to 16384 points, a text scale 0.25 to 4 or none.
- Gate: round-trip and golden tests. Real captures (`Scripts/sillrelay.py --record`, `sillclient.py`)
  replace the hand-built fixtures when someone with a Mac records them.

### M2: TLS identity and session

- A self-signed P-256 device certificate, stored 0600. A custom rustls verifier on both sides trusts only
  the SHA-256 of the peer's SPKI; names, dates and chains are never looked at.
- TLS 1.3 only, tickets and resumption off, SNI `sill`, ALPN `sill/1`.
- Hello (kind 23: `appVersion`, `build`, `protocol` 1, `device`) is the first message.
- The host answers with the window list (2), settings (16), Mac info (18), then frames.
- Liveness: ping (kind 10) every 0.25 s, drop after about 6 s with no bytes, `clientStats` (kind 12) once
  a second, goodbye (kind 22) honoured with its `reconnect` flag.
- Gate: connects to `sill-mockhost` in CI; by hand, `SillHost --synthetic --pairing` on loopback.

### M3: Pairing

- Kinds 19 and 20 on a separate `sill-pair/1` connection: one message each way, then the host closes.
- Key: the QR link's 16-byte secret, or PBKDF2-HMAC-SHA256 (600,000 rounds, 32 bytes) of the 12 digits
  with salt `"sill-pair-v1" || fp_mac`.
- `proof_D = HMAC(key, "sill-pair-v1 device" 0x00 fp_device fp_mac)`; verify `proof_M` before saving.
- Fingerprints come from the TLS session, never from a message.
- Input: a pasted `sill://pair?...` link, or 12 digits and an address.
- Saved Macs: Mac ID, pinned fingerprint, addresses, name. Handle goodbye `removed` and `pairingRequired`.

### M4: Discovery and connecting

- Browse `_sill._tcp`; read TXT `r` (recognition tag) and `p` (`1` pairing required, `0` open, absent a
  plain door, which this client never dials).
- Add by address from saved records. Reconnect after 5, 10, 20, then every 30 s, at most 12 tries.
- Remote (away from home) dialing can follow; the same TLS stack serves it.

### M5: Video decode and display, Desktop first

- Kind 0: nal length byte, a count, then 2-byte-length VPS/SPS/PPS. Kind 1: length-prefixed NALs (hvcC
  style); convert to Annex B or build hvcC extradata.
- Low-delay decode, no reorder buffer (the host sends no B-frames). VAAPI HEVC on Intel through
  `ffmpeg-next` first (check the dev machine with `vainfo`), Vulkan Video and software behind it.
- wgpu on Vulkan (Mesa ANV). Start with NV12 uploaded to textures; dmabuf import is a later step.
- Present at once, no queue. On a decoder error flush and send kind 44, 2 s apart at least, longer after
  repeated failures.
- Viewport (kind 9) with the window's pixel size, scale and `fps` from the monitor, within
  `ViewportBounds` (the host refuses anything else whole).
- The Mac's cursor shape (kind 14) and pointer (kind 26) as a sprite overlay.
- Gate: 60 fps from `SillHost --synthetic` at the dev machine's native resolution with VAAPI, and a
  measured software-decode figure beside it.

### M6: Input

- Positions are fractions 0..1 of the video frame, not window pixels; allow for letterboxing.
- Pointer: `move`, `leftDown`, `leftUp`, `rightDown`, `rightUp` (kind 8).
- Scroll: wheel notches as plain `scroll`; touchpad as `scrollGesture(began)`, deltas, `ended`; momentum
  is optional and client-generated. Positive `dy` moves the content down.
- Keys: winit physical `KeyCode` to USB HID usage. Modifier bits are CGEventFlags: shift `0x20000`,
  control `0x40000`, option `0x80000`, command `0x100000`. When the window list says `keyRepeat >= 1`
  every key goes as a key and the host repeats it; `text` only for IME commits. Release every held key
  on focus loss.
- Gate: `SILL_TEST_INPUT_LOG=1` on a synthetic host prints what it would post.

### M7: Sources and settings

- Switcher with thumbnails (kind 3) and icons (kind 4); pick with kind 6, launch with kind 7, window
  buttons with kind 15, the app list (kind 5).
- Host settings through kinds 16 and 17, tokens increasing. Handle `locked` and `offConsole`.
- The OS window title follows the streamed window. Resizing sends a new viewport.

### M8: Polish

- Clipboard (kind 32, text), the Mac's menus (24, 25, 27), gestures (28), remote access.
- AppImage or Flatpak. Wayland and X11 specifics.

## Audio (v2)

Keep the module boundary clean so an AAC-ELD decode and playout path can be added later.

## Compatibility rules

- Always offer `sill/1`. Fields are additive and optional. Kinds 29 to 31 and 33 to 42 are unknown.
- Respect the size caps both ways.

## CI and tests

- Unit tests and golden fixtures per crate; `sill-mockhost` for session tests.
- A Linux job in `.github/workflows/ci.yml` (path-filtered to `LinuxClient/`): `cargo test`,
  `cargo clippy`.
- Manual checklist for Noah: pair, Desktop, window pick, typing, scroll, reconnect over Wi-Fi and
  Tailscale.

## Risks

- VAAPI HEVC coverage varies by GPU and driver; software decode must work.
- Wayland pointer and keyboard behaviour.
- Frame age assumes synchronized clocks (host wall-clock timestamps).

## M5 results (2026-11-05)

- `sill-media` (nal, decoder, recovery, viewport, cursor, stats, clip) and `sill-app` (`sill` binary) added; `sill-mockhost` gained an optional looping video clip (kind 44 restarts it). Dev shell: `LinuxClient/shell.nix` (`nix-shell`).
- VAAPI HEVC works on the dev machine (Intel Panther Lake, iHD 26.1.6, `LIBVA_DRIVER_NAME=iHD`, `LIBVA_DRIVERS_PATH` from intel-media-driver). Through ffmpeg 8.1 with the read-back to system memory, VAAPI measured 117 fps at 1080p, 27 at 4K and 8 at 8K; software (8 cores) 392, 124 and 34. VAAPI decode alone (read-back every 30th frame) 765, 249 and 76 fps: the CPU read-back of tiled surfaces is the cost, so dmabuf import into Vulkan is the next step for 4K and Retina-sized streams. 8192x8192 decodes on both.
- `SILL_DECODER=sw|vaapi|auto`; `auto` falls back to software when the VAAPI device or driver will not open or fails its first frames.

## M6 results (2026-11-05)

- `sill-input` (pure; keymap, mods, keyboard, pointer, scroll, feed, engine) is wired into `sill-app`: window events go through `InputEngine` and out as `AppCommand::Session(Command::Input(..))`; `control.rs` counts input messages per connection (`PointerFeed`) and drops kind 26 reports whose `seen` is lower, as iOS does. Own pointer/scroll input hides the Mac's sprite.
- Defaults: Super->Command, Alt->Option, Ctrl->Control (`ModifierMap`); wheel notch = 10 points (iOS measured 3); `reverse_scroll` and touchpad momentum configurable in `ScrollConfig`; Caps Lock stays local (letters carry Shift).
- Host check on a Mac (not run here): `SILL_TEST_INPUT_LOG=1 SILL_TEST_LOOPBACK=1 SILL_TEST_SOFTWARE_ENCODER=1 SILL_TEST_NO_ROUTER=1 SILL_TEST_NO_SCREEN=1 SillHost --synthetic`, then `sill --address 127.0.0.1:PORT --tofu` (see main.rs flags) and compare the "Test input:" lines with the engine's sequence.

## M7 results (2026-11-05)

- `sill-ui-state` (pure; ledger, auto_desktop, locked, switcher, model, controller) ports `HostSettingsLedger`, `autoDesktop`/`autoDesktopRecheck`, `LockedMac`; `sill-app` gains `ui.rs` (egui 0.32 over the wgpu 25 renderer: switcher, Apps drawer, Settings, Mac list with pairing, status bar) and a hub in `control.rs` (controller, dialing, pairing, Bonjour rows, the reconnect ladder, saved-Mac standing from goodbyes, kind 18 addresses).
- The window sends `UiIntent`s as `AppCommand::Intent`; the hub publishes a `Model` as `UiEvent::Model`. The viewport and the input engine's letterbox are the video area the panels leave (`InputEngine::set_origin`, `Renderer::set_area`). egui's `wants_pointer_input`/`wants_keyboard_input` gate forwarding; there is no CentralPanel (it counts as an egui area).
- Deviations: no drag reordering (host order), no tiles; typed addresses default to port 7455; the Desktop auto-ask is the full `autoDesktop` rule (re-check, closed-window wait) in place of M5's plain 10 s spacing; no per-Mac order or window-title sync beyond the OS title.

## M8 results (2026-11-05)

Tests: 287 across the workspace (255 before), `cargo clippy --workspace --all-targets` clean. Nothing was run against a real Mac.

- **Decode default.** `DecoderPref::Auto` now reads the coded size from the SPS when parameter sets arrive and uses software above 1920x1200 (`decoder::auto_kind`, `AUTO_SOFTWARE_ABOVE_PIXELS`), VAAPI at or below it (VAAPI with CPU read-back measured 27 fps at 4K against 124 for software; at 1080p it is 117 against 392, both above 60 fps, and VAAPI saves CPU). A smaller stream afterwards goes back to VAAPI if the device opened. `SILL_DECODER=sw|vaapi` / `--decoder` still force one. Unit test for the rule.
- **CI.** `.github/workflows/linux-client.yml` (ubuntu-24.04, path-filtered to `LinuxClient/` and itself; `cargo test --locked`, `cargo clippy --all-targets -- -D warnings`; actions pinned by the commits ci.yml uses); ci.yml now ignores `LinuxClient/**`. Not run on GitHub. Risk: `ffmpeg-next 8` needs FFmpeg 8 development files and Ubuntu 24.04's apt has 6.x, so the build step may fail there until a newer image or a vendored/static FFmpeg is used. CLAUDE.md's Layout has a short `LinuxClient/` entry.
- **Clipboard (kind 32, text).** `sill-proto::Clipboard`; `sill-ui-state::ClipboardSync` ports `ClipboardPolicy`'s device rules: subscribe when the window list says `clipboard` >= 1 and the switch is on, `{"stop":true}` when it goes off, 4 sends a second (subscriptions count), text 1 byte to 256 KB, the first look after subscribing is recorded and not sent, the Mac's text is written locally and recorded as seen so it is never sent back. `sill-app::clipboard`: `ClipboardBackend` trait (arboard with the Wayland data-control feature, or X11), a worker thread that looks every 0.25 s while subscribed and drops a look that began before a write; `SILL_NO_CLIPBOARD=1` turns it off. Settings window: "This computer" with the switch (on by default; outlives a session).
- **Menus (24/25/27).** `sill-proto::{MacMenu, MacMenuItem, FetchMenu, PressMenuItem}`; `sill-ui-state::MenuState`: subscription (kind 27 without id) at the first list, a fetch per opened menu under the shown title and version, tokens increasing, 20 fetches / 4 presses a second, only enabled leaves shown, a new version drops cached ids and pending tokens, stale and refused answers leave a note. UI: a "Menus" button once a top level has arrived, a window with the top level and the opened menu (submenus open in place, a crumb goes back, a leaf is pressed on the Mac).
- **Gestures (28).** `sill-proto::TrackpadGesture`; `GestureGate` sends only when the list says `gestures` >= 1, six names, 3 or 4 fingers, 4 a second. UI: six buttons in "This computer" (3 fingers; labelled with the Mac's default action). No keyboard chord yet.
- `sill-net`: `Event::{Clipboard, MacMenu}`, `Command::{Clipboard, FetchMenu, PressMenu, Gesture}`.
- **Not done:** real-host testing (still nothing has run against a Mac); a chord/mouse mapping for gestures; menus in a menu-bar style popup (it is a window); image clipboard (v2 on the wire too); remote access; link-local IPv6 dialing (mdns-sd drops scope ids); packaging (AppImage/Flatpak); VAAPI to Vulkan dmabuf import (would make hardware decode win above 1080p); interactive checks in a real window.

## M9 results (2026-11-05)

Tests: 298 across the workspace (287 before), `cargo clippy --workspace --all-targets` clean. Nothing was run against a real Mac; everything below ran on this machine (Intel Panther Lake, Mesa ANV, iHD) against the in-process mock host (`SILL_MOCK_CLIP=file.hevc`, `SILL_MOCK_FPS=n`, test-only, stream a clip of your own).

- **What works: VAAPI to Vulkan with no CPU copy.** The decoder exports each decoded surface as a DRM PRIME dmabuf (`av_hwframe_map` to `AV_PIX_FMT_DRM_PRIME`, which waits for the decode; `sill_media::PrimeFrame` owns the mapped AVFrame, so the dmabuf fd and the VAAPI surface live as long as it does). The window's Vulkan device imports it (`sill-app/src/zerocopy.rs`): `VkImage` of `G8_B8R8_2PLANE_420_UNORM`, `DRM_FORMAT_MODIFIER_EXT` tiling with the modifier and plane layouts the decoder reports (explicit), `VkImportMemoryFdInfoKHR` over a dup of the fd with a dedicated allocation, wrapped with `wgpu::hal::vulkan::Device::texture_from_raw` and `create_texture_from_hal` as a wgpu `NV12` texture. The renderer samples its two plane views (R8 and RG8) with the shader it already had; the only shader change is a crop (the surface is aligned up, 1088 rows for 1080) clamped to texel centres.
- **Device.** wgpu 25 has no hook for extra device extensions, so `zerocopy::open_device` builds the `ash::Device` itself (queue family 0 as wgpu-hal does, wgpu's own extensions and features for NV12 plus `VK_KHR_external_memory_fd`, `VK_EXT_external_memory_dma_buf`, `VK_EXT_image_drm_format_modifier`) and hands it over with `device_from_raw` and `create_device_from_hal`. Used by the window and by `--headless`; the plain `Gpu::request` (tests) is unchanged. ANV exposes everything; a surface-compatible device made this way presents fine in a real Wayland window (checked with `--mock`, 1080p and 4K).
- **Lifetime and synchronisation.** The imported image's wgpu drop callback owns the `Arc<PrimeFrame>` and destroys the image and memory, so wgpu frees them only once the GPU work that used the texture is done: the dmabuf and surface cannot go away under the GPU, and the frame in the slot or on screen keeps its own. The decode side is synchronous (FFmpeg `vaSyncSurface`s in the map), so the GPU never reads a surface the decoder is still writing; no queue-family-foreign barrier is recorded (same device, Mesa), which is the one thing a stricter driver could want. A budget of `MAX_LIVE_PICTURES` (6) exported pictures alive at once (`LiveFrames`, a guard in each `PrimeFrame`) keeps the decoder's surface pool from running dry (`extra_hw_frames` is set to 8): past it a picture is not exported or shown, which the unit test pins.
- **Choosing the path, at run time, logged.** The GPU decides and sets a process-wide state (`sill_media::zerocopy::State`: Unknown, Active, Off). Log lines: `video path: zero-copy VAAPI dmabuf -> Vulkan (...)` or `video path: CPU upload (zero-copy off: <why>)` when the device is made, `decode path: ...` whenever the decoder's way changes, and the HUD/`Report.decoder` carry it ("VAAPI (ffmpeg hevc), zero-copy dmabuf to Vulkan" or ", read back to memory"). Any failure falls back, per frame and for good: a missing extension or NV12 or modifier at device creation, an import error (`dmabuf import failed (...)`, state to Off), or an export error in the decoder (`prime_broken`); the decoder then reads pictures back, `Auto` re-chooses (a change of decoder restarts it and asks for a keyframe, kind 44), and the CPU upload path is exactly the old one. `SILL_ZEROCOPY=0|off|false|no` turns it off. TEST ONLY `SILL_TEST_ZEROCOPY_FAIL=1` fails every import (the fallback was run live: 1080p stays on VAAPI with read-back, 4K goes to software and keeps presenting).
- **`auto_kind(size, zero_copy)`.** Hardware at every size while zero-copy is possible, else the old rule (software above 1920x1200). The decode thread waits up to 2 s for the GPU's verdict before opening its decoder (not when software is forced), so a 4K stream does not start in software and restart; `--headless` without a GPU says Off at once.
- **Pure logic with tests** (`sill_media::zerocopy`): the env switch, the state, the live-picture budget, FOURCCs, the import plan from a DRM descriptor (NV12 as one two-plane layer or as R8 + GR88 layers; one object, a known modifier; refuses P010, several objects, bad planes), the modifier negotiation against the device's list (plane count must be 2), the crop; `pick_memory_type`; `auto_kind` with zero-copy. Integration tests that skip without the hardware: `sill-media/tests/zerocopy_state.rs` (export while Active, budget stops it at 6, read-back once Off) and `sill-app/tests/zerocopy.rs` (40 frames of the 720p fixture decoded by VAAPI and imported draw within 2/255 of the same frames decoded in software and uploaded; same on screen at 1080p by eye).
- **Numbers** (release build; decode+export is the bench, `bench FILE vaapi 3 prime`; the rest is `sill --mock --headless` with the mock sending at 240 fps, so it includes the import, the draw and a 1280x720 offscreen read-back per frame; software has 4 slice threads):

  | | 1080p | 4K | 8K |
  |---|---|---|---|
  | software decode only (bench) | 402 | 155 | 33 |
  | VAAPI + CPU read-back (bench) | 103 | 28 | (8 in M5) |
  | VAAPI decode + dmabuf export (bench) | 883 | 332 | 80 |
  | headless, software | 243 | 80 | 19 |
  | headless, VAAPI + read-back | 121 | 27 | 9 |
  | headless, zero-copy | 178-198 | 225-251 | ~90 |

  Zero-copy beats software from 4K up (3x at 4K, 5x at 8K) and read-back at every size; at 1080p the harness's per-frame work (and an import of ~tens of microseconds) leaves it just behind software, both far above 60 fps, with the CPU free. In a real window with `--mock` at 1080p and 4K the decoder kept up at 60 fps using about 1 s of CPU in 7 s.
- **What remains / limits.** 8-bit 4:2:0 only (a P010 or non-NV12 export falls back); a dmabuf with more than 2 memory planes (compressed or auxiliary planes) or several objects falls back, which is not what iHD exports for decode here; the Vulkan adapter is picked as before (low power), and it is not checked against the VAAPI render node, so on a machine with two GPUs a mismatch shows up as an import error and a fallback; no explicit foreign-queue ownership transfer; a dmabuf is imported per picture (no cache keyed by the buffer; each costs a `vkCreateImage` and an allocation, small beside decoding); the crop clamp changes the CPU path's sampling by nothing visible. Not run: AMD or NVIDIA, Wayland compositors other than this one, a real Mac stream (the mock clip is x265 test video, no B-frames, 8-bit).
