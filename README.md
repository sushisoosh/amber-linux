# Amber for Linux

**Local-first speech-to-text for the Linux desktop.** Dictate, review the transcript,
and copy it into another application, with optional cleanup and guarded X11 paste.
This is an independent port of [Amber for Windows](https://github.com/EinFabo/amberpc),
originally created by [EinFabo](https://github.com/EinFabo), not an official upstream release.

**Preview:** targets x86_64 Linux with CPU inference. X11 and Wayland have different
capabilities; real-desktop validation is still required. There are no published Linux
desktop releases in this repository yet.

- **Local or hosted recognition:** Whisper, Parakeet, Moonshine, or your chosen provider.
- **Model library:** verified downloads and offline imports of supported speech models.
- **Optional cleanup:** local rules, local or hosted language models, and keyword instructions.
- **Compare results:** replay one recording with different models and cleanup settings.
- **Private storage:** encrypted credentials, opt-in history, and saved Testing tables.

[Build and install](#build-and-install) | [First run](#first-run) |
[Desktop support](#linux-desktop-support) | [Speech engines](#speech-engines) |
[Privacy](#privacy-and-history) | [Development](#development) | [License](#license-and-attribution)

## Build and Install

**Download status:** build from source using the instructions below. Upstream Windows
installers and Amber Server downloads do **not** install this Linux desktop port.
The CI configuration can build `.deb`/`.rpm` artifacts, but that is not a published
Linux release or a guarantee of desktop compatibility.

> **License notice:** this is source-available, not open-source software. The retained
> [LICENSE](LICENSE) is all rights reserved and requires prior written permission for
> modification or redistribution. These instructions grant no additional rights;
> see [License and Attribution](#license-and-attribution) before proceeding.

### Get the Source

Install Git through your distribution's package manager if needed, then clone this port:

```bash
git clone https://github.com/sushisoosh/amber-linux.git
cd amber-linux
```

Run the following commands from that repository root on the Linux machine you intend
to use. Choose the instructions for your distribution.

### Arch/i3

Build and install the current checkout as a native Arch package:

```bash
sudo pacman -Syu --needed base-devel nodejs npm
bash tools/arch.sh --force --install
amber-dictation
```

Run the helper as your **regular user**, not root. It snapshots the current files,
including uncommitted changes, then uses `makepkg` to install missing dependencies,
build, test, and install. It does not fetch the unported upstream tree. `--force`
ensures an existing package with the same version is rebuilt.

Packages land in `target/arch`; the installed command is **`amber-dictation`**, not
`amber`. Builds default to two jobs (`CARGO_BUILD_JOBS` overrides this). If you already
use rustup, select current stable Rust, at least 1.90. For microphone, keyring, startup,
and lock-screen setup, see [First Run](#first-run) and the i3 notes below.

### Ubuntu 24.04

Install **Node.js 22 with npm** and **current stable Rust (1.90 or newer)** first,
using the [Node.js installation instructions](https://nodejs.org/en/download) and
[rustup](https://rustup.rs/), or your existing toolchain manager. Ubuntu's default
Node.js/Rust packages may be older than required.

```bash
sudo apt-get update
sudo apt-get install build-essential cmake pkg-config clang libclang-dev \
  libwebkit2gtk-4.1-dev libgtk-3-dev libasound2-dev libayatana-appindicator3-dev \
  librsvg2-dev libxdo-dev libxtst-dev libssl-dev libdbus-1-dev python3
npm ci
npm run app:build -- --bundles deb
sudo apt install ./target/release/bundle/deb/*.deb
amber-dictation
```

`app:build` builds the frontend and desktop app and automatically selects the Linux
Tauri configuration. `apt install ./...deb` installs the resulting package and resolves
runtime dependencies. If the output directory contains several versions, give `apt`
the path to the **one** package you want instead of the wildcard. You can also launch
Amber from your application menu.

For the CI-style `.deb` and `.rpm` build, install `rpm` and run
`npm run app:build -- --bundles deb,rpm`. Outputs are under `target/release/bundle/`.
Generating an RPM on Ubuntu does not establish compatibility with an RPM-based distro;
use the `.deb` on Ubuntu. Linux updates are installed by rebuilding/replacing the package,
not through the Windows updater.

## First Run

1. Launch `amber-dictation` after installation, or `npm run app` for development.
2. Choose and test a microphone.
3. In **Engine**, download or import a local speech model, or choose a hosted provider
   and add its key under **Credentials** or **Connections**.
4. Record a short phrase in **Recorder**, review it, and use **Copy**. Paste manually
   into a test document first.
5. Set a dictation shortcut. The X11 default is `Ctrl+Alt+Space`. Automatic completion
   delivery is **off by default**; read the [paste safeguards](#linux-desktop-support)
   before enabling it. Wayland requires manual paste.

For offline recognition, choose local speech with no hosted cleanup. Initial model and
runtime downloads need a network connection; downloaded local models can run offline.

**Microphone missing on Arch?** Audio capture uses ALSA. On a PipeWire desktop, check
that `pipewire-alsa` is installed; on PulseAudio, the alternative is `pulseaudio-alsa`.
Do not replace a working audio stack or install both routes blindly.

**Credentials or history will not save?** Persistent encrypted storage needs an unlocked
Secret Service wallet, such as GNOME Keyring or a compatible wallet. Bare i3 does not
unlock one automatically. Local recognition and session-only credentials work without
one; a wallet error never enables plaintext persistence. After a credential-save error,
changes may work for this session without surviving a restart.

## Linux Desktop Support

The preview targets **x86_64 Linux on Arch/i3, GNOME, and KDE**, not every distribution
or Windows feature parity. GPU acceleration, ARM, Flatpak, and AppImage are deferred.

| Capability | X11 | Wayland |
|---|---|---|
| Record, review, and copy | Yes | Yes; focus Amber for clipboard access |
| Dictation shortcuts | Native global shortcuts | Desktop portal when available |
| Automatic paste | Opt-in, subject to focus/text safeguards | Not implemented; paste manually |
| Desktop shortcut fallback | `amber-dictation --toggle` / `--cancel` | Same commands; toggle-only |

GNOME 48+ and KDE Plasma 6 are the primary portal targets. On older or missing portal
backends, bind the commands above in your desktop settings. Tray and notification support
are optional; the Recorder and its Quit button remain available without them.

Linux has a **recording/processing indicator**: a click-through, non-focusable
floating window shown during recording, transcription, and insertion. It is not the
interactive Windows bubble: mouse dictation, dragging, and its Windows settings are
not offered on Linux. Placement and stacking depend on the compositor, especially on
Wayland; the Recorder remains the fallback.

**Before enabling paste:** **Delivery > Copy and paste completed dictation** is off by
default. Copy, automatic delivery, and explicit insertion replace the clipboard without
restoring its old text, images, or files; clipboard managers may retain the result.
Keep Amber running until you paste. Amber cannot verify editable fields, detect every
password field, or confirm that an application accepted the paste.

<details>
<summary>X11 delivery options and safeguards</summary>

- **Paste into any window** is on by default: browsers, chat apps, dialogs, and terminals
  are eligible, not just known native editors. Turn it off to restore that restriction.
  This does not enable automatic delivery by itself.
- **Allow pasting into a different window** is off by default: completion requires the
  target captured at recording start. When enabled, Amber chooses the focused target
  once when delivery begins, not when recording stops.
- `amber-dictation --insert-last` captures the currently focused target when invoked.
  It works independently of automatic completion delivery and follows **Paste into any
  window** and **Paste shortcut**.
- Every X11 paste still requires single-line, control-free text and an observable
  non-Amber target with stable window and input focus. Amber never switches focus or
  sends Enter. Blocked/unconfirmed results remain in Recorder for manual copying.

**Delivery > Paste shortcut** defaults to `Ctrl+V`. Choose `Ctrl+Shift+V` for applications
that use it, `Shift+Insert`, or a custom binding. The same binding is sent to **all targets**
for completion and explicit insertion; it is not registered as a global shortcut. Avoid
the recording hotkey or other global commands. Some applications use `Shift+Insert` for
the primary selection, which Amber leaves unchanged.

The binding recorder uses logical letters, including on Colemak/Dvorak. Escape cancels;
Enter/Return/NumpadEnter, unmodified typing, and modifier-only bindings are rejected.
F1-F24 can stand alone. Shifted numbers/punctuation and AltGr are not recorded because
their logical base key is ambiguous across layouts. **Reset to Ctrl+V** resets only
the binding (`insert.pasteShortcut: "Ctrl+KeyV"`), not delivery or target permissions.
Invalid imported bindings are not silently replaced, and invalid saves are rejected.

</details>

<details>
<summary>i3 shortcuts, autostart, and screen locking</summary>

Optional i3 entries, using unused keys of your choice:

```i3
exec --no-startup-id amber-dictation --autostart
bindsym $mod+F9 exec --no-startup-id amber-dictation --toggle
bindsym $mod+F10 exec --no-startup-id amber-dictation --cancel
bindsym $mod+F11 exec --no-startup-id amber-dictation --insert-last
```

Do not register the same combination in Amber and i3. Bare i3 does not process XDG
autostart files: use the explicit `exec` line above **or** an XDG launcher such as
`dex`, not both. The `--insert-last` binding follows the X11 safeguards above.

For locking, install `i3lock` and optionally `xss-lock`. Direct `i3lock` does not reliably
announce a login1 lock. The Arch package's wrapper requests cancellation, including the
microphone test, before running the locker; absent/unresponsive Amber does not prevent locking:

```i3
bindsym $mod+Shift+l exec --no-startup-id amber-i3-lock --nofork
exec --no-startup-id xss-lock --transfer-sleep-lock -- amber-i3-lock --nofork
```

Do not add a second locker alongside an existing one; adapt the existing binding.
Cancellation is best-effort, not proof that an unresponsive process released its
microphone. Recorder diagnostics show suspend/session-monitoring availability. Check
actual audio, keyring, locking, and resume behavior on the destination desktop.

</details>

Phone/tunnel/control servers, automatic terminal-command mode, Discord mute, automatic
per-app profiles, and the Windows updater are disabled in the Linux desktop. Retained
Windows and standalone-server code does not imply Linux desktop support for those features.

## Speech Engines

| Engine | Where your voice goes | Notes |
|---|---|---|
| Whisper, CPU | stays on this PC | Built-in inference; download or import model weights |
| Parakeet, CPU | stays on this PC | v3 int8: 25 languages including German; v2 int8: English only |
| Moonshine, CPU | stays on this PC | Tiny/Base int8, English only; long recordings are chunked |
| Groq | Groq | Whisper presets; provider account and API key required |
| OpenAI | OpenAI | GPT-4o (mini) Transcribe, Whisper |
| Tinfoil.sh | Tinfoil | Voxtral Small 24B (default), Whisper large-v3-turbo; HTTPS without client enclave verification |
| Mistral | Mistral | Voxtral Mini Transcribe |
| Deepgram | Deepgram | Nova-3; native transcription API |
| ElevenLabs | ElevenLabs | Scribe v2; no speaker labels or paid vocabulary add-on |
| Own server | your configured endpoint | Compatible `POST /v1/audio/transcriptions` API; HTTPS required except for literal loopback IPs |

Whisper inference is built in; download or import its model weights. Parakeet and
Moonshine also need the verified ONNX Runtime offered during setup. With streaming
chunking active, finished parts are recognized at pauses while you keep speaking.
Testing and enabled keyword instructions process complete recordings instead.

<details>
<summary>Tinfoil setup and verification limits</summary>

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

</details>

<details>
<summary>Model catalog, downloads, and runtime requirements</summary>

The default Whisper catalog includes **FUTO Base**, **FUTO Small**, and **ACFT Large v3
Turbo q8_0**. Model licenses are separate from Amber's license. Larger models require
more memory and CPU time; choose one that fits your machine.

Catalog downloads are pinned by URL and SHA-256 in [local.rs](crates/core/src/local.rs)
and [onnx.rs](crates/core/src/onnx.rs). Mismatched checksums are rejected. Linux Whisper inference
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

</details>

<details>
<summary>Loading, unloading, and importing local models</summary>

**Model memory**

Downloaded, selected and loaded are separate states. With **Load when Amber starts** on
(the default), Amber preloads the selected local speech model at startup. Otherwise a
dictation starts loading it while you speak; Testing loads it on demand. It stays resident
between dictations. **Load now** warms it explicitly; **Unload model** releases the speech
model without deleting the files or changing the selection. Active desktop recordings/tests
must finish first, and in-flight local inference holds the model lock. The next use loads
it again. Switching models replaces the resident speech model; switching to a server or
quitting releases it. Ordinary local use has no configurable idle-unload timer. A model
used for server fallback retains the existing ten-minute idle cleanup behavior.

**Custom imports**

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

</details>

<details>
<summary>Audio upload formats, compression, and provider limitations</summary>

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

</details>

## Testing

Use **Testing** to compare engines and cleanup settings with the **same recording**:
record once, change Engine, Audio upload, or Cleanup, then choose **Replay** on a row.
Each replay starts from the original lossless WAV, not a compressed upload, and does
not reopen the microphone. **Details** shows captured settings, raw/cleaned text,
timings, attempts, and failures.

Testing never pastes, types, changes the clipboard, adds ordinary history/statistics,
or trains the timing estimator. Choosing a hosted provider still sends audio or text
to it, including on replay; Testing is not a privacy bypass.

<details>
<summary>Timing interpretation, saved tables, and audio retention</summary>

Times are milliseconds. Recognition wall time includes encoding, model loading, and
retries; combined stage time does not add encoding twice. Testing uses complete recordings,
not live chunking. The Opus checkmark describes the actual upload format, not just the
requested setting. Missing stages are not zero-time successes. File sizes exclude multipart
overhead and do not confirm that every byte was sent.

The current table holds up to 20 results. **Clear table** removes only these unsaved rows.
Give the table an optional name and choose **Save table & start new** to keep its transcripts,
configuration and timings across restarts and start an empty table. Up to 10 saved tables
appear in separate sections below; they can be collapsed or explicitly deleted. Saving and
deleting use encrypted local storage (`testing-tables.bin`), not plaintext. Linux requires an
unlocked Secret Service wallet. A failed save leaves the current table intact.
Unlock the wallet and **Reload saved tables** to recover
from storage errors. Saved tables are not silently overwritten when the limit is reached.

Audio stays in memory only, shared across current and saved rows under a 64 MiB budget.
Replaying a saved row adds a new current row without changing that saved table. Removing
the last comparison that uses a recording releases it; the audio budget can also evict
the least recently used recording while leaving its text and timings intact. After Amber
exits, unsaved rows and all audio are gone. Saved tables remain, but their Replay buttons
are unavailable without the original audio.

</details>

## Cleanup

Under **Cleanup**, rule-based processing handles spoken punctuation, layout, and
filler words locally. Optional smart cleanup uses a language model to repair
self-corrections and punctuation, with checks intended to preserve meaning. Review
important text: those checks do not guarantee a correct transcript.

Local smart cleanup uses a separately downloaded Linux CPU llama.cpp runtime and
a selectable GGUF language model, not the Whisper speech model. It runs an authenticated
server on `127.0.0.1`. **Cleanup > On this PC** offers Qwen3 4B Instruct 2507 (the default),
Qwen2.5 1.5B, English-focused SmolLM2 1.7B, and Qwen2.5 7B, all in Q4_K_M quantization.
The catalog shows download sizes and approximate speed, memory, language, and license
guidance. Speed labels are relative, not benchmarks; compare actual cleanup with **Try it**
or the same recording in **Testing**. Memory use exceeds the downloaded file size.
Downloads are pinned to a revision and SHA-256 verified; they can be cancelled and resumed.
Turn off **Use downloads when finished** before starting a model download to keep your
current selection. This preference does not change downloads already running or runtime setup.

**Import a cleanup model** accepts an existing single-file GGUF instruct/chat model with
an embedded chat template. Browse to the file, name it, and confirm that you trust its
source. Importing is offline and copies the model into Amber's managed storage, so allow
extra disk space. The source file is never modified or deleted. Structure checks and a
recorded checksum cannot guarantee safety, llama.cpp compatibility, or cleanup quality.
Use non-thinking text models; split GGUF files, adapters, and speech models are not supported.
Imported models appear beside the catalog and can be selected or deleted independently.
**Use after import** saves the selection after copying; otherwise the current model stays
selected. The llama.cpp runtime is still required and is downloaded separately. Missing
selected models produce an error rather than silently switching to another cleanup model.

Alternatively, select a hosted cleanup provider or an existing
local Ollama instance. Hosted cleanup sends recognized text to that provider even
when speech recognition itself is local. Automatic per-application profiles and the
Windows terminal-command mode are disabled on Linux.

Provider lists distinguish speech recognition from cleanup. Named providers offer curated
model presets; Own server accepts a model ID, but compatibility with arbitrary model
parameters is not guaranteed. The Ollama preset connects to `http://127.0.0.1:11434/v1` without a key;
install Ollama and pull the chosen model separately. Use Own server for a different address.
Connection checks do not submit inference requests and do not prove that a selected model
or API key can perform paid inference. Anthropic uses its native Messages protocol.

<details>
<summary>Saved connections and model discovery</summary>

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

</details>

<details>
<summary>Opt-in keyword instructions</summary>

**Settings > Instructions** is a separate opt-in transformation feature. A first whole
keyword selects a saved prompt and is removed before transforming the rest of the dictation.
For example, **Use Bash example** creates an editable, initially disabled `bash` rule:

```text
bash list all files modified today
```

The chosen cleanup model can turn that into command text such as `find . -type f -mtime -1`.
Shell output is always review/copy-only, never automatically pasted or executed.
Choose Text output for transformations meant for normal delivery, such as drafting an
email. Rule prompts are ordinary settings, not secret storage.

Rules require both their own Enabled switch and global smart cleanup.
Matching happens before filler cleanup: `please bash...`,
`bashful...` and quoted keywords do not trigger the Bash rule. Failure keeps the original
request, including its keyword. Enabled rules defer streaming chunks so they apply once to
the complete utterance. Testing/replay and the explicit preview support them.
Ordinary cleanup retains its existing meaning-preservation checks.

</details>

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

### History and Timing Estimates

New history entries show the saved cleanup method/model and outcome alongside the
speech engine. Legacy entries show **Not recorded** rather than guessing from today's
settings.

**Comparable estimates** lists successful speech/cleanup combinations actually used
in retained history, respecting the current history filters. It normalizes recognition
to **10 seconds of audio** and model cleanup to **100 Unicode characters**, using the
**same persisted estimator as the progress ring**. Separate recognition, cleanup, and
combined estimates include sample counts and learned/default/legacy indicators.
Sample counts belong to each stage across successful dictations, not just the displayed
filter or pair; sparse or similar-length data is blended with prior estimates.

These are approximate latency estimates, **not a benchmark or quality score**, and not
averages calculated from history's wall times. Startup, retries, streaming, and different
inputs can change observed timings. Rules-only/snippet paths have no cleanup *model*
budget, not zero processing time. Missing route metadata, mixed-route streaming runs,
and failed/skipped model cleanup are excluded from the comparison. Use
[Testing](#testing) for same-recording comparisons.

Learning works even with history/statistics off. Compact timing aggregates persist
locally in `estimates.json`, separately from history, without audio, transcripts, or
individual timing records. Deleting history removes those combinations from this overview
but **does not reset learned estimates**. These aggregates are not encrypted.

## Development

Install the native dependencies and toolchains above, then run from the repository root:

```bash
npm ci
npm run app
```

`npm run app` starts the full Tauri desktop application and stays running until `Ctrl+C`.
`npm run dev` starts **only** the frontend, without the desktop backend. On Arch, the
package helper installs build/test dependencies but builds in a separate staging directory;
run `npm ci` in this checkout before developing here.

For source checks:

```bash
npm run build
npm run test:ui
python3 tools/test-arch.py
cargo test --workspace --locked
```

For just the platform-independent logic, use `cargo test -p amber-core --locked`.
Its bundled codec needs CMake; set `CMAKE` if it is not on `PATH`. Ordinary tests do not
download models or contact paid APIs; ignored model tests need explicitly provided files.

For disposable X11 integration tests on Ubuntu, install the additional dependencies:

```bash
sudo apt-get install python3 xvfb i3-wm xauth x11-utils x11-xkb-utils fonts-dejavu-core
python3 tools/test-x11.py
```

The Arch helper already includes these test dependencies and runs the X11 checks.
The harness creates an authenticated Xvfb/i3 session, not tests on your current desktop.
It checks focus, clipboard transfers, keyboard layouts, and input events with test windows;
it cannot prove a real editor accepted text or identify every terminal/password field.
After building, `python3 tools/test-x11.py --app target/release/amber-dictation` also
smoke-tests startup, closing, and reopening the application.

CI and these checks do **not** replace clean installation, real microphone/keyring,
clipboard/focus, lock/suspend/resume, and native Wayland/XWayland checks on target desktops.

<details>
<summary>Arch source snapshots and native build troubleshooting</summary>

To prepare the current source on a non-Arch machine with Git and Node.js installed:

```bash
bash tools/arch.sh --prepare-only /tmp/amber-arch-source
```

The destination must be empty and outside the checkout. Transfer that directory to Arch
and run `makepkg --syncdeps --check --install` inside it as a regular user. The generated
PKGBUILD pins the archive SHA-256; repository metadata, build outputs, dependency trees,
and common signing-secret files are excluded. This is not an AUR upload or release process;
source/runtime/model redistribution still requires the appropriate permissions.

The helper prints and retains its staging directory so failed builds can be inspected.
It uses a fresh native build directory. When changing `GGML_*` CPU flags in an existing
build directory, run `cargo clean -p whisper-rs-sys` first: the upstream build script does
not reliably track those environment changes.

</details>

<details>
<summary>Repository layout and development conventions</summary>

```text
crates/core/      platform-independent settings, providers, cleanup, downloads, tests
crates/inference/ shared local Whisper inference
crates/onnx/      shared Parakeet and Moonshine inference
src-tauri/        desktop application and platform-specific integration
src/             React settings, Recorder, Testing, and indicator UI
packaging/arch/   native Arch package recipe and desktop entry
tools/           build, packaging, and integration-test helpers
server/           separate standalone server, not the Linux desktop phone API
streamdeck/       retained upstream Windows Stream Deck integration
```

Keep platform-independent logic in `crates/core` and platform API work in `src-tauri`.
Keep `package.json` and the workspace version in `Cargo.toml` equal. The retained
[standalone server](server/README.md) is separate from the Linux desktop. The inherited
[roadmap](docs/PLAN.md) includes Windows/server plans, not promises of Linux support.

</details>

## License and Attribution

Amber was created by [EinFabo](https://github.com/EinFabo). This is a derivative of
[Amber for Windows](https://github.com/EinFabo/amberpc), not a clean-room rewrite or an
endorsed upstream Linux release. The original application, design, artwork, and retained
source come from Amber. Original Git history is preserved, with Linux development
following upstream commit
[`64d67e3`](https://github.com/EinFabo/amberpc/commit/64d67e308511cb922e1de957d7896b1cdd56e42d).

The original [LICENSE](LICENSE) says **Copyright (c) 2026 EinFabo. All rights reserved.**
Amber is **not Apache-licensed**. It requires prior written permission to modify or
redistribute the source, artwork, or a modified version. The limited GitHub viewing/forking
allowance is not a general license to publish this port, and attribution does not grant
those rights. This README asserts no permission or endorsement. Retain the license and
copyright notices; permission requests go to the [original project's issues](https://github.com/EinFabo/amberpc/issues).
Third-party libraries, runtimes, and models keep their own licenses, which do not relicense Amber.

<details>
<summary>Maintainer publication and release cautions</summary>

[Publishing to GitHub](docs/github-publishing.md) describes the permission gate and
publication precautions. Its remote/setup examples reflect an earlier checkout;
inspect the current repository rather than applying them blindly.

The inherited CI can upload Linux package artifacts, and the server workflow can publish
releases on matching tags. Both source publication and binary redistribution require
appropriate permission. An enabled workflow is not evidence of that permission.

Do **not** use `tools/release.sh` for this Linux port: it still targets the original
creator's Windows repository. The server-release workflow publishes to its current
repository, but the server installers still default to upstream downloads. Neither is
a ready-to-use Linux desktop release process.

</details>
