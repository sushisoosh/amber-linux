# Amber for Windows

## Linux Desktop Preview

This worktree also contains a Linux desktop port targeting Arch/i3, GNOME and KDE. It is a
preview until the real-desktop integration matrix has passed, not a claim of
Windows feature parity. Upstream source, artwork, and redistribution remain subject
to the repository license and the author's written permission.

- Local CPU Whisper inference is built in; only its model weights need downloading.
  Parakeet and Moonshine additionally download a verified ONNX Runtime on first setup.
  Hosted recognition and optional local/server cleanup are supported.
- Dictation shortcuts leave Amber in the background. The Linux Recorder page keeps results
  in memory for review and explicit Copy by default. Under **Delivery**, enable **Copy and
  paste completed dictation (Ctrl+V)** to copy completed text and paste automatically on
  supported X11 targets. The target must remain unchanged for automatic paste; unsupported
  or changed targets receive clipboard text for manual paste, without refocusing a window.
  On X11, `amber-dictation --insert-last` can also submit a single-line
  result to a supported native editor; a submitted paste is not proof the editor accepted it.
  Browsers, terminals, dialogs, unknown targets, and multiline text still require manual paste.
- Wayland shortcuts use the desktop portal when available. GNOME 48+ and KDE Plasma
  6 are the primary targets. On older/missing portal backends, configure a desktop
  shortcut for `amber-dictation --toggle`; `amber-dictation --cancel` stops/cancels a dictation. That
  fallback is toggle-only. On Wayland, clipboard access requires Amber to be focused.
- Credentials and optional history use authenticated encryption with a key in Secret
  Service (GNOME Keyring or a compatible wallet). A missing/locked wallet never
  causes plaintext persistence. Credential-save errors leave keys available only
  for the current session. History, statistics, and login startup are opt-in.
- Phone/tunnel/control servers, automatic terminal-command mode, Discord mute, automatic
  per-app profiles, the Windows updater, and the Windows floating bubble are disabled
  in the Linux desktop. Updates are installed from Linux packages.

### Arch/i3

Build a native package from this checkout, after transferring the port through your
own repository. The helper includes current uncommitted source files; it does **not**
clone the unported upstream repository or require a hardcoded fork URL.

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
Quit button must remain usable without them. Explicit paste is intentionally
restricted to supported native editors and single-line text; terminals, browsers
and other unverified targets use Copy and manual paste.

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
sudo apt-get install build-essential cmake pkg-config clang libclang-dev \
  libwebkit2gtk-4.1-dev libgtk-3-dev libasound2-dev libayatana-appindicator3-dev \
  librsvg2-dev libxdo-dev libxtst-dev libssl-dev libdbus-1-dev rpm
npm ci
npm run build
npm run test:ui
cargo test -p amber-core -p amber-inference -p amber-server -p amber-pc --locked
npm run app
npm run app:build -- --bundles deb,rpm
```

The Linux Tauri configuration is selected automatically. `.deb`/`.rpm` outputs are
under `target/release/bundle/`. Model tests marked ignored require explicitly
provided model files; ordinary tests do not download models or contact paid APIs.
CI builds/tests/packages are not substitutes for microphone, keyring, clipboard,
focus, suspend/revocation, native Wayland/XWayland, and clean-install testing on real
GNOME/KDE sessions. Wayland automatic text injection is not implemented; use review,
Copy, and manual paste. GPU/ARM/Flatpak/AppImage support is deferred.

The remainder of this README describes the upstream Windows application.

Dictation into **any** window - Amber's Android dictation, on the PC. Speak, and the text
appears where your cursor is: in Notepad, the browser, Discord, VS Code, Word.

Local-first: your voice is recognized on this PC by whisper.cpp (CPU or NVIDIA GPU),
Parakeet or Moonshine (CPU), or by a server with your own API key. Local model setup
downloads files; recognition itself stays offline.

## Installing

Download `Amber_<version>_x64-setup.exe` from the
[latest release](https://github.com/EinFabo/amberpc/releases/latest) and run it. It installs
for your Windows account only - no administrator rights - into
`%LOCALAPPDATA%\Amber`, with a Start menu entry. Windows SmartScreen may warn on first run
because the installer is not code-signed: *More info → Run anyway*.

On first start a short setup guide picks the engine and downloads what it needs. Amber
then starts with Windows (in the tray, without a window - switch it off under
*Settings → App*) and updates itself from the releases. Uninstall it under *Windows
Settings → Apps*; that also removes the autostart entry, and optionally the downloaded
models.

## Using it

- **Hotkey** (default `Ctrl+Alt+Space`), in three modes: *tap or hold* (a tap starts, the
  next tap stops; holding dictates until you let go), *tap to toggle*, *hold to talk*.
- **The bubble** at the screen edge: click to start and stop, hold to talk, drag to move it
  (it settles at the nearer edge of whichever monitor you drop it on), right-click for the
  settings. It never takes the focus from the window you are writing in.
- **Tray icon**: settings, start/stop, show/hide the bubble, copy the last dictation,
  recognize a failed recording again, updates, quit.
- **Stream Deck** key: hold to talk (see below).

The look is Amber's fire: a flat amber disc with a flame at rest, level bars and sparks
while listening, a rim that fills against the expected wait while recognizing, a tick when
the text is in, red when something failed. The bubble keeps out of full-screen programs
(games, videos) and out of the programs you list; the hotkey works there anyway.

Say "neue Zeile", "neuer Absatz", "Komma", "Fragezeichen", "Punkt" (at a sentence end) - or
"new line", "comma", "question mark" - and the marks go in. A **snippet** phrase on its own
("meine Adresse") inserts its saved text.

## Engines

| Engine | Where your voice goes | Notes |
|---|---|---|
| Local, CPU | nowhere | whisper.cpp `b5454`, 8 MB |
| Local, NVIDIA GPU | nowhere | CUDA 12.4 build with cuBLAS, 653 MB |
| Parakeet, CPU | nowhere | v3 int8: 25 languages including German; v2 int8: English only |
| Moonshine, CPU | nowhere | Tiny/Base int8, English only; long recordings are chunked |
| Groq | Groq | Whisper large-v3(-turbo), about $0.04 per hour of speech |
| OpenAI | OpenAI | GPT-4o (mini) Transcribe, Whisper |
| Tinfoil.sh | Tinfoil | Voxtral Small 24B (default), Whisper large-v3-turbo; HTTPS without client enclave verification |
| Mistral | Mistral | Voxtral Mini Transcribe |
| Deepgram | Deepgram | Nova-3; native transcription API |
| ElevenLabs | ElevenLabs | Scribe v2; no speaker labels or paid vocabulary add-on |
| Own server | your server | anything with `POST /v1/audio/transcriptions`; plain `http://` only inside your network |

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

The recommended Whisper models are the ones Amber for Android uses: **FUTO Base** and **FUTO Small**
(keyboard.futo.org) and **ACFT Large v3 Turbo q8_0** (DeadBranches on Hugging Face, from
MahmoudAshraf's Apache-2.0 fine-tune). All three are tuned so the encoder stops where the
speech stops - Amber sends whisper-server a matching `audio_ctx` per request instead of a
30-second window; on Android that made the large model 4x faster under 8 s of speech, and
more accurate. Amber suggests one by hardware (NVIDIA → Large v3 Turbo, a CPU with room →
Small, an old office PC → Base) and learns how long recognition takes on this PC.

Long dictations are **recognized while you speak**: cut at real pauses, each finished part
recognized while you keep talking, so only the last part is left after you stop.

Runtimes and models are downloaded on demand from those sources, each pinned by URL (Hugging
Face at a fixed commit) and SHA-256 in `crates/core/src/local.rs`; a runtime archive missing
a required DLL is refused. A file with the wrong checksum is deleted, never run. The local
engine runs as `whisper-server.exe` on `127.0.0.1` only, in a Windows job object, so it never
outlives Amber.

The **Engine > Model library** also offers stock Whisper Tiny, Base, Small, Medium,
Large v3 and Turbo variants, including compact Q5, Q8 and F16 weights and selected
English-only variants. These use the full encoder context; the tuned dictation defaults
remain unchanged. **Use downloads when finished** can be switched off to download without
changing the selected model. Downloads remain cancellable and checksum-verified.

Parakeet and Moonshine use `transcribe-rs` with CPU ONNX Runtime 1.24.4. Their first download
also fetches a checksum-pinned Microsoft native runtime package (about 119 MiB); only the
platform libraries and license notices are installed. No Python or .NET setup is needed.
Windows additionally requires the Microsoft Visual C++ 2015-2022 x64 redistributable.
The supplied Linux runtime targets x64 GNU/Linux with glibc 2.27 or newer and a compatible
system C++ runtime. Parakeet v3 detects its 25 supported languages automatically; v2 and
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

With a server engine, a server that cannot be reached (offline, down) hands the recording to
the downloaded local model with a notice - never the other way round.

API keys are encrypted with Windows DPAPI for your account (`secrets.bin`), never written
to the settings file or the log.

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
unlocked Secret Service wallet; Windows uses account-bound DPAPI encryption. A failed save
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

Between recognition and inserting (*Settings → Cleanup*):

- **By rule** - local, instant, nothing invented: spoken punctuation and layout; filler words
  (äh, ähm, öhm, hm, uh, erm …; German words that look like English fillers, "um" and "eh",
  stay); German commas before "dass", "weil", "wenn", "um … zu", "sondern" (Amber for
  Android's rules, 98.5 % right on 4,200 sentences).
- **Smart cleanup** (optional): a language model resolves self-corrections ("um drei, nee
  stopp, um vier" → "um vier"), false starts and punctuation. Locally with llama.cpp
  `b11981` and Qwen3 4B Instruct 2507 (Apache 2.0; CUDA 641 MB + model 2.4 GB, ~3 GB of
   graphics memory), or through Groq, OpenAI, Tinfoil.sh, Mistral, OpenRouter, Google Gemini,
   DeepSeek, Anthropic, local Ollama or an own server. Keys are managed in Credentials.
  The prompt puts hard rules first - the dictation's language (named when one is chosen or
  clear from its words), every negation, number, name and condition kept, never an answer -
  then what to fix, self-corrections ("um drei, nee, um vier" → "um vier"), the style with
  what it may not change, and the program's own instruction, fenced: tone and form only. A
  few example dictations go before the real one. A reply is thrown away and the text goes
  in as recognized when it is cut off, empty, of implausible length, made of words that were
  not dictated, in another language, or has lost or gained a negation or a number (what a
  spoken self-correction took back may go) - the log names the reason. Measured with
  `tools/cleanup-eval` (76 cases, Qwen3 4B): the prompt of 0.4.6 got 7 wrong and 5 of them
  through; this one gets 1 wrong and none through. A dictation never waits for a model that
  is still loading.
- **Per program** (*Settings → Programs*): a writing style - as spoken, casual (chats),
  formal (mail), concise (notes) - an own instruction, and the smart cleanup as set, always
  or never. The program is the window the dictation started in.

Provider lists distinguish speech recognition from cleanup. Named providers offer curated
model presets; Own server accepts a model ID, but compatibility with arbitrary model
parameters is not guaranteed. The Ollama preset connects to `http://127.0.0.1:11434/v1` without a key;
install Ollama and pull the chosen model separately. Use Own server for a different address.
Connection checks do not submit inference requests and do not prove that a selected model
or API key can perform paid inference. Anthropic uses its native Messages protocol while
Amber's phone API remains OpenAI-compatible.

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
Shell output is always review/copy-only, never automatically pasted or executed, even if
terminal auto-run is enabled. Choose Text output for transformations meant for normal
delivery, such as drafting an email. Rule prompts are ordinary settings, not secret storage.

Rules require both their own Enabled switch and global smart cleanup; a program's Never
setting still prevents them. Matching happens before filler cleanup: `please bash...`,
`bashful...` and quoted keywords do not trigger the Bash rule. Failure keeps the original
request, including its keyword. Enabled rules defer streaming chunks so they apply once to
the complete utterance. Testing/replay and the explicit preview support them; phone API
cleanup does not apply desktop instruction rules. Ordinary cleanup retains its existing
meaning-preservation checks.

## Terminal commands

In a terminal, what is said can be a request instead of text (*Settings → Programs*): "such
mir alle Dateien, die mit CL anfangen" becomes `find . -name 'CL*'`, "list the five biggest
files in my downloads folder" becomes `Get-ChildItem ~\Downloads -File | Sort-Object Length
-Descending | Select-Object -First 5`. Amber **types the command at the prompt and never runs
it** - no Enter, no line break; you read it and press Enter (unless the danger zone below
says otherwise).

- **When**: automatically in terminal programs (Windows Terminal, PowerShell, Git Bash,
  PuTTY, WSL - per program on, off, or a fixed shell; or not automatically at all); anywhere
  with the command hotkey (*Settings → Hotkeys*); or for one dictation that starts with a
  start word ("Befehl: ...", "Command: ...").
- **Which shell**: bash or PowerShell, told by the program and the window title
  ("fabi@pc: ~/projects", "Windows PowerShell", "Ubuntu"); cmd is not supported. The title is
  the only context - nothing of the terminal's content is read.
- **The model**: the cleanup's (local Qwen3 4B, or the provider's), also while the smart
  cleanup is off; examples of each shell in the prompt. When what was said is no command - a
  message to Claude Code or an editor running in the terminal - it is dictated as usual;
  titles of Claude Code and Vim skip the model.
- **Risky commands** (deleting, overwriting, `sudo`, a script piped into a shell, forcing
  git, disks, shutting down) come commented out in *safe mode* (the default: `# rm -rf build`
  - Enter does nothing, remove the `#` to run it); without it they come as they are, with a
  warning. Amber never runs a risky command by itself.
- **Danger zone** (red, on the Programs page): *safe mode* off, and running commands by
  themselves (off by default, switched on only after a confirmation) - Amber presses Enter
  itself, so a harmless command runs at once. A risky one never does.
- The bubble shows `>_`; the history keeps "request → command".

## History and statistics

The last dictations - text, program, engine - are kept on this PC, encrypted for the
Windows account, for 7 days by default (*Settings → History*); copy one to put it where it
should have gone. Switching the history off deletes it. A recording whose recognition failed
stays in memory for ten minutes: *Recognize again* (tray or History page) puts the text on
the clipboard. The statistics count words and timings only, never the text: today, the
streak, time saved against typing at 40 words a minute, programs, the wait per engine.

History supports text/program search and combined program, local-date and insertion-status
filters. Search terms stay in memory only. **Delete all history** always clears the entire
history, including entries hidden by the current filters; the UI makes this scope explicit.

## Stream Deck

`com.einfabo.amber.streamDeckPlugin` (attached to every release; double-click installs it)
adds a *Dictate* key: hold it while you speak, let go to insert - or tap to start and stop.
The key shows the state (flame, listening, working, failed; grey while Amber is not
running). It talks to Amber through a control port on 127.0.0.1 only, with a fresh token
every start, handed over in `%APPDATA%\app.amber.pc\streamdeck.json`.

**Muted in Discord while dictating** (Microphone page; any dictation, not only the Stream
Deck): while Amber listens it presses Discord's own microphone switch - found through UI
Automation, pressed with a click sent to Discord's window as a message, which cannot take
the focus from the game or program in front - and presses it again at the stop. Amber only
takes back its own mute: not one the user set, not while they are deafened; an unmute that
does not take (Discord minimized, closed) is tried again until it does. Nothing to set up
in Discord. Not while Discord is minimized or in the tray, not for dictations into Discord
itself, and only with Discord in English or German (the switch is found by its name).

```bash
cd streamdeck && npm run setup && npm test && npm run pack   # -> dist/
```

## Phone API (optional)

Amber on the Android phone can use this PC as its server - same network, the PC's GPU,
no change to the phone app. Turn it on under *Settings → Phone*; it shows what to enter in
the phone's *Engine → Own server*, as with OpenAI: the **address** (e.g.
`http://192.168.178.30:8765/v1`) and, apart, the **API key** (hidden until *Show*), each with
a copy button - or both at once as a pairing link in a QR code
(`amber://server?url=…&key=…`, Amber for Android 0.30.0 or newer).

- OpenAI-compatible: `POST /v1/audio/transcriptions` (multipart, WAV or Ogg Opus) answers
  `{"text": …}`; `GET /v1/models` lists every downloaded speech and cleanup model and the
  cloud models set in the app, the PC's own as the default - the phone picks among them, and
  the PC loads the one it names (one local model at a time). See `docs/phone-api.md`.
- `POST /v1/chat/completions` answers the phone's own cleanup (Amber for Android 0.17 and
  later asks for it separately, with its styles per app) with the cleanup model it names -
  local or the provider under Cleanup - or the PC's own.
- The key goes as `Authorization: Bearer`; the older form with the key in the path
  (`/<key>/v1`, Amber for Android up to 0.29.0) is still accepted, and never logged. Several
  keys, one per device or person, each with the models it may use and its requests a day
  (*Phone → Access keys*); a new key or removing one locks out only its devices.
- Only devices in the private network (192.168/16, 10/8, 172.16/12, Tailscale's 100.64/10,
  loopback) are answered. Off by default; Windows asks once to let Amber through its
  firewall.
- Silence answers `{"text": ""}`, which the phone's *Test connection* counts as success.
- The PC's cleanup applies to the phone's dictations too (switch on the Phone page): fillers
  out, and the smart cleanup if it is on - for phone apps without their own cleanup. Off when
  the phone cleans up through the PC itself, or the text is cleaned twice.
- **From anywhere**: a Cloudflare Tunnel on the user's own domain. The user creates the
  tunnel in Cloudflare (public hostname -> `http://localhost:<port>`) and pastes its token;
  Amber downloads cloudflared (pinned, checksum-verified), keeps it running with the token in
  its environment (never on the command line), and shows the address `https://<host>/v1` and
  the pairing link as a QR code. Requests arrive from 127.0.0.1 through cloudflared; the access key keeps strangers
  out, and refusals are logged at most every 10 s with the sender Cloudflare names.
  One command does all of it from WSL: `tools/setup-tunnel.sh amber-x7k2.example.com` logs
  in to Cloudflare (browser, once), creates the tunnel, routes the hostname, and hands token
  and hostname to Amber (`amber.exe --import-tunnel <file>`, the file deleted on reading)
  with a new access key.

## Amber Server

The same phone API without the desktop app, for a PC, a NAS or a small Linux box: one
command installs it (Linux or Windows, CPU or Vulkan GPU), sets it up with the model that
suits the machine and starts it as a service; every setting and model has a command, and the
phone chooses among the models it has downloaded.

Linux:

```sh
curl -fsSL https://github.com/EinFabo/amberpc/releases/download/server-latest/install-server.sh | sh
```

Windows (PowerShell, no admin rights needed):

```powershell
irm https://github.com/EinFabo/amberpc/releases/download/server-latest/install-server.ps1 | iex
```

Run either again to update. Then `amber-server url` shows the phone's address and QR code;
`amber-server key add <name>` gives another device its own key (with its models and requests a
day), and `amber-server tunnel setup <hostname>` reaches the server from anywhere through a
Cloudflare Tunnel. Every command and setting: [server/README.md](server/README.md).

## Inserting

The text goes into the window the dictation started in. If another program is in front by
the time the text is ready, Amber does not type into it: the text goes on the clipboard with
a notice. Amber waits until the hotkey's modifier keys are released, then:

- **Paste** (default): clipboard + `Ctrl+V`, the previous clipboard text restored after
  400 ms. Dictations are kept out of the clipboard history (`Win+V`).
- **Type**: Unicode keystrokes via `SendInput`; the clipboard stays untouched. A line break
  is Shift+Enter, so a chat does not send it.

Windows blocks synthetic input into windows running as administrator. Amber checks for that
up front and leaves the text on the clipboard with a note, instead of pretending it typed.

## Layout

```
crates/core/   pure Rust: audio maths, engines, cleanup, settings, downloads, gestures,
               the hardened HTTP server behind the phone API and the control port  → tests
streamdeck/    the Stream Deck plugin (TypeScript, Elgato SDK v2)
src-tauri/     the Windows app: capture, dictation, insertion, whisper process, bubble
src/           web UI: settings window (React), bubble (canvas)
tools/win.sh   mirrors the tree to %USERPROFILE%\build\amberpc and builds there
```

Same rule as Amber on Android: **whatever can go wrong without a platform API belongs in
`core`**, where it is tested without Windows.

## Building

Release builds started from the build folder (`target\release`) never register themselves for
autostart - only the installed app does.

```bash
cargo test -p amber-core        # anywhere, also in WSL
tools/win.sh install            # npm ci on the Windows side
tools/win.sh check              # cargo check against the real Windows APIs
tools/win.sh test               # all tests on Windows
tools/win.sh build              # signed NSIS installer in target/release/bundle/nsis
tools/win.sh dev                # tauri dev
```

The updater key lives outside the repo in `~/.amberpc/` (`updater.key`,
`updater.password`); `tools/win.sh build` passes it to the build.

Data: `%APPDATA%\app.amber.pc\` (settings, keys), `%LOCALAPPDATA%\app.amber.pc\` (runtimes,
models, `history.bin`, `stats.jsonl`, `estimates.json`, `logs\amber.log`).

## Releases and updates

`tools/release.sh "notes"` runs the tests, builds and signs the installer on this PC and
publishes it as a GitHub release together with `latest.json`. The signing key stays in
`~/.amberpc/` and never goes to GitHub. Bump `version` in `package.json` and `Cargo.toml`
first.

Installed copies check
`github.com/EinFabo/amberpc/releases/latest/download/latest.json` at start and every six
hours, download in the background, and install once no dictation has run for a minute.
GitHub Actions (`.github/workflows/ci.yml`) runs the tests and a Windows build on every push.

## System requirements

Windows 10 1809 or newer, x64. WebView2 (the installer fetches it if missing). For the GPU
engine an NVIDIA card from the GTX 900 series on with a current driver.

## Not in yet

Tinfoil's enclave verification, configurable idle model unloading, and GPU acceleration for
the new ONNX speech engines are not implemented. The roadmap is `docs/PLAN.md`.
