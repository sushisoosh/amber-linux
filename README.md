# Amber for Linux

An independent Linux desktop port of [Amber for Windows](https://github.com/EinFabo/amberpc),
originally created by [EinFabo](https://github.com/EinFabo). Speak into a microphone,
recognize speech locally or through a provider, review the result, and copy or paste it
into another application.

This is a derivative port, not a clean-room rewrite or an official Linux release from
the original creator. The original application, design, artwork, and retained source
come from Amber; this tree adds Linux desktop integration, packaging, and further
model, provider, and workflow features. The original Git history is preserved, with
Linux development following upstream commit
[`64d67e3`](https://github.com/EinFabo/amberpc/commit/64d67e308511cb922e1de957d7896b1cdd56e42d).

## License and Publication Status

**Amber is not Apache-licensed.** The original [LICENSE](LICENSE) says
`Copyright (c) 2026 EinFabo. All rights reserved.` It requires prior written
permission from the copyright holder to modify or redistribute the source, artwork,
or a modified version. Its limited allowance for GitHub forks is not a general
license to publish this port. Attribution alone does not grant those rights.

No permission or endorsement is asserted by this README. Obtain written permission
before publishing modified source or packages, and retain the original license and
copyright notices. The license directs permission requests to the
[original project's issues](https://github.com/EinFabo/amberpc/issues).
Third-party libraries, runtimes, and models retain their separate licenses; those
licenses do not relicense Amber itself.

For maintainers, [Publishing to GitHub](docs/github-publishing.md) explains the
permission gate, creating a linked fork, preserving the existing local remote,
reviewing files, and pushing deliberately. Build instructions below describe the
checkout; they do not grant modification or redistribution permission.

## Features

- Local CPU speech recognition with Whisper, Parakeet, or Moonshine, plus hosted
  transcription with your own credentials.
- A model library, checksum-verified downloads, and offline imports of supported
  local speech models.
- Rule-based cleanup, optional local or hosted smart cleanup, saved provider
  connections, and opt-in keyword instructions.
- A Recorder for reviewing results and a Testing page for comparing engines and
  cleanup settings using the same recording.
- X11 shortcuts and guarded clipboard paste; desktop-portal shortcuts and manual
  paste on Wayland.
- Optional encrypted history and saved testing tables, with encrypted credentials
  backed by the desktop's Secret Service wallet.

Start with [Build and Install](#build-and-install), then [First Run](#first-run).
Read [Linux Desktop Support](#linux-desktop-support) before enabling automatic paste
and [Privacy and History](#privacy-and-history) before choosing hosted services.

## Linux Desktop Support

The desktop port targets x86_64 Linux on Arch/i3, GNOME, and KDE. It remains a preview
until the real-desktop integration matrix has passed, not a claim of Windows feature
parity or support for every distribution. GPU acceleration, ARM, Flatpak, and AppImage
support are deferred. Windows and standalone-server code remain in this repository;
this README describes the Linux desktop, not those separate applications.

- Local CPU Whisper inference is built in; only its model weights need downloading.
  Parakeet and Moonshine additionally download a verified ONNX Runtime on first setup.
  Hosted recognition and optional local/server cleanup are supported.
- Dictation shortcuts leave Amber in the background. The Linux Recorder page keeps results
  in memory for review and explicit Copy by default. Under **Delivery**, enable **Copy and
  paste completed dictation** to copy completed text and paste automatically on
  eligible X11 targets. Completion delivery remains **off by default**. Copy, completion
  delivery and explicit insertion replace clipboard text, images or files without restoring
  them; clipboard managers may retain the text. Keep Amber running until you paste.
- On X11, **Paste into any window** is **on by default**: browsers, chat apps, dialogs,
  terminals and other window types are eligible, not just known native editors. Turning it
  off restores the native-editor restriction. This option also applies to
  `amber-dictation --insert-last`, which captures the currently focused destination when
  invoked and works independently of completion delivery.
- X11's **Allow pasting into a different window** is **off by default**: completion requires
  the window captured at recording start. When enabled, the currently focused destination
  is chosen once when delivery begins, not when recording stops. Both target options can
  be configured while completion delivery is off.
- Under **Delivery > Paste shortcut** on X11, choose `Ctrl+V` (the unchanged default,
  stored as `insert.pasteShortcut: "Ctrl+KeyV"`), `Ctrl+Shift+V` (common in terminals),
  or `Shift+Insert`, or record a custom combination. Choose the binding the target
  application actually uses for clipboard paste: the same binding applies to **all targets**,
  for both automatic completion and explicit `--insert-last`. This setting is available with
  completion off; it does not enable completion or change either target option.
  It is sent to the destination, not registered as a global shortcut; avoid the recording
  hotkey or other globally bound commands. Some X11 apps use `Shift+Insert` to paste the
  primary selection, not the clipboard; Amber leaves that selection unchanged. The recorder
  uses logical letters, so V remains V on Colemak/Dvorak. Escape cancels recording;
  Enter/Return/NumpadEnter, unmodified typing and modifier-only bindings are not allowed.
  F1-F24 can stand alone. Shifted numbers/punctuation and AltGr are not recorded because
  their logical base key cannot be determined reliably across layouts.
  **Reset to Ctrl+V** restores the default binding only.
  Invalid imported bindings are not silently replaced; invalid saves are rejected.
  Windows still uses Ctrl+V, and Wayland still requires manual paste.
- All X11 paste still requires single-line text without control characters and an observable
  non-Amber target with stable window and input focus during paste. Amber sends the configured
  paste shortcut, never Enter or a focus switch. Changing the binding does not relax these
  safeguards. Editable fields (including password fields) and paste acceptance cannot be verified. Targets that
  change during paste, unobservable targets and multiline/control-character text require
  manual paste; completed results remain in Recorder even when automatic paste is blocked
  or cannot be confirmed.
- Wayland shortcuts use the desktop portal when available. GNOME 48+ and KDE Plasma
  6 are the primary targets. On older/missing portal backends, configure a desktop
  shortcut for `amber-dictation --toggle`; `amber-dictation --cancel` stops/cancels a dictation. That
  fallback is toggle-only. Wayland remains manual-paste-only; clipboard access requires
  Amber to be focused.
- Credentials and optional history use authenticated encryption with a key in Secret
  Service (GNOME Keyring or a compatible wallet). A missing/locked wallet never
  causes plaintext persistence. After a credential-save error, changes remain active
  in the current session, but persistence may have failed or be unconfirmed; do not
  rely on them surviving a restart. History, statistics, and login startup are opt-in.
- Phone/tunnel/control servers, automatic terminal-command mode, Discord mute, automatic
  per-app profiles, the Windows updater, and the Windows floating bubble are disabled
  in the Linux desktop. Updates are installed from Linux packages.

## Build and Install

Run these commands from the repository root. Package generation does not demonstrate
compatibility with every distribution; test installation on the intended desktop.

### Arch/i3

Build a native package from this checkout, subject to the license requirements above.
The helper includes current uncommitted source files; it does **not** clone the
unported upstream repository or require a hardcoded fork URL.

```bash
sudo pacman -Syu --needed base-devel nodejs npm
bash tools/arch.sh --force --install
amber-dictation
```

`makepkg` runs as your regular user and asks pacman for missing build/test dependencies.
An existing rustup installation can provide Cargo; use a current stable toolchain
(the locked desktop dependencies require Rust 1.90 or newer). Build concurrency
defaults to two jobs; set `CARGO_BUILD_JOBS` to override it. Packages are kept in
`target/arch`, and the helper prints its staging directory so a failed build can be
inspected. The installed command is **`amber-dictation`**, not `amber`, to avoid
colliding with unrelated Arch/AUR programs.

`--force` rebuilds the current source even if `target/arch` already contains this version.
Without it, `makepkg --install` can reinstall an older build instead of compiling your changes.

The source snapshot uses a fresh native build directory. If changing the `GGML_*`
CPU flags in an existing checkout, first run `cargo clean -p whisper-rs-sys`:
the upstream build script does not track those environment changes reliably.

To prepare sources on a non-Arch machine instead:

```bash
bash tools/arch.sh --prepare-only /tmp/amber-arch-source
```

Transfer that directory to Arch, then run `makepkg --syncdeps --check --install`
inside it as a regular user. Its generated PKGBUILD pins the archive SHA-256.
Repository metadata, build outputs, dependency trees and common signing-secret
files are excluded. Runtime/model redistribution and public publishing still need
the appropriate permissions; no AUR upload or release publication is automated.

First run: select a microphone, download a speech model, and test the Recorder page.
CPAL uses ALSA: for a PipeWire desktop, install `pipewire-alsa` if the default input
is missing. For a PulseAudio desktop, the alternative is `pulseaudio-alsa`. Do not
replace a working audio stack or install both routes blindly. Persistent encrypted
credentials/history need an unlocked Secret Service implementation such as GNOME
Keyring; local recognition and session-only credentials work without one. Bare i3
does not automatically unlock a keyring.

Optional i3 config entries, using unused keys of your choice:

```i3
exec --no-startup-id amber-dictation --autostart
bindsym $mod+F9 exec --no-startup-id amber-dictation --toggle
bindsym $mod+F10 exec --no-startup-id amber-dictation --cancel
bindsym $mod+F11 exec --no-startup-id amber-dictation --insert-last
```

Amber's own X11 shortcut is `Ctrl+Alt+Space` by default. Do not register the same
combination in both Amber and i3. Bare i3 does not process XDG autostart files:
use the explicit `exec` line above **or** an XDG launcher such as `dex`, not both.
A tray host and a notification daemon are optional; the Recorder window and its
Quit button must remain usable without them. The explicit paste command uses Delivery's
**Paste shortcut** and **Paste into any window** option and captures its current destination.
Single-line, control-free text and stable focus are required; the shortcut never switches windows or
sends Enter, and cannot confirm that the destination accepted the paste.

For locking, install `i3lock` and optionally `xss-lock`. Direct `i3lock` does not
reliably announce a login1 lock. The packaged wrapper requests Amber cancellation
(including the microphone test) before running the locker, without preventing
locking when Amber is absent/unresponsive:

```i3
bindsym $mod+Shift+l exec --no-startup-id amber-i3-lock --nofork
exec --no-startup-id xss-lock --transfer-sleep-lock -- amber-i3-lock --nofork
```

Do not add a second locker alongside an existing one; adapt the existing binding.
Cancellation is a best-effort request, not proof that an unresponsive process
released its microphone. Suspend/session monitoring and its availability are shown
in Recorder diagnostics. Actual audio, keyring, lock and resume behavior still
require a check on the destination desktop.

Reproducible checks (X11 tests create a disposable authenticated Xvfb/i3 session,
never use the current desktop):

```bash
npm ci
npm run test:ui
python tools/test-arch.py
python tools/test-x11.py
cargo test --workspace --locked
```

The X11 integration tests use fake client windows to validate focus, clipboard
transfers and input events. They do not prove text landed in a real editor, identify
all terminal/password fields, or replace a microphone test on the destination PC.

### Ubuntu Build

Build on Ubuntu 24.04 with current stable Rust, Node.js 22, and:

```bash
sudo apt-get update
sudo apt-get install build-essential cmake pkg-config clang libclang-dev \
  libwebkit2gtk-4.1-dev libgtk-3-dev libasound2-dev libayatana-appindicator3-dev \
  librsvg2-dev libxdo-dev libxtst-dev libssl-dev libdbus-1-dev rpm
npm ci
npm run build
npm run test:ui
cargo test -p amber-core -p amber-inference -p amber-server -p amber-pc --locked
npm run app:build -- --bundles deb,rpm
```

The Linux Tauri configuration is selected automatically. `.deb`/`.rpm` outputs are
under `target/release/bundle/`. Model tests marked ignored require explicitly
provided model files; ordinary tests do not download models or contact paid APIs.
CI builds/tests/packages are not substitutes for microphone, keyring, clipboard,
focus, suspend/revocation, native Wayland/XWayland, and clean-install testing on real
GNOME/KDE sessions. Wayland automatic text injection is not implemented; use review,
Copy, and manual paste. GPU/ARM/Flatpak/AppImage support is deferred.

For development, `npm run app` starts the complete Tauri application and keeps running
until stopped with `Ctrl+C`. `npm run dev` starts only the frontend, not the desktop
backend. Run these commands from the repository root after installing the prerequisites
and running `npm ci`. The upstream Windows installers do not contain this Linux port;
no published Linux download is assumed here.

For the disposable X11 integration tests on Ubuntu, also install their desktop-test
dependencies and run the harness with Python 3:

```bash
sudo apt-get install xvfb i3-wm xauth x11-utils x11-xkb-utils fonts-dejavu-core
python3 tools/test-x11.py
```

## First Run

1. Launch `amber-dictation` after installation, or use `npm run app` for development.
2. Choose and test a microphone. On bare i3, check the audio and keyring notes above.
3. In **Engine**, download a local speech model, import a supported model, or choose a
   hosted provider and add its key in **Credentials** or **Connections**.
4. Record a short phrase in **Recorder**. Review the result and use **Copy**, then paste
   manually into a test document.
5. Set a dictation shortcut. On X11, enable automatic completion delivery only after
   reviewing the clipboard and focus safeguards above. Wayland requires manual paste.

Start with local speech and no hosted cleanup if you want recognition to stay on your
machine. Initial model/runtime downloads need a network connection; recognition with
downloaded local models can run offline.

## Speech Engines

| Engine | Where your voice goes | Notes |
|---|---|---|
| Whisper, CPU | stays on this PC | Built-in inference; download or import model weights |
| Parakeet, CPU | nowhere | v3 int8: 25 languages including German; v2 int8: English only |
| Moonshine, CPU | nowhere | Tiny/Base int8, English only; long recordings are chunked |
| Groq | Groq | Whisper presets; provider account and API key required |
| OpenAI | OpenAI | GPT-4o (mini) Transcribe, Whisper |
| Tinfoil.sh | Tinfoil | Voxtral Small 24B (default), Whisper large-v3-turbo; HTTPS without client enclave verification |
| Mistral | Mistral | Voxtral Mini Transcribe |
| Deepgram | Deepgram | Nova-3; native transcription API |
| ElevenLabs | ElevenLabs | Scribe v2; no speaker labels or paid vocabulary add-on |
| Own server | your configured endpoint | Compatible `POST /v1/audio/transcriptions` API; HTTPS required except for literal loopback IPs |

For Tinfoil on either desktop platform, save an inference API key from
[its dashboard](https://dash.tinfoil.sh) on the **Credentials** page. Keys can be managed
without changing your engine or cleanup selections, including local speech recognition
with Tinfoil cleanup. Select **Tinfoil.sh** on the Engine page to use it for speech. The preset uses
`https://inference.tinfoil.sh/v1`, not a model-page URL or the full transcription endpoint.
Select Tinfoil.sh separately under Cleanup to reuse that key with DeepSeek V4.1 Flash
(default), Gemma 4 31B, or Llama 3.3 70B. DeepSeek and Gemma have thinking disabled.
Unlike Android, the desktop client does not verify enclave attestation. The connection
test only fetches the public model catalog; a successful test does not validate the key
or prove that inference works.

The default Whisper catalog includes **FUTO Base**, **FUTO Small**, and **ACFT Large v3
Turbo q8_0**. Model licenses are separate from Amber's license. Larger models require
more memory and CPU time; choose one that fits your machine.

With streaming chunking active, long dictations are **recognized while you speak**:
finished parts are recognized at pauses while you continue recording. Testing and
enabled keyword instructions process complete recordings instead.

Catalog downloads are pinned by URL and SHA-256 in `crates/core/src/local.rs` and
`crates/core/src/onnx.rs`. Mismatched checksums are rejected. Linux Whisper inference
runs inside the application, not as a Windows executable or an external speech server.

The **Engine > Model library** also offers stock Whisper Tiny, Base, Small, Medium,
Large v3 and Turbo variants, including compact Q5, Q8 and F16 weights and selected
English-only variants. These use the full encoder context; the tuned dictation defaults
remain unchanged. **Use downloads when finished** can be switched off to download without
changing the selected model. Downloads remain cancellable and checksum-verified.

Parakeet and Moonshine use `transcribe-rs` with CPU ONNX Runtime 1.24.4. Their first download
also fetches a checksum-pinned Microsoft native runtime package (about 119 MiB); only the
platform libraries and license notices are installed. No Python or .NET setup is needed.
The supplied ONNX runtime targets x64 GNU/Linux with glibc 2.27 or newer and a compatible
system C++ runtime; that is not a compatibility guarantee for the whole desktop app.
Parakeet v3 detects its 25 supported languages automatically; v2 and
these Moonshine variants only recognize English. Neither engine uses the vocabulary prompt
or forces the selected language. Long ONNX recordings use bounded chunks, which can affect
words near chunk boundaries. Model manifests and runtime hashes live in `crates/core/src/onnx.rs`.

### Model Memory

Downloaded, selected and loaded are separate states. With **Load when Amber starts** on
(the default), Amber preloads the selected local speech model at startup. Otherwise a
dictation starts loading it while you speak; Testing loads it on demand. It stays resident
between dictations. **Load now** warms it explicitly; **Unload model** releases the speech
model without deleting the files or changing the selection. Active desktop recordings/tests
must finish first, and in-flight local inference holds the model lock. The next use loads
it again. Switching models replaces the resident speech model; switching to a server or
quitting releases it. Ordinary local use has no configurable idle-unload timer. A model
used for server fallback retains the existing ten-minute idle cleanup behavior.

### Custom Model Imports

**Engine > Import a local speech model** accepts Whisper GGML `.bin` files, Parakeet int8
ONNX folders, and Moonshine Tiny/Base ONNX folders. Browse to the source, name the model,
declare its actual supported languages, and confirm that you trust its source. This is a
speech importer, not a cleanup GGUF or arbitrary-ONNX importer. Each ONNX variant lists the
exact filenames it expects; external-data ONNX files and symlinks are rejected.

Imports copy files into managed storage, validate their structure and record SHA-256
checksums. Source files are never changed or deleted, and their paths are not persisted.
Checksums detect changes but do not establish publisher trust or architecture compatibility.
Imports appear under **Imported models** and can be selected, unloaded and deleted like
catalog models; deleting removes only Amber's copy. Importing is offline. If ONNX Runtime
is missing, its separate **Install ONNX runtime** action explicitly downloads it.
The importer reclaims its own abandoned staging/unregistered copies before a subsequent
import. Failed deletions remain visible and retryable. **Select after import** is persisted
by the backend, so closing the settings window does not lose that preference.

If local fallback is enabled and the selected local model/runtime is installed,
Amber can attempt local recognition when the speech server is unavailable. Fallback
can itself fail; local recognition never silently falls back to a hosted provider.

On Linux, API keys are stored in an encrypted vault backed by Secret Service. A locked
or unavailable wallet does not cause a fallback to plaintext credential storage.

### Audio Uploads

Under **Engine > Server > Audio upload**, choose **Ogg Opus (lossy, ~32 kbps)** to reduce
the audio sent for recognition. Encoding runs locally using the bundled codec, without
temporary files or an extra encoder installation. WAV remains the lossless default;
local recognition and local fallback always receive the original WAV. Compression can
change recognition quality and adds encoding time, so it is not guaranteed to reduce
total latency on every connection.

Groq and OpenAI document Ogg input. Tinfoil's published Voxtral server source accepts
Ogg Opus, although its model documentation lists MP3/WAV and live authenticated Opus
inference has not been verified here. Other servers may reject it. Custom Amber servers
are checked against their advertised formats before uploading; older Amber catalogs without
an audio list retain their documented WAV baseline. If WAV is selected instead of Opus,
that decision is shown in the test result. Amber never retries a rejected paid request
automatically with a different format.

The Mistral, Deepgram and ElevenLabs adapters currently use WAV even when Opus is selected.
This is decided before uploading and recorded in Testing details, not a retry of a paid
request. Deepgram and ElevenLabs vocabulary hints are omitted because their keyterms options
add a surcharge.

## Testing

The **Testing** page records comparisons without pasting, typing, changing the clipboard,
or adding transcripts to ordinary history/statistics. The comparison table aligns model
identifiers, compression and smart-cleanup checkmarks, encoding/recognition/cleanup times,
the combined stage time, and result previews. Times are milliseconds; recognition includes
encoding, so the total does not add encoding twice. The Opus checkmark describes the actual
prepared upload, not just the requested setting. Missing stages are not shown as zero-time
successes. **Details** expands a row to show its captured settings, raw and cleaned text,
individual attempts, file sizes, and failures. File sizes exclude multipart overhead and
are not confirmation that all bytes were sent.

Record once, change Engine, Audio upload or Cleanup, and use **Replay** on a row (or
**Run again with current settings** in its details). Every replay starts from the same original prepared lossless
WAV, never from a compressed upload, and creates a separate comparison with the same
recording number. It does not open the microphone. Recognition wall time includes encoding,
model loading and retries; Testing processes complete recordings instead of live chunking.

The current table holds up to 20 results. **Clear table** removes only these unsaved rows.
Give the table an optional name and choose **Save table & start new** to keep its transcripts,
configuration and timings across restarts and start an empty table. Up to 10 saved tables
appear in separate sections below; they can be collapsed or explicitly deleted. Saving and
deleting use encrypted local storage (`testing-tables.bin`), not plaintext. Linux requires an
unlocked Secret Service wallet. A failed save
leaves the current table intact. Unlock the wallet and **Reload saved tables** to recover
from storage errors. Saved tables are not silently overwritten when the limit is reached.

Audio stays in memory only, shared across current and saved rows under a 64 MiB budget.
Replaying a saved row adds a new current row without changing that saved table. Removing
the last comparison that uses a recording releases it; the audio budget can also evict
the least recently used recording while leaving its text and timings intact. After Amber
exits, unsaved rows and all audio are gone. Saved tables remain, but their Replay buttons
are unavailable without the original audio. Selecting a server still sends audio or text
to that provider, including when replaying; Testing is not a privacy bypass.

## Cleanup

Under **Cleanup**, rule-based processing handles spoken punctuation, layout, and
filler words locally. Optional smart cleanup uses a language model to repair
self-corrections and punctuation, with checks intended to preserve meaning. Review
important text: those checks do not guarantee a correct transcript.

Local smart cleanup uses a separately downloaded Linux CPU llama.cpp runtime and
Qwen3 4B Instruct 2507 model, not the Whisper speech model. It runs an authenticated
server on `127.0.0.1`. Alternatively, select a hosted cleanup provider or an existing
local Ollama instance. Hosted cleanup sends recognized text to that provider even
when speech recognition itself is local. Automatic per-application profiles and the
Windows terminal-command mode are disabled on Linux.

Provider lists distinguish speech recognition from cleanup. Named providers offer curated
model presets; Own server accepts a model ID, but compatibility with arbitrary model
parameters is not guaranteed. The Ollama preset connects to `http://127.0.0.1:11434/v1` without a key;
install Ollama and pull the chosen model separately. Use Own server for a different address.
Connection checks do not submit inference requests and do not prove that a selected model
or API key can perform paid inference. Anthropic uses its native Messages protocol.

### Connections and Discovery

**Settings > Connections** saves multiple named provider accounts or OpenAI-compatible
base URLs. Each has an independent encrypted key, bound to its identity, provider and
origin; existing provider-default keys are not copied automatically. Choose a saved
connection separately under Engine and Cleanup. **Provider default** preserves the original
Credentials workflow. Selected connections cannot be removed from the UI until deselected.

**Discover models** makes one catalog GET using that connection's key, never an inference
request. Known operation capabilities are shown separately for speech and cleanup; models
without sufficient capability information are explicitly unclassified. Discovery is bounded
to one page and 1,024 model IDs, reports truncation, and does not prove inference access,
pricing or compatibility with every model's parameters. Curated presets and an explicit
advanced manual-ID option remain available. ElevenLabs' catalog primarily lists voice
generation models, so use the Scribe presets for transcription.

Saved custom URLs are exact API bases: `/models`, `/audio/transcriptions` and
`/chat/completions` are appended without changing the prefix. HTTPS is required except
for literal loopback addresses. Missing/stale connections fail rather than silently using
another endpoint or key. Testing snapshots retain the selected connections and instruction
rules; credential changes are blocked until the test or its cancellation has settled.

### Keyword Instructions

**Settings > Instructions** is a separate opt-in transformation feature. A first whole
keyword selects a saved prompt and is removed before transforming the rest of the dictation.
For example, **Use Bash example** creates an editable, initially disabled `bash` rule:

```text
bash list all files modified today
```

The chosen cleanup model can turn that into command text such as `find . -type f -mtime -1`.
Shell output is always review/copy-only, never automatically pasted or executed.
Choose Text output for transformations meant for normal
delivery, such as drafting an email. Rule prompts are ordinary settings, not secret storage.

Rules require both their own Enabled switch and global smart cleanup.
Matching happens before filler cleanup: `please bash...`,
`bashful...` and quoted keywords do not trigger the Bash rule. Failure keeps the original
request, including its keyword. Enabled rules defer streaming chunks so they apply once to
the complete utterance. Testing/replay and the explicit preview support them.
Ordinary cleanup retains its existing
meaning-preservation checks.

## Privacy and History

- Local speech recognition stays on this machine after setup. Hosted speech sends
  audio to the selected endpoint; hosted cleanup sends text. A local fallback cannot
  undo an upload already attempted against a server.
- Credentials, optional history, and explicitly saved Testing tables use encrypted
  storage with a key in Secret Service. Unlock the wallet for persistence; missing
  wallet access never enables plaintext credential/history storage.
- History, statistics, and login startup are opt-in on Linux. Ordinary settings,
  diagnostics, timing estimates, and optional statistics are not all encrypted.
  Instruction prompts and custom endpoint URLs should not contain secrets.
- Copying or pasting replaces the clipboard without restoring its old content.
  Other applications and clipboard managers may read or retain the copied text.
- Testing audio is held in memory, not saved with comparison tables. Saved tables
  contain transcripts and settings; delete them explicitly when no longer needed.

History supports text/program search and combined program, local-date, and insertion-status
filters. Search terms stay in memory only. **Delete all history** clears the entire
history, including entries hidden by the current filters.

## Layout

```text
crates/core/      platform-independent settings, providers, cleanup, downloads, tests
crates/inference/ shared local Whisper inference
crates/onnx/      shared Parakeet and Moonshine inference
src-tauri/        desktop application and platform-specific integration
src/             React settings, Recorder, Testing, and indicator UI
packaging/arch/   native Arch package recipe and desktop entry
tools/            build, packaging, and integration-test helpers
server/           separate standalone server, not the Linux desktop phone API
streamdeck/       retained upstream Windows Stream Deck integration
```

Keep platform-independent logic in `crates/core` and platform API work in `src-tauri`.
Keep `package.json` and the workspace version in `Cargo.toml` equal. The build and
test commands above cover source checks; real microphone, clipboard, wallet, focus,
lock/resume, Wayland, and clean-install tests remain necessary on target desktops.

## Updates and Related Projects

Linux updates are installed through rebuilt/replacement Linux packages, not the
Windows updater. Review the publication guide before enabling GitHub Actions: the
inherited CI can upload Linux package artifacts, and the server workflow can publish
releases when matching tags are pushed. Source publication and binary redistribution
both need the appropriate permission.

Do not use `tools/release.sh` to release this Linux port: it still targets the
original creator's Windows repository. The inherited server-release workflow instead
publishes to the repository where it runs, but its installer scripts still default
to upstream downloads. Neither workflow is a ready-to-use Linux desktop release
process. Upstream Windows updater and server-download URLs are not download links
for this port.

- [Original Amber for Windows, by EinFabo](https://github.com/EinFabo/amberpc): original
  application, Windows installation instructions, releases, and upstream development.
- [Standalone server documentation](server/README.md): retained server functionality,
  separate from this Linux desktop; its upstream download links do not install this port.
- [Publishing to GitHub](docs/github-publishing.md): progressive maintainer instructions.

Tinfoil enclave verification and configurable idle model unloading are not implemented.
The inherited [roadmap](docs/PLAN.md) also includes Windows/server plans; it is not a
promise that those features work on Linux.
