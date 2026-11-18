//! Audio in, text out. HTTP adapters for OpenAI-compatible providers and the Windows local
//! whisper.cpp helper. Embedded backends implement the same [Transcriber] contract.

use std::time::Duration;
use std::sync::{Arc, OnceLock, atomic::{AtomicBool, Ordering}};

use reqwest::{multipart, Client};
use crate::settings::UploadFormat;

/// Per-call file payload metadata, including failed attempts. Not a count of socket bytes:
/// an interrupted request may have sent only part of the file; multipart overhead is excluded.
/// Zero upload bytes means no audio request was attempted; `note` explains the decision.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct UploadStats {
    pub format: String,
    pub input_bytes: u64,
    pub upload_bytes: u64,
    pub encoding_ms: u64,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TranscribeOptions {
    /// ISO 639-1 code, several of them comma-separated ("de,en": detect among these), or None
    /// to let the engine detect any language.
    pub language: Option<String>,
    /// Names and jargon the engine should expect.
    pub prompt: Option<String>,
}

impl TranscribeOptions {
    /// [language]: "auto", a code ("de", "de-DE") or several ("de, en-US").
    pub fn new(language: &str, vocabulary: &str) -> Self {
        let mut codes: Vec<String> = Vec::new();
        for code in language.split(',').map(|c| c.trim().split(['-', '_']).next().unwrap_or("").to_lowercase()) {
            if !code.is_empty() && code != "auto" && !codes.contains(&code) {
                codes.push(code);
            }
        }
        let language = (!codes.is_empty()).then(|| codes.join(","));
        let prompt = Some(vocabulary.trim().to_string()).filter(|p| !p.is_empty());
        TranscribeOptions { language, prompt }
    }

    /// The languages to choose among - empty for any language.
    pub fn among(&self) -> Vec<String> {
        self.language.as_deref().map(|l| l.split(',').map(str::to_string).collect()).unwrap_or_default()
    }

    /// The one language, when exactly one is set.
    pub fn fixed(&self) -> Option<&str> {
        self.language.as_deref().filter(|l| !l.contains(','))
    }
}

/// Up to this much audio, a dictation is short: whisper's language guess on a second or so is
/// weak, and a wrong guess costs much more one way than the other. Measured on whisper-server
/// with Large v3 Turbo (ACFT), German and English phrases of 0.5-1.1 s (TTS at 1.5-2x speed,
/// Fabi's failures were 0.9-1.4 s): German was taken for English with p(en) 0.29-0.47 and
/// p(de) 0.03-0.47, and run again in English it came back translated ("Okay, danke." ->
/// "Okay, thank you.", "Ich hab kein Wasser." -> "I have no water."); short English run in
/// German stayed English ("See you later.", "No problem.", "Sounds good."). From about 2 s on
/// every phrase was detected clearly (the right language at 0.87 or more, mostly 0.99).
pub const SHORT_DICTATION_MS: u64 = 3_000;

/// Another language than the first chosen needs at least this probability ("more likely than
/// not") - at any length: Fabi's log of 2026-11-07, 372 picks, had long dictations at 0.9 or
/// more but one, a 3.7 s first part at 0.12 against 0.10 that turned German into English...
pub const CLEAR_LANGUAGE: f64 = 0.5;

/// ...and in a short dictation this many times the first one's. English phrases cleared both
/// at 0.64-1.0; the German ones taken for English stayed below 0.47.
pub const CLEAR_MARGIN: f64 = 2.0;

/// Of [allowed], the language to recognize [audio_ms] of speech in, by whisper.cpp's
/// [probabilities] (`{"de": 0.2, ...}`): the likeliest of them when Whisper is fairly sure
/// ([CLEAR_LANGUAGE]) - in a short dictation the first chosen unless another is clearly
/// likelier ([SHORT_DICTATION_MS]). The first, too, when it
/// heard none of them or two equally. Short German speech is easily taken for Turkish; among
/// German and English only, it can no longer be.
pub fn pick_language(probabilities: &serde_json::Value, allowed: &[String], audio_ms: u64) -> String {
    let p = |code: &str| probabilities.get(code).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let first = allowed.first().cloned().unwrap_or_default();
    let mut best = first.clone();
    for code in allowed {
        if p(code) > p(&best) {
            best = code.clone();
        }
    }
    // Below CLEAR_LANGUAGE Whisper is guessing at any length; in a short dictation another
    // language must also be clearly ahead of the first.
    let unsure = p(&best) < CLEAR_LANGUAGE;
    let behind = audio_ms <= SHORT_DICTATION_MS && p(&best) < CLEAR_MARGIN * p(&first);
    let picked = if unsure || behind { first } else { best };
    if allowed.len() > 1 {
        note(language_note(probabilities, allowed, audio_ms, &picked));
    }
    picked
}

/// The log line for a pick: length, the chosen languages' probabilities, the pick - never the
/// text.
pub fn language_note(probabilities: &serde_json::Value, allowed: &[String], audio_ms: u64, picked: &str) -> String {
    let each: Vec<String> = allowed.iter().map(|c| format!("{c} {:.2}", probabilities.get(c).and_then(|v| v.as_f64()).unwrap_or(0.0))).collect();
    format!("language: {:.1} s of audio, among {}: {} -> {picked}", audio_ms as f64 / 1000.0, allowed.join(","), each.join(", "))
}

/// The language a cloud service is sent: the one chosen, or for a short dictation the first
/// of several - it would guess freely, and a short German phrase guessed as English comes
/// back translated. A long one it detects itself.
pub fn cloud_language(options: &TranscribeOptions, audio_ms: u64) -> Option<String> {
    match options.fixed() {
        Some(one) => Some(one.to_string()),
        None if audio_ms <= SHORT_DICTATION_MS => options.among().into_iter().next(),
        None => None,
    }
}

static LANGUAGE_LOG: std::sync::OnceLock<Box<dyn Fn(String) + Send + Sync>> = std::sync::OnceLock::new();

/// Where the language picks are logged - the desktop's log, the server's.
pub fn log_languages_to(log: impl Fn(String) + Send + Sync + 'static) {
    let _ = LANGUAGE_LOG.set(Box::new(log));
}

fn note(line: String) {
    if let Some(log) = LANGUAGE_LOG.get() {
        log(line);
    }
}

/// Milliseconds of audio in a 16 kHz 16-bit mono WAV as Amber writes it (44-byte header).
pub fn wav_ms(wav: &[u8]) -> u64 {
    (wav.len().saturating_sub(44) / 2) as u64 * 1000 / 16_000
}

/// One dictation's cancellation, never reset or reused for another run. Native model loading
/// may not be interruptible; callers must still serialize work and discard stale results.
#[derive(Clone, Default, Debug)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn cancel(&self) { self.0.store(true, Ordering::Release); }
    pub fn is_cancelled(&self) -> bool { self.0.load(Ordering::Acquire) }
    pub fn check(&self) -> Result<(), SttError> {
        if self.is_cancelled() {
            Err(SttError::new(ErrorKind::Cancelled, "Dictation cancelled.", "cancelled"))
        } else {
            Ok(())
        }
    }
}

pub trait Transcriber: Send + Sync {
    fn transcribe(&self, wav: &[u8], options: &TranscribeOptions) -> Result<String, SttError>;

    /// The text and, where the engine says so, the language it detected (Whisper's name,
    /// "german") - for keeping one language across the parts of a long dictation.
    fn transcribe_detecting(&self, wav: &[u8], options: &TranscribeOptions) -> Result<(String, Option<String>), SttError> {
        self.transcribe(wav, options).map(|t| (t, None))
    }

    /// Backends override this to stop work in flight. The default only prevents starting or
    /// accepting cancelled work; it cannot interrupt an arbitrary legacy implementation.
    fn transcribe_cancellable(&self, wav: &[u8], options: &TranscribeOptions, cancel: &CancellationToken) -> Result<String, SttError> {
        cancel.check()?;
        let result = self.transcribe(wav, options);
        cancel.check()?;
        result
    }

    /// Metadata belongs to this call, including failures, never to a shared "last upload".
    /// Local backends do not upload a compressed copy and need no metadata.
    fn transcribe_with_upload(&self, wav: &[u8], options: &TranscribeOptions, cancel: &CancellationToken) -> (Result<String, SttError>, Option<UploadStats>) {
        (self.transcribe_cancellable(wav, options, cancel), None)
    }

    fn transcribe_detecting_cancellable(&self, wav: &[u8], options: &TranscribeOptions, cancel: &CancellationToken) -> Result<(String, Option<String>), SttError> {
        cancel.check()?;
        let result = self.transcribe_detecting(wav, options);
        cancel.check()?;
        result
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Cancelled,
    Unauthorized,
    RateLimited,
    TooLarge,
    Unreachable,
    Timeout,
    /// The server answered 5xx: down or overloaded - like no connection, worth a fallback.
    Unavailable,
    Server,
    BadResponse,
}

impl ErrorKind {
    /// Failures another engine could fix: no connection, a timeout, a server that is down. A
    /// refused key or a bad request needs the user, not a detour.
    pub fn service_unavailable(self) -> bool {
        matches!(self, ErrorKind::Unreachable | ErrorKind::Timeout | ErrorKind::Unavailable)
    }
}

/// [message] is for the user, [detail] for the log.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message} ({detail})")]
pub struct SttError {
    pub kind: ErrorKind,
    pub message: String,
    pub detail: String,
}

impl SttError {
    pub fn new(kind: ErrorKind, message: impl Into<String>, detail: impl Into<String>) -> Self {
        SttError { kind, message: message.into(), detail: detail.into() }
    }
}

pub(crate) fn client(timeout: Duration, base_url: &str) -> Client {
    let mut builder = Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .user_agent(concat!("AmberPC/", env!("CARGO_PKG_VERSION")));
    if reqwest::Url::parse(base_url).ok().is_some_and(|u| crate::providers::is_loopback_host(u.host_str())) {
        // A local helper's audio/key must not be forwarded to an environment-configured proxy.
        builder = builder.no_proxy();
    }
    builder.build().unwrap_or_else(|_| panic!("Could not initialize HTTP client."))
}

pub(crate) const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_CATALOG_BYTES: usize = 512 * 1024;

pub(crate) fn validate_endpoint(url: &str) -> Result<(), SttError> {
    crate::providers::check_base_url(url)
        .map(|_| ())
        .map_err(|message| SttError::new(ErrorKind::Server, message, "endpoint rejected"))
}

/// The synchronous engine API runs on a blocking worker. A shared async transport lets
/// cancellation drop an in-flight upload/read instead of leaving a detached HTTP worker.
pub(crate) fn request(http: &Client, request: reqwest::RequestBuilder, limit: usize, cancel: &CancellationToken) -> Result<(u16, Vec<u8>), SttError> {
    cancel.check()?;
    let request = request.build().map_err(network_error)?;
    validate_endpoint(request.url().as_str())?;
    static HTTP_RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    let runtime = HTTP_RUNTIME.get_or_init(|| tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().expect("HTTP runtime"));
    let result = runtime.block_on(async {
        let response = async {
            let mut resp = http.execute(request).await.map_err(network_error)?;
            let status = resp.status().as_u16();
            // Error bodies are neither needed nor trusted. Do not download or log them.
            if !(200..300).contains(&status) { return Ok((status, Vec::new())); }
            let too_large = || SttError::new(ErrorKind::BadResponse, "The engine's answer was too large.", format!("response exceeds {limit} bytes"));
            if resp.content_length().is_some_and(|n| n > limit as u64) { return Err(too_large()); }
            let mut body = Vec::new();
            while let Some(chunk) = resp.chunk().await.map_err(network_error)? {
                if chunk.len() > limit.saturating_sub(body.len()) { return Err(too_large()); }
                body.extend_from_slice(&chunk);
            }
            Ok((status, body))
        };
        let cancelled = async {
            loop {
                if let Err(error) = cancel.check() { break error; }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::select! {
            biased;
            error = cancelled => Err(error),
            result = response => result,
        }
    });
    cancel.check()?;
    result
}

pub(crate) fn response_json(status: u16, body: &[u8]) -> Result<serde_json::Value, SttError> {
    if !(200..300).contains(&status) { return Err(status_error(status, "")); }
    serde_json::from_slice(body).map_err(|e| json_error(&e, body.len()))
}

/// OpenAI-compatible providers: `POST {base}/audio/transcriptions`.
pub struct OpenAiTranscriber {
    base_url: String,
    api_key: String,
    model: String,
    http: Client,
    discover: bool,
    catalog: OnceLock<Option<crate::providers::AmberCatalog>>,
    upload_format: UploadFormat,
}

impl OpenAiTranscriber {
    pub fn new(base_url: &str, api_key: &str, model: &str) -> Self {
        OpenAiTranscriber {
            base_url: base_url.trim().trim_end_matches('/').to_string(),
            api_key: api_key.trim().to_string(),
            model: model.to_string(),
            http: client(Duration::from_secs(90), base_url.trim()),
            discover: false,
            catalog: OnceLock::new(),
            upload_format: UploadFormat::Wav,
        }
    }

    pub fn with_upload_format(mut self, format: UploadFormat) -> Self {
        self.upload_format = format;
        self
    }

    /// Enable only for custom endpoints. No paid requests or alternate destinations are
    /// probed. A catalog is cached only for this immutable URL/key pair.
    pub fn with_catalog_discovery(mut self) -> Self {
        self.discover = true;
        self
    }

    pub fn catalog(&self) -> Result<Option<crate::providers::AmberCatalog>, SttError> {
        self.discover_catalog(&CancellationToken::default())
    }

    fn discover_catalog(&self, cancel: &CancellationToken) -> Result<Option<crate::providers::AmberCatalog>, SttError> {
        cancel.check()?;
        if let Some(catalog) = self.catalog.get() { return Ok(catalog.clone()); }
        let (status, body) = self.models(cancel)?;
        // Some ordinary compatible servers have no catalog. Other failures do not justify
        // uploading audio to a service whose identity could not be established.
        let catalog = if matches!(status, 404 | 405) {
            None
        } else {
            let root = response_json(status, &body)?;
            let catalog = crate::providers::AmberCatalog::parse(&root);
            if catalog.is_none() && !root.get("data").is_some_and(|v| v.is_array()) {
                return Err(SttError::new(ErrorKind::BadResponse, "The server did not send a model catalog.", "not a model list"));
            }
            catalog
        };
        let _ = self.catalog.set(catalog.clone());
        Ok(catalog)
    }

    fn models(&self, cancel: &CancellationToken) -> Result<(u16, Vec<u8>), SttError> {
        validate_endpoint(&self.base_url)?;
        let mut req = self.http.get(format!("{}/models", self.base_url)).timeout(Duration::from_secs(5));
        if !self.api_key.is_empty() { req = req.bearer_auth(&self.api_key); }
        request(&self.http, req, MAX_CATALOG_BYTES, cancel)
    }

    /// `GET {base}/models` checks reachability without sending audio or paying for inference.
    /// Public catalogs (including Tinfoil's) do not establish whether the key is valid.
    pub fn check(&self) -> Result<(), SttError> {
        let (status, body) = self.models(&CancellationToken::default())?;
        let root = response_json(status, &body)?;
        if !root.get("data").is_some_and(|v| v.is_array()) {
            return Err(SttError::new(ErrorKind::BadResponse, "The server did not send a model catalog.", "not a model list"));
        }
        Ok(())
    }
}

impl Transcriber for OpenAiTranscriber {
    fn transcribe(&self, wav: &[u8], options: &TranscribeOptions) -> Result<String, SttError> {
        self.transcribe_cancellable(wav, options, &CancellationToken::default())
    }

    fn transcribe_cancellable(&self, wav: &[u8], options: &TranscribeOptions, cancel: &CancellationToken) -> Result<String, SttError> {
        self.transcribe_detecting_cancellable(wav, options, cancel).map(|(text, _)| text)
    }

    fn transcribe_detecting(&self, wav: &[u8], options: &TranscribeOptions) -> Result<(String, Option<String>), SttError> {
        self.transcribe_detecting_cancellable(wav, options, &CancellationToken::default())
    }

    fn transcribe_detecting_cancellable(&self, wav: &[u8], options: &TranscribeOptions, cancel: &CancellationToken) -> Result<(String, Option<String>), SttError> {
        self.transcribe_upload(wav, options, cancel).0
    }

    fn transcribe_with_upload(&self, wav: &[u8], options: &TranscribeOptions, cancel: &CancellationToken) -> (Result<String, SttError>, Option<UploadStats>) {
        let (result, upload) = self.transcribe_upload(wav, options, cancel);
        (result.map(|(text, _)| text), Some(upload))
    }
}

impl OpenAiTranscriber {
    fn transcribe_upload(&self, wav: &[u8], options: &TranscribeOptions, cancel: &CancellationToken) -> (Result<(String, Option<String>), SttError>, UploadStats) {
        let mut upload = UploadStats {
            format: self.upload_format.as_str().into(), input_bytes: wav.len() as u64,
            upload_bytes: 0, encoding_ms: 0, note: Some("No audio upload was attempted.".into()),
        };
        let result = self.upload_request(wav, options, cancel, &mut upload);
        (result, upload)
    }

    fn upload_request(&self, wav: &[u8], options: &TranscribeOptions, cancel: &CancellationToken, upload: &mut UploadStats) -> Result<(String, Option<String>), SttError> {
        cancel.check()?;
        validate_endpoint(&self.base_url)?;
        let catalog = if self.discover { self.discover_catalog(cancel)? } else { None };
        let mut format = self.upload_format;
        let mut format_note = None;
        if let Some(catalog) = &catalog {
            if !catalog.audio.iter().any(|f| f == format.as_str()) {
                if format == UploadFormat::Opus && catalog.audio.iter().any(|f| f == "wav") {
                    format = UploadFormat::Wav;
                    format_note = Some("Opus was requested, but this Amber server does not advertise Opus support. Selected lossless WAV before uploading; no retry.".into());
                } else {
                    let message = format!("This Amber server does not advertise {} uploads. No audio was uploaded.", format.as_str().to_uppercase());
                    upload.note = Some(message.clone());
                    return Err(SttError::new(ErrorKind::BadResponse, message, "requested audio format unavailable; no upload attempted"));
                }
            }
        } else if format == UploadFormat::Opus {
            // Current Groq/OpenAI transcription references list Ogg. Tinfoil's published
            // Voxtral image uses tinfoilsh/vllm-openai-audio v0.0.11, accepting Ogg/Opus.
            // Public /models is not evidence of authenticated format support.
            // https://console.groq.com/docs/speech-to-text
            // https://developers.openai.com/api/reference/resources/audio/subresources/transcriptions/methods/create
            // https://github.com/tinfoilsh/vllm-openai-audio/blob/v0.0.11/audio_proxy.py
            format_note = Some(if self.base_url == crate::providers::TINFOIL.base_url {
                if self.model == "voxtral-small-24b" {
                    "Tinfoil's published Voxtral server source accepts Ogg Opus; live authenticated support is unverified. No format retry."
                } else {
                    "Ogg Opus support for this Tinfoil model is unverified; the reviewed proxy source applies to Voxtral. No format retry."
                }
            } else if [crate::providers::GROQ.base_url, crate::providers::OPENAI.base_url].contains(&self.base_url.as_str()) {
                "The provider documents Ogg input; live inference with this encoder is unverified. No format retry."
            } else {
                "This endpoint has no Amber format advertisement. Explicitly requested Ogg Opus may be incompatible; no format retry."
            }.into());
        }
        upload.format = format.as_str().into();
        let (part, file_bytes) = if format == UploadFormat::Opus {
            let started = std::time::Instant::now();
            let encoded = crate::audio::encode_ogg_opus(wav, cancel);
            upload.encoding_ms = started.elapsed().as_millis() as u64;
            let encoded = encoded.map_err(|e| {
                upload.note = Some("Opus encoding did not complete. No audio upload or WAV retry was attempted.".into());
                e
            })?;
            let bytes = encoded.len() as u64;
            (multipart::Part::bytes(encoded).file_name("dictation.ogg").mime_str("audio/ogg").expect("static mime"), bytes)
        } else { (wav_part(wav), wav.len() as u64) };
        let model = if self.model.trim().is_empty() {
            catalog.as_ref().and_then(|c| c.default_speech_model()).unwrap_or(&self.model)
        } else { &self.model };
        let mut form = multipart::Form::new()
            .part("file", part)
            .text("model", model.to_string())
            .text("response_format", "json")
            .text("temperature", "0");
        // Amber detects among the entire list. Voxtral uses Android's auto-detection policy;
        // the short-dictation workaround is for Whisper, not Voxtral.
        let language = if catalog.is_some() {
            options.language.clone()
        } else if model == "voxtral-small-24b" {
            options.fixed().map(str::to_string)
        } else {
            cloud_language(options, wav_ms(wav))
        };
        if let Some(lang) = language {
            form = form.text("language", lang.to_string());
        }
        if catalog.is_some() { form = form.text("amber_cleanup", "client"); }
        if let Some(prompt) = &options.prompt {
            form = form.text("prompt", prompt.clone());
        }
        let mut req = self.http.post(format!("{}/audio/transcriptions", self.base_url)).multipart(form);
        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }
        cancel.check()?;
        // The payload length, not multipart or transport bytes. Keep it even if HTTP fails.
        upload.upload_bytes = file_bytes;
        upload.note = format_note;
        let (status, body) = request(&self.http, req, MAX_RESPONSE_BYTES, cancel)?;
        if format == UploadFormat::Opus && matches!(status, 400 | 415 | 422) {
            return Err(SttError::new(ErrorKind::Server,
                format!("The server rejected the Ogg Opus request (HTTP {status}). Select WAV if this endpoint does not support it. No format retry was made."),
                format!("HTTP {status}")));
        }
        let root = response_json(status, &body)?;
        let text = parse_text_value(&root, body.len())?;
        let language = root.get("language").and_then(|v| v.as_str()).map(str::to_string);
        cancel.check()?;
        Ok((text, language))
    }
}

/// whisper.cpp's `whisper-server`: `POST {base}/inference`.
pub struct WhisperServerTranscriber {
    base_url: String,
    /// For models tuned to a short encoder context: the slack beyond the audio.
    context_slack: Option<u32>,
    http: Client,
}

impl WhisperServerTranscriber {
    /// [context_slack]: `Some` for a short-context (ACFT) model - the encoder then stops
    /// where the speech stops instead of working through 30 s.
    pub fn new(base_url: &str, context_slack: Option<u32>) -> Self {
        // Generous: a long dictation on a CPU takes a while, and nothing is billed per second.
        WhisperServerTranscriber {
            base_url: base_url.trim().trim_end_matches('/').to_string(),
            context_slack,
            http: client(Duration::from_secs(300), base_url.trim()),
        }
    }
}

impl Transcriber for WhisperServerTranscriber {
    fn transcribe(&self, wav: &[u8], options: &TranscribeOptions) -> Result<String, SttError> {
        self.transcribe_cancellable(wav, options, &CancellationToken::default())
    }

    fn transcribe_detecting(&self, wav: &[u8], options: &TranscribeOptions) -> Result<(String, Option<String>), SttError> {
        self.transcribe_detecting_cancellable(wav, options, &CancellationToken::default())
    }

    fn transcribe_cancellable(&self, wav: &[u8], options: &TranscribeOptions, cancel: &CancellationToken) -> Result<String, SttError> {
        self.request(wav, options, "json", cancel).map(|(text, _)| text)
    }

    fn transcribe_detecting_cancellable(&self, wav: &[u8], options: &TranscribeOptions, cancel: &CancellationToken) -> Result<(String, Option<String>), SttError> {
        self.request(wav, options, "verbose_json", cancel)
    }
}

impl WhisperServerTranscriber {
    fn request(&self, wav: &[u8], options: &TranscribeOptions, format: &str, cancel: &CancellationToken) -> Result<(String, Option<String>), SttError> {
        cancel.check()?;
        let among = options.among();
        if among.len() < 2 {
            return self.run(wav, options, format, false, cancel).map(|(text, language, _)| (text, language));
        }
        // Several languages: one run with "auto", and the probabilities whisper-server works out
        // after it - on this dictation's own context (its detection before the run uses the
        // previous request's). Again in the likeliest allowed language when Whisper chose
        // another: short German speech is easily taken for Turkish or English.
        let any = TranscribeOptions { language: None, prompt: options.prompt.clone() };
        let (text, language, probabilities) = self.run(wav, &any, "verbose_json", true, cancel)?;
        let picked = pick_language(&probabilities, &among, wav_ms(wav));
        if language.as_deref().and_then(crate::chunking::language_code) == Some(picked.as_str()) {
            return Ok((text, language));
        }
        let fixed = TranscribeOptions { language: Some(picked), prompt: options.prompt.clone() };
        self.run(wav, &fixed, format, false, cancel).map(|(text, language, _)| (text, language))
    }

    /// One inference: the text, the language whisper-server names, and - with
    /// [probabilities] - its language probabilities (`{"de": 0.6, ...}`).
    fn run(&self, wav: &[u8], options: &TranscribeOptions, format: &str, probabilities: bool, cancel: &CancellationToken) -> Result<(String, Option<String>, serde_json::Value), SttError> {
        cancel.check()?;
        validate_endpoint(&self.base_url)?;
        let mut form = multipart::Form::new()
            .part("file", wav_part(wav))
            .text("response_format", format.to_string())
            .text("temperature", "0.0")
            // As Amber for Android decodes (and measured its speed): no timestamp tokens.
            .text("no_timestamps", "true")
            .text("language", options.language.clone().unwrap_or_else(|| "auto".into()));
        if format == "verbose_json" && !probabilities {
            // Only the detected language is wanted; the probabilities for every language cost
            // another encoder pass.
            form = form.text("no_language_probabilities", "true");
        }
        if let Some(prompt) = &options.prompt {
            form = form.text("prompt", prompt.clone());
        }
        let mut full_window = false;
        if let Some(slack) = self.context_slack {
            // 16-bit mono after the 44-byte header Amber writes.
            let ctx = crate::local::audio_ctx_for(wav.len().saturating_sub(44) / 2, slack);
            if ctx > 0 {
                form = form.text("audio_ctx", ctx.to_string());
            }
            full_window = ctx == 0;
        }
        let req = self.http.post(format!("{}/inference", self.base_url)).multipart(form);
        let (status, body) = request(&self.http, req, MAX_RESPONSE_BYTES, cancel)?;
        let json = response_json(status, &body)?;
        let text = parse_text_value(&json, body.len())?;
        if full_window {
            // whisper.cpp detects the language with the previous request's context; after a
            // full window that would cost the next short dictation a 30 s encoder pass.
            let (http, base, cancel) = (self.http.clone(), self.base_url.clone(), cancel.clone());
            std::thread::spawn(move || reset_context(&http, &base, &cancel));
        }
        let language = json.get("language").and_then(|l| l.as_str()).map(str::to_string);
        cancel.check()?;
        Ok((text, language, json["language_probabilities"].clone()))
    }
}

impl WhisperServerTranscriber {
    /// Leaves the server's state at a short context: after loading a tuned model, and after a
    /// dictation that needed the full window.
    pub fn reset_context(&self) {
        reset_context(&self.http, &self.base_url, &CancellationToken::default());
    }
}

/// A second of silence with a fixed language and the smallest context - whisper.cpp keeps
/// that context for its next language detection. Also warms the GPU up.
fn reset_context(http: &Client, base_url: &str, cancel: &CancellationToken) {
    if cancel.is_cancelled() || validate_endpoint(base_url).is_err() { return; }
    let silence = crate::audio::encode_wav(&vec![0.0; 19_200], crate::audio::TARGET_RATE);
    let form = multipart::Form::new()
        .part("file", wav_part(&silence))
        .text("response_format", "json")
        .text("no_timestamps", "true")
        .text("language", "en")
        .text("audio_ctx", "256");
    let _ = request(http, http.post(format!("{base_url}/inference")).multipart(form), MAX_RESPONSE_BYTES, cancel);
}

/// Whether a local server (whisper.cpp, llama.cpp) is up and done loading. Both answer 503
/// while their model loads - whisper.cpp even opens its port before that, so an open port
/// alone says nothing. Other non-redirect answers, even a 404 for a missing /health, mean
/// ready. Use [health_ok] when identifying the expected helper is required.
pub fn local_server_ready(base_url: &str) -> bool {
    if validate_endpoint(base_url).is_err() { return false; }
    let url = format!("{}/health", base_url.trim_end_matches('/'));
    let http = client(Duration::from_secs(2), base_url);
    request(&http, http.get(url), 64 * 1024, &CancellationToken::default()).is_ok_and(|(status, _)| status != 503 && !(300..400).contains(&status))
}

/// Whether the server at [base_url] says it is ready: `GET /health` answering 200 with
/// `{"status": "ok"}` (llama-server, whisper-server). Anything else - loading, an error, some
/// other program that took the port - is not.
pub fn health_ok(base_url: &str) -> bool {
    health(base_url).is_some_and(|v| v["status"] == "ok")
}

/// What `GET /health` at [base_url] answered with 200, if anything.
pub fn health(base_url: &str) -> Option<serde_json::Value> {
    validate_endpoint(base_url).ok()?;
    let url = format!("{}/health", base_url.trim_end_matches('/'));
    let http = client(Duration::from_secs(2), base_url);
    let (status, body) = request(&http, http.get(url), 64 * 1024, &CancellationToken::default()).ok()?;
    (status == 200).then(|| response_json(status, &body).ok()).flatten()
}

fn wav_part(wav: &[u8]) -> multipart::Part {
    multipart::Part::bytes(wav.to_vec()).file_name("dictation.wav").mime_str("audio/wav").expect("static mime")
}

pub fn parse_text(body: &str) -> Result<String, SttError> {
    let parsed = serde_json::from_str(body).map_err(|e| json_error(&e, body.len()))?;
    parse_text_value(&parsed, body.len())
}

fn parse_text_value(parsed: &serde_json::Value, bytes: usize) -> Result<String, SttError> {
    if parsed.get("error").is_some_and(|v| !v.is_null()) {
        return Err(SttError::new(ErrorKind::Server, "The engine reported an error.", "response contains an error field"));
    }
    parsed.get("text").and_then(|v| v.as_str()).map(str::to_string)
        .ok_or_else(|| SttError::new(ErrorKind::BadResponse, "The engine sent no text.", format!("{bytes} bytes without a text field")))
}

pub fn status_error(status: u16, _body: &str) -> SttError {
    // Bodies and headers can contain credentials, transcripts, or a proxy's reflected URL.
    let detail = format!("HTTP {status}");
    match status {
        401 | 403 => SttError::new(ErrorKind::Unauthorized, "The API key was rejected - check it in the settings.", detail),
        404 => SttError::new(ErrorKind::Server, "The server does not know this address or model.", detail),
        413 => SttError::new(ErrorKind::TooLarge, "The recording is too long for this server.", detail),
        429 => SttError::new(ErrorKind::RateLimited, "Rate limit reached - try again in a moment.", detail),
        500..=599 => SttError::new(ErrorKind::Unavailable, format!("The server had a problem (HTTP {status})."), detail),
        _ => SttError::new(ErrorKind::Server, format!("The server had a problem (HTTP {status})."), detail),
    }
}

pub(crate) fn network_error(e: reqwest::Error) -> SttError {
    if e.is_timeout() {
        SttError::new(ErrorKind::Timeout, "The engine took too long to answer.", "request timeout")
    } else if e.is_connect() {
        SttError::new(ErrorKind::Unreachable, "The engine is not reachable - check the connection.", "connection failed")
    } else {
        SttError::new(ErrorKind::Unreachable, "The connection to the engine failed.", "request failed")
    }
}

pub(crate) fn json_error(error: &serde_json::Error, bytes: usize) -> SttError {
    // serde's Display can quote a value from the response. Position and size are enough.
    SttError::new(ErrorKind::BadResponse, "The engine sent an answer Amber does not understand.",
        format!("invalid JSON at line {} column {} ({bytes} bytes)", error.line(), error.column()))
}

#[cfg(test)]
pub(crate) mod testserver {
    //! A one-shot HTTP server on 127.0.0.1 for tests: answers each request with the next
    //! canned response and hands back what it received.

    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    pub struct Received {
        pub head: String,
        pub body: Vec<u8>,
    }

    pub fn serve(responses: Vec<(u16, &'static str, Vec<u8>)>) -> (String, mpsc::Receiver<Received>) {
        serve_raw(responses.into_iter().map(|(status, content_type, body)| {
            let mut response = format!("HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
            response.extend(body);
            response
        }).collect())
    }

    pub fn serve_raw(responses: Vec<Vec<u8>>) -> (String, mpsc::Receiver<Received>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for response in responses {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut head = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                    head.push_str(&line);
                }
                let len = head
                    .lines()
                    .find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                    .unwrap_or(0);
                let mut req_body = vec![0; len];
                reader.read_exact(&mut req_body).unwrap();
                let mut stream = reader.into_inner();
                let _ = tx.send(Received { head, body: req_body });
                let _ = stream.write_all(&response);
            }
        });
        (url, rx)
    }

    /// Hold an incomplete response until the test releases it, without timing-dependent sleeps.
    pub fn stalled(prefix: &'static [u8]) -> (String, mpsc::Receiver<()>, mpsc::Sender<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
            let mut reader = BufReader::new(stream);
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 { return; }
                if line == "\r\n" { break; }
                if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            // Do not race the client's request with an unsolicited partial response.
            reader.read_exact(&mut vec![0; length]).unwrap();
            let mut stream = reader.into_inner();
            let _ = stream.write_all(prefix);
            let _ = ready_tx.send(());
            let _ = release_rx.recv();
        });
        (url, ready_rx, release_tx)
    }
}

#[cfg(test)]
mod tests {
    use super::testserver::serve;
    use super::*;

    fn json(status: u16, body: &str) -> (u16, &'static str, Vec<u8>) {
        (status, "application/json", body.as_bytes().to_vec())
    }

    #[test]
    fn opus_upload_has_matching_multipart_metadata_and_voxtral_fields() {
        let (url, rx) = serve(vec![json(200, r#"{"text":"Hello."}"#)]);
        let wav = crate::audio::encode_wav(&vec![0.1; 32_007], 16_000);
        let original = wav.clone();
        let (result, upload) = OpenAiTranscriber::new(&format!("{url}/v1"), "test-key", "voxtral-small-24b")
            .with_upload_format(UploadFormat::Opus)
            .transcribe_with_upload(&wav, &TranscribeOptions::new("de,en", "Amber"), &CancellationToken::default());
        assert_eq!(result.unwrap(), "Hello.");
        let upload = upload.unwrap();
        assert_eq!(upload.format, "opus");
        assert_eq!(upload.input_bytes, wav.len() as u64);
        assert!(upload.upload_bytes < upload.input_bytes / 4);
        assert!(upload.note.as_deref().unwrap().contains("may be incompatible"));
        assert_eq!(wav, original);
        let req = rx.recv().unwrap();
        assert!(req.head.starts_with("POST /v1/audio/transcriptions "));
        assert!(req.head.to_lowercase().contains("authorization: bearer test-key\r\n"));
        let body = String::from_utf8_lossy(&req.body);
        for field in ["filename=\"dictation.ogg\"", "Content-Type: audio/ogg", "name=\"model\"\r\n\r\nvoxtral-small-24b",
            "name=\"response_format\"\r\n\r\njson", "name=\"prompt\"\r\n\r\nAmber"] {
            assert!(body.contains(field), "missing {field}");
        }
        assert!(!body.contains("name=\"language\"") && !body.contains("amber_cleanup"));
        let start = req.body.windows(4).position(|w| w == b"OggS").unwrap();
        let file = &req.body[start..start + upload.upload_bytes as usize];
        assert_eq!(crate::audio::decode_upload(file).unwrap().0.len(), 32_007);
        let serialized = serde_json::to_value(upload).unwrap();
        assert_eq!(serialized["inputBytes"], wav.len());
        assert!(serialized["encodingMs"].is_u64());
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err(), "no catalog probe or extra POST");
    }

    #[test]
    fn amber_upload_format_negotiation_happens_before_a_single_post() {
        for (amber, requested, actual) in [
            (r#"{"audio":["wav","opus"]}"#, UploadFormat::Wav, "wav"),
            (r#"{"audio":["wav","opus"]}"#, UploadFormat::Opus, "opus"),
            (r#"{"audio":["opus"]}"#, UploadFormat::Opus, "opus"),
            (r#"{"audio":["wav"]}"#, UploadFormat::Opus, "wav"),
            (r#"{}"#, UploadFormat::Wav, "wav"),
            (r#"{}"#, UploadFormat::Opus, "wav"),
        ] {
            let (url, rx) = serve(vec![json(200, &format!(r#"{{"amber":{amber},"data":[]}}"#)), json(200, r#"{"text":"x"}"#)]);
            let wav = crate::audio::encode_wav(&vec![0.1; 16_000], 16_000);
            let engine = OpenAiTranscriber::new(&url, "", "m").with_catalog_discovery();
            let engine = if requested == UploadFormat::Opus { engine.with_upload_format(requested) } else { engine };
            let (result, upload) = engine.transcribe_with_upload(&wav, &TranscribeOptions::new("de,en", ""), &CancellationToken::default());
            assert!(result.is_ok());
            let upload = upload.unwrap();
            assert_eq!(upload.format, actual);
            assert!(rx.recv().unwrap().head.starts_with("GET /models "));
            let req = rx.recv().unwrap();
            assert!(req.head.starts_with("POST /audio/transcriptions "));
            let body = String::from_utf8_lossy(&req.body);
            assert!(body.contains("name=\"language\"\r\n\r\nde,en"));
            assert!(body.contains("name=\"amber_cleanup\"\r\n\r\nclient"));
            if actual == "wav" {
                assert_eq!(upload.upload_bytes, wav.len() as u64);
                assert_eq!(upload.encoding_ms, 0);
                assert!(req.body.windows(wav.len()).any(|w| w == wav));
                assert!(body.contains("filename=\"dictation.wav\"") && body.contains("Content-Type: audio/wav"));
            } else { assert!(body.contains("filename=\"dictation.ogg\"")); }
            if requested == UploadFormat::Opus && actual == "wav" {
                assert!(upload.note.unwrap().contains("before uploading"));
            } else { assert!(upload.note.is_none()); }
            assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        }
    }

    #[test]
    fn amber_explicit_invalid_or_unsupported_formats_never_imply_support() {
        for amber in [r#"{"audio":[]}"#, r#"{"audio":["flac"]}"#, r#"{"audio":null}"#,
            r#"{"audio":"wav"}"#, r#"{"audio":{}}"#, r#"{"audio":true}"#, r#"{"audio":1}"#,
            r#"{"audio":["WAV","audio/wav",1]}"#] {
            for format in [UploadFormat::Wav, UploadFormat::Opus] {
                let (url, rx) = serve(vec![json(200, &format!(r#"{{"amber":{amber},"data":[]}}"#))]);
                let (result, upload) = OpenAiTranscriber::new(&url, "", "m").with_catalog_discovery().with_upload_format(format)
                    .transcribe_with_upload(b"unencoded", &TranscribeOptions::default(), &CancellationToken::default());
                assert_eq!(result.unwrap_err().kind, ErrorKind::BadResponse);
                let upload = upload.unwrap();
                assert_eq!(upload.format, format.as_str());
                assert_eq!(upload.upload_bytes, 0);
                assert_eq!(upload.encoding_ms, 0);
                assert!(upload.note.unwrap().contains("No audio was uploaded"));
                assert!(rx.recv().unwrap().head.starts_with("GET "));
                assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
            }
        }
    }

    #[test]
    fn opus_rejection_keeps_attempt_metadata_without_a_wav_retry() {
        for status in [400, 415, 422, 401, 500] {
            let (url, rx) = serve(vec![json(status, "untrusted response")]);
            let wav = crate::audio::encode_wav(&vec![0.1; 16_000], 16_000);
            let (result, upload) = OpenAiTranscriber::new(&url, "", "voxtral-small-24b").with_upload_format(UploadFormat::Opus)
                .transcribe_with_upload(&wav, &TranscribeOptions::default(), &CancellationToken::default());
            let error = result.unwrap_err();
            assert_eq!(error.detail, format!("HTTP {status}"));
            if matches!(status, 400 | 415 | 422) {
                assert!(error.message.contains("Select WAV"));
                assert!(!error.kind.service_unavailable());
            }
            let upload = upload.unwrap();
            assert_eq!(upload.format, "opus");
            assert!(upload.upload_bytes > 0);
            assert!(String::from_utf8_lossy(&rx.recv().unwrap().body).contains("dictation.ogg"));
            assert!(rx.recv_timeout(Duration::from_millis(100)).is_err(), "only one paid request");
        }
    }

    #[test]
    fn upload_stats_are_per_call_not_shared_between_concurrent_recordings() {
        let (url, rx) = serve(vec![json(200, r#"{"text":"x"}"#), json(200, r#"{"text":"x"}"#)]);
        let transcriber = Arc::new(OpenAiTranscriber::new(&url, "", "m").with_upload_format(UploadFormat::Opus));
        let workers: Vec<_> = [16_000, 64_123].into_iter().map(|len| {
            let transcriber = transcriber.clone();
            std::thread::spawn(move || {
                let wav = crate::audio::encode_wav(&vec![0.1; len], 16_000);
                let (result, upload) = transcriber.transcribe_with_upload(&wav, &TranscribeOptions::default(), &CancellationToken::default());
                result.unwrap();
                let upload = upload.unwrap();
                assert_eq!(upload.input_bytes, wav.len() as u64);
                (len, upload.upload_bytes)
            })
        }).collect();
        let expected: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        for _ in 0..2 {
            let req = rx.recv().unwrap();
            let start = req.body.windows(4).position(|w| w == b"OggS").unwrap();
            assert!(expected.iter().any(|&(len, size)| req.body.get(start..start + size as usize)
                .is_some_and(|file| crate::audio::decode_upload(file).is_ok_and(|(s, _)| s.len() == len))));
        }
    }

    #[test]
    fn wav_upload_stats_and_preupload_failures_are_honest() {
        let wav = crate::audio::encode_wav(&vec![0.1; 16_000], 16_000);
        let (url, rx) = serve(vec![json(200, r#"{"text":"x"}"#)]);
        let (result, upload) = OpenAiTranscriber::new(&url, "", "m")
            .transcribe_with_upload(&wav, &TranscribeOptions::default(), &CancellationToken::default());
        result.unwrap();
        let upload = upload.unwrap();
        assert_eq!(upload.format, "wav");
        assert_eq!(upload.input_bytes, wav.len() as u64);
        assert_eq!(upload.upload_bytes, upload.input_bytes);
        assert_eq!(upload.encoding_ms, 0);
        assert!(upload.note.is_none());
        assert!(rx.recv().unwrap().body.windows(wav.len()).any(|w| w == wav));
        for cancelled in [false, true] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let cancel = CancellationToken::default();
            if cancelled { cancel.cancel(); }
            let (result, upload) = OpenAiTranscriber::new(&format!("http://{}", listener.local_addr().unwrap()), "", "m")
                .with_upload_format(UploadFormat::Opus).transcribe_with_upload(b"invalid WAV", &TranscribeOptions::default(), &cancel);
            assert!(result.is_err());
            if cancelled { assert_eq!(result.unwrap_err().kind, ErrorKind::Cancelled); }
            let upload = upload.unwrap();
            assert_eq!(upload.upload_bytes, 0);
            assert!(upload.note.unwrap().contains("No audio upload"));
            assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
        }
    }

    #[test]
    fn local_transcribers_keep_original_wav_and_have_no_upload_stats() {
        struct Local(Vec<u8>);
        impl Transcriber for Local {
            fn transcribe(&self, wav: &[u8], _: &TranscribeOptions) -> Result<String, SttError> {
                assert_eq!(wav, self.0);
                Ok("local".into())
            }
        }
        let wav = crate::audio::encode_wav(&vec![0.1; 16_000], 16_000);
        let (result, upload) = Local(wav.clone()).transcribe_with_upload(&wav, &TranscribeOptions::default(), &CancellationToken::default());
        assert_eq!(result.unwrap(), "local");
        assert!(upload.is_none());
    }

    #[test]
    fn openai_request_and_answer() {
        let (url, rx) = serve(vec![json(200, r#"{"text":" Hallo Welt."}"#)]);
        let t = OpenAiTranscriber::new(&format!("{url}/v1/"), " sk-test ", "whisper-large-v3-turbo");
        let text = t.transcribe(b"RIFFdata", &TranscribeOptions::new("de", "Tauri")).unwrap();
        assert_eq!(text, " Hallo Welt.");

        let req = rx.recv().unwrap();
        assert!(req.head.starts_with("POST /v1/audio/transcriptions "));
        assert!(req.head.to_lowercase().contains("authorization: bearer sk-test\r\n"));
        let body = String::from_utf8_lossy(&req.body);
        for needle in ["name=\"model\"\r\n\r\nwhisper-large-v3-turbo", "name=\"language\"\r\n\r\nde", "name=\"prompt\"\r\n\r\nTauri",
                       "filename=\"dictation.wav\"", "RIFFdata"] {
            assert!(body.contains(needle), "missing {needle}");
        }
    }

    #[test]
    fn auto_language_and_empty_vocabulary_are_left_out() {
        let (url, rx) = serve(vec![json(200, r#"{"text":"x"}"#)]);
        OpenAiTranscriber::new(&url, "", "m").transcribe(b"", &TranscribeOptions::new("auto", "  ")).unwrap();
        let req = rx.recv().unwrap();
        let body = String::from_utf8_lossy(&req.body);
        assert!(!body.contains("name=\"language\""));
        assert!(!body.contains("name=\"prompt\""));
        assert!(!req.head.to_lowercase().contains("authorization"));
    }

    #[test]
    fn tinfoil_voxtral_uses_wav_json_and_only_a_single_explicit_language() {
        let model = crate::providers::TINFOIL.default_model();
        let wav = crate::audio::encode_wav(&vec![0.1; 16_000], 16_000);
        for (language, expected) in [("de,en", None), ("auto", None), ("de-DE", Some("de"))] {
            let (url, rx) = serve(vec![json(200, r#"{"text":"Hello world."}"#)]);
            let result = OpenAiTranscriber::new(&format!("{url}/v1/"), "tinfoil-test-key", model)
                .transcribe_detecting(&wav, &TranscribeOptions::new(language, "Amber")).unwrap();
            assert_eq!(result, ("Hello world.".into(), None));
            let req = rx.recv().unwrap();
            assert!(req.head.starts_with("POST /v1/audio/transcriptions "));
            assert!(req.head.to_lowercase().contains("authorization: bearer tinfoil-test-key\r\n"));
            assert!(req.head.to_lowercase().contains("content-type: multipart/form-data; boundary="));
            assert!(req.body.windows(wav.len()).any(|bytes| bytes == wav));
            let body = String::from_utf8_lossy(&req.body);
            for field in ["name=\"model\"\r\n\r\nvoxtral-small-24b", "name=\"response_format\"\r\n\r\njson",
                          "filename=\"dictation.wav\"", "Content-Type: audio/wav", "name=\"prompt\"\r\n\r\nAmber"] {
                assert!(body.contains(field), "missing {field}");
            }
            match expected {
                Some(code) => assert!(body.contains(&format!("name=\"language\"\r\n\r\n{code}\r\n"))),
                None => assert!(!body.contains("name=\"language\"")),
            }
            assert!(!body.contains("amber_cleanup"));
            assert!(rx.try_recv().is_err(), "no catalog probe or duplicate upload");
        }
    }

    #[test]
    fn connection_check_requires_a_catalog_but_not_inference() {
        for key in ["", "invalid-test-key"] {
            let (url, rx) = serve(vec![json(200, r#"{"object":"list","data":[{"id":"voxtral-small-24b"}]}"#)]);
            OpenAiTranscriber::new(&format!("{url}/v1"), key, "").check().unwrap();
            assert!(rx.recv().unwrap().head.starts_with("GET /v1/models "));
            assert!(rx.try_recv().is_err());
        }
        let (url, _rx) = serve(vec![json(200, r#"{"status":"ok"}"#)]);
        assert_eq!(OpenAiTranscriber::new(&url, "", "").check().unwrap_err().kind, ErrorKind::BadResponse);
    }

    #[test]
    fn whisper_server_request() {
        let (url, rx) = serve(vec![json(200, r#"{"text":" Test.\n"}"#)]);
        let text = WhisperServerTranscriber::new(&url, None).transcribe(b"wav", &TranscribeOptions::new("auto", "")).unwrap();
        assert_eq!(text, " Test.\n");
        let req = rx.recv().unwrap();
        assert!(req.head.starts_with("POST /inference "));
        let body = String::from_utf8_lossy(&req.body).to_string();
        assert!(body.contains("name=\"language\"\r\n\r\nauto"));
        assert!(!body.contains("audio_ctx"), "a stock model gets the full window");
    }

    #[test]
    fn languages_read_as_a_choice() {
        let o = TranscribeOptions::new("de-DE, en_US", "");
        assert_eq!(o.language.as_deref(), Some("de,en"));
        assert_eq!((o.among(), o.fixed()), (vec!["de".to_string(), "en".to_string()], None));
        let one = TranscribeOptions::new("de", "");
        assert_eq!((one.among(), one.fixed()), (vec!["de".to_string()], Some("de")));
        let auto = TranscribeOptions::new("auto", "");
        assert_eq!((auto.language.clone(), auto.among()), (None, Vec::<String>::new()));
        assert_eq!(TranscribeOptions::new("de,de, ,auto", "").language.as_deref(), Some("de"));
    }

    #[test]
    fn the_likeliest_allowed_language_wins() {
        // A long dictation: the likeliest, whatever the margin - once Whisper is fairly sure.
        let long = 6_000;
        let allowed = ["de".to_string(), "en".to_string()];
        assert_eq!(pick_language(&serde_json::json!({"tr": 0.1, "en": 0.6, "de": 0.3}), &allowed, long), "en");
        assert_eq!(pick_language(&serde_json::json!({"tr": 0.5, "en": 0.3, "de": 0.2}), &allowed, long), "de", "unsure: the first");
        // Turkish is not allowed; neither listed language heard at all: the first.
        assert_eq!(pick_language(&serde_json::json!({"tr": 0.9}), &allowed, long), "de");
        assert_eq!(pick_language(&serde_json::json!({"de": 0.4, "en": 0.4}), &allowed, long), "de", "a tie: the first");
        assert_eq!(pick_language(&serde_json::Value::Null, &allowed, long), "de");
        // A German speaker's English sentence, 7.4 s (TTS "Stefan").
        assert_eq!(pick_language(&serde_json::json!({"de": 0.216, "en": 0.587}), &allowed, 7_390), "en");
    }

    #[test]
    fn a_short_dictation_stays_in_the_first_language_unless_another_is_clear() {
        let de_en = ["de".to_string(), "en".to_string()];
        let pick = |de: f64, en: f64, ms: u64| pick_language(&serde_json::json!({"de": de, "en": en, "ja": 0.2}), &de_en, ms);
        // Measured on whisper-server, Large v3 Turbo (ACFT), TTS at 1.5-2x speed: German that
        // came back as English (in English, "Okay, danke." is "Okay, thank you.").
        assert_eq!(pick(0.025, 0.292, 650), "de", "Okay, danke.");
        assert_eq!(pick(0.177, 0.300, 740), "de", "Na gut.");
        assert_eq!(pick(0.037, 0.414, 570), "de", "Na gut.");
        assert_eq!(pick(0.466, 0.469, 880), "de", "Okay, bis morgen.");
        // German that was right stays right.
        assert_eq!(pick(0.998, 0.001, 940), "de", "Ich hab kein Wasser.");
        assert_eq!(pick(0.999, 0.000, 720), "de", "Bis gleich.");
        // Clearly English stays English.
        assert_eq!(pick(0.253, 0.701, 870), "en", "See you later.");
        assert_eq!(pick(0.000, 0.998, 830), "en", "See you later.");
        assert_eq!(pick(0.081, 0.639, 660), "en", "Thank you.");
        // Unsure English goes to German - whisper keeps short English as English there
        // (measured: "No problem.", "Sounds good.", "Thank you." unchanged).
        assert_eq!(pick(0.352, 0.378, 880), "de", "No problem.");
        assert_eq!(pick(0.407, 0.568, 850), "de", "Sounds good.");
        // Nothing hardcoded to German: the first chosen decides.
        let en_de = ["en".to_string(), "de".to_string()];
        assert_eq!(pick_language(&serde_json::json!({"de": 0.40, "en": 0.30}), &en_de, 900), "en");
        assert_eq!(pick_language(&serde_json::json!({"de": 0.95, "en": 0.03}), &en_de, 900), "de");
        // A tie and none of them: the first, short or long.
        assert_eq!(pick(0.4, 0.4, 900), "de");
        assert_eq!(pick_language(&serde_json::json!({"tr": 0.9}), &de_en, 900), "de");
        // Long: the likeliest again - when Whisper is anywhere near sure.
        assert_eq!(pick(0.177, 0.600, SHORT_DICTATION_MS + 1), "en");
    }

    #[test]
    fn an_unsure_guess_keeps_the_first_language_at_any_length() {
        let de_en = ["de".to_string(), "en".to_string()];
        let pick = |de: f64, en: f64, ms: u64| pick_language(&serde_json::json!({"de": de, "en": en}), &de_en, ms);
        // Fabi's dictation of 2026-11-07, its first part of 3.7 s: German taken for English at
        // 0.12 to 0.10 - and every later part recognized in English too.
        assert_eq!(pick(0.10, 0.12, 3_700), "de");
        assert_eq!(pick(0.177, 0.300, 9_000), "de");
        // Long and sure, as nearly all are (236 of 243 in Fabi's log at 0.9 or more).
        assert_eq!(pick(0.01, 0.98, 6_200), "en");
        assert_eq!(pick(0.216, 0.587, 7_390), "en");
    }

    #[test]
    fn a_cloud_service_gets_the_first_language_for_a_short_dictation() {
        let list = TranscribeOptions::new("de,en", "");
        assert_eq!(cloud_language(&list, 1_400).as_deref(), Some("de"), "short: no free detection");
        assert_eq!(cloud_language(&list, SHORT_DICTATION_MS + 1), None, "long: the service detects");
        assert_eq!(cloud_language(&TranscribeOptions::new("en", ""), 60_000).as_deref(), Some("en"));
        assert_eq!(cloud_language(&TranscribeOptions::new("auto", ""), 900), None);
    }

    #[test]
    fn the_pick_is_logged_without_the_text() {
        let line = language_note(&serde_json::json!({"de": 0.025, "en": 0.292, "ja": 0.3}), &["de".to_string(), "en".to_string()], 1377, "de");
        assert_eq!(line, "language: 1.4 s of audio, among de,en: de 0.03, en 0.29 -> de");
    }

    #[test]
    fn whisper_server_keeps_its_choice_when_it_is_allowed() {
        // One run with "auto": the probabilities come from after the run, on its own context
        // (whisper.cpp detects on the previous request's context - the review's finding).
        let (url, rx) = serve(vec![json(200, r#"{"text":"Ok, danke.","language":"german","language_probabilities":{"de":0.6,"en":0.3}}"#)]);
        let (text, language) = WhisperServerTranscriber::new(&url, Some(128))
            .transcribe_detecting(&crate::audio::encode_wav(&vec![0.1; 16_000], 16_000), &TranscribeOptions::new("de,en", ""))
            .unwrap();
        let run = String::from_utf8_lossy(&rx.recv().unwrap().body).to_string();
        assert!(run.contains("name=\"language\"\r\n\r\nauto") && run.contains("verbose_json") && !run.contains("no_language_probabilities"), "{run}");
        assert_eq!((text.as_str(), language.as_deref()), ("Ok, danke.", Some("german")));
        assert!(rx.try_recv().is_err(), "one request when Whisper chose an allowed language");
    }

    #[test]
    fn whisper_server_runs_again_in_the_likeliest_allowed_language() {
        // Short German speech that whisper.cpp alone took for Turkish.
        let (url, rx) = serve(vec![
            json(200, r#"{"text":"Ich hap kain vasser","language":"turkish","language_probabilities":{"tr":0.55,"de":0.3,"en":0.1}}"#),
            json(200, r#"{"text":"Ich hab kein Wasser.","language":"german"}"#),
        ]);
        let (text, language) = WhisperServerTranscriber::new(&url, Some(128))
            .transcribe_detecting(&crate::audio::encode_wav(&vec![0.1; 16_000], 16_000), &TranscribeOptions::new("de,en", ""))
            .unwrap();
        let _first = rx.recv().unwrap();
        let second = String::from_utf8_lossy(&rx.recv().unwrap().body).to_string();
        assert!(second.contains("name=\"language\"\r\n\r\nde"), "Turkish was never allowed: {second}");
        assert_eq!((text.as_str(), language.as_deref()), ("Ich hab kein Wasser.", Some("german")));
    }

    #[test]
    fn a_cloud_engine_gets_no_language_list() {
        // Groq or OpenAI refuse "de,en": they detect by themselves then - a long dictation.
        let (url, rx) = serve(vec![json(200, r#"{"text":"x"}"#), json(200, r#"{"text":"x"}"#)]);
        let long = crate::audio::encode_wav(&vec![0.1; 16 * (SHORT_DICTATION_MS as usize + 500)], 16_000);
        OpenAiTranscriber::new(&url, "", "m").transcribe(&long, &TranscribeOptions::new("de,en", "")).unwrap();
        let body = String::from_utf8_lossy(&rx.recv().unwrap().body).to_string();
        assert!(!body.contains("name=\"language\""), "{body}");
        // A short one in the first chosen language.
        let short = crate::audio::encode_wav(&vec![0.1; 16_000], 16_000);
        OpenAiTranscriber::new(&url, "", "m").transcribe(&short, &TranscribeOptions::new("de,en", "")).unwrap();
        let body = String::from_utf8_lossy(&rx.recv().unwrap().body).to_string();
        assert!(body.contains("name=\"language\"\r\n\r\nde\r\n"), "{body}");
    }

    #[test]
    fn a_tuned_model_gets_a_short_encoder_context() {
        let (url, rx) = serve(vec![json(200, r#"{"text":"x"}"#)]);
        // 5 s of 16 kHz audio.
        let wav = crate::audio::encode_wav(&vec![0.1; 80_000], 16_000);
        WhisperServerTranscriber::new(&url, Some(64)).transcribe(&wav, &TranscribeOptions::default()).unwrap();
        let body = String::from_utf8_lossy(&rx.recv().unwrap().body).to_string();
        assert!(body.contains("name=\"audio_ctx\"\r\n\r\n314"), "250 positions of speech + 64");
    }

    #[test]
    fn errors_become_plain_words() {
        let (url, _rx) = serve(vec![
            json(401, r#"{"error":{"message":"Invalid API Key","type":"invalid_request_error"}}"#),
            json(429, "slow down"),
            json(500, "boom"),
            json(200, "<html>"),
            json(200, r#"{"error":"model not loaded"}"#),
        ]);
        let t = OpenAiTranscriber::new(&url, "k", "m");
        let o = TranscribeOptions::default();
        let e = t.transcribe(b"", &o).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Unauthorized);
        assert_eq!(e.detail, "HTTP 401");
        assert_eq!(t.transcribe(b"", &o).unwrap_err().kind, ErrorKind::RateLimited);
        let down = t.transcribe(b"", &o).unwrap_err().kind;
        assert_eq!(down, ErrorKind::Unavailable);
        assert!(down.service_unavailable() && !ErrorKind::Unauthorized.service_unavailable());
        assert_eq!(t.transcribe(b"", &o).unwrap_err().kind, ErrorKind::BadResponse);
        assert_eq!(t.transcribe(b"", &o).unwrap_err().kind, ErrorKind::Server);
    }

    #[test]
    fn unreachable_server() {
        // Bind and drop: the port is closed.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let e = WhisperServerTranscriber::new(&format!("http://127.0.0.1:{port}"), None).transcribe(b"", &TranscribeOptions::default()).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Unreachable);
    }

    #[test]
    fn a_full_window_is_followed_by_a_short_reset() {
        let (url, rx) = serve(vec![
            (200, "application/json", br#"{"text":"long"}"#.to_vec()),
            (200, "application/json", br#"{"text":""}"#.to_vec()),
        ]);
        // 30 s: past the short-context range.
        let wav = crate::audio::encode_wav(&vec![0.1; 16_000 * 30], 16_000);
        WhisperServerTranscriber::new(&url, Some(128)).transcribe(&wav, &TranscribeOptions::default()).unwrap();
        let first = String::from_utf8_lossy(&rx.recv().unwrap().body).to_string();
        assert!(!first.contains("audio_ctx") && first.contains("name=\"no_timestamps\"\r\n\r\ntrue"));
        let reset = String::from_utf8_lossy(&rx.recv_timeout(Duration::from_secs(5)).unwrap().body).to_string();
        assert!(reset.contains("name=\"audio_ctx\"\r\n\r\n256") && reset.contains("name=\"language\"\r\n\r\nen"));
    }

    #[test]
    fn the_detected_language_comes_along() {
        let (url, rx) = serve(vec![json(200, r#"{"task":"transcribe","language":"german","text":" Hallo.","segments":[]}"#)]);
        let (text, lang) = WhisperServerTranscriber::new(&url, None).transcribe_detecting(b"wav", &TranscribeOptions::default()).unwrap();
        assert_eq!((text.as_str(), lang.as_deref()), (" Hallo.", Some("german")));
        let body = String::from_utf8_lossy(&rx.recv().unwrap().body).to_string();
        assert!(body.contains("verbose_json") && body.contains("name=\"no_language_probabilities\"\r\n\r\ntrue"));
    }

    #[test]
    fn health_means_this_kind_of_server_said_ok() {
        let (url, _rx) = serve(vec![json(200, r#"{"status":"ok"}"#)]);
        assert!(health_ok(&url));
        let (url, _rx) = serve(vec![json(503, r#"{"error":"loading"}"#)]);
        assert!(!health_ok(&url));
        // Some other program on the port.
        let (url, _rx) = serve(vec![json(200, r#"<html>hello</html>"#)]);
        assert!(!health_ok(&url));
        assert!(!health_ok("http://127.0.0.1:9"));
    }

    #[test]
    fn local_server_readiness() {
        let (url, _rx) = serve(vec![json(503, r#"{"status":"loading model"}"#), json(200, "{}"), json(404, "{}")]);
        assert!(!local_server_ready(&url), "503 is still loading");
        assert!(local_server_ready(&url));
        assert!(local_server_ready(&url), "no /health at all, but answering");
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert!(!local_server_ready(&format!("http://127.0.0.1:{port}")), "not listening");
    }

    #[test]
    fn key_check() {
        let (url, rx) = serve(vec![json(200, r#"{"data":[]}"#), json(401, "{}")]);
        let t = OpenAiTranscriber::new(&url, "k", "m");
        assert!(t.check().is_ok());
        assert!(rx.recv().unwrap().head.starts_with("GET /models "));
        assert_eq!(t.check().unwrap_err().kind, ErrorKind::Unauthorized);
    }

    #[test]
    fn amber_discovery_preserves_wav_languages_model_and_client_cleanup() {
        let (url, rx) = serve(vec![
            json(200, r#"{"amber":{"service":"amber-server","audio":["wav","opus"]},"data":[{"id":"speech/default","amber":{"kind":"speech","default":true}}]}"#),
            json(200, r#"{"text":"Hello.","language":"english"}"#),
            json(200, r#"{"text":"Again."}"#),
        ]);
        let engine = OpenAiTranscriber::new(&format!("{url}/private-path/v1"), "test-key", "chosen/model:Q8").with_catalog_discovery();
        let wav = crate::audio::encode_wav(&vec![0.1; 16_000 * 4], 16_000);
        let (text, language) = engine.transcribe_detecting(&wav, &TranscribeOptions::new("de-DE,en_US", "Tauri")).unwrap();
        assert_eq!((text.as_str(), language.as_deref()), ("Hello.", Some("english")));
        let discovery = rx.recv().unwrap();
        assert!(discovery.head.starts_with("GET /private-path/v1/models "));
        assert!(discovery.head.to_lowercase().contains("authorization: bearer test-key"));
        let upload = rx.recv().unwrap();
        assert!(upload.head.starts_with("POST /private-path/v1/audio/transcriptions "));
        assert!(upload.body.windows(wav.len()).any(|w| w == wav));
        let form = String::from_utf8_lossy(&upload.body);
        for field in ["name=\"language\"\r\n\r\nde,en\r\n", "name=\"model\"\r\n\r\nchosen/model:Q8\r\n", "name=\"amber_cleanup\"\r\n\r\nclient\r\n", "name=\"prompt\"\r\n\r\nTauri\r\n", "filename=\"dictation.wav\"", "Content-Type: audio/wav"] {
            assert!(form.contains(field), "missing {field}");
        }
        engine.transcribe(b"short", &TranscribeOptions::new("en,de", "")).unwrap();
        let second = rx.recv().unwrap();
        assert!(second.head.starts_with("POST "), "discovery cached for this immutable endpoint");
        assert!(String::from_utf8_lossy(&second.body).contains("name=\"language\"\r\n\r\nen,de\r\n"));
    }

    #[test]
    fn amber_uses_catalog_default_only_for_an_empty_selection() {
        let (url, rx) = serve(vec![
            json(200, r#"{"amber":{"audio":["wav"]},"data":[{"id":"first","amber":{"kind":"speech"}},{"id":"recommended","amber":{"kind":"speech","default":true}}]}"#),
            json(200, r#"{"text":"x"}"#),
        ]);
        OpenAiTranscriber::new(&url, "", "").with_catalog_discovery().transcribe(b"wav", &TranscribeOptions::default()).unwrap();
        rx.recv().unwrap();
        assert!(String::from_utf8_lossy(&rx.recv().unwrap().body).contains("name=\"model\"\r\n\r\nrecommended\r\n"));
    }

    #[test]
    fn ordinary_custom_catalogs_keep_openai_fields_and_no_blind_retries() {
        for catalog in [json(200, r#"{"data":[]}"#), json(404, "missing"), json(405, "unsupported")] {
            let (url, rx) = serve(vec![catalog, json(400, "prompt refused")]);
            let error = OpenAiTranscriber::new(&url, "", "m").with_catalog_discovery()
                .transcribe(b"wav", &TranscribeOptions::new("de,en", "vocab")).unwrap_err();
            assert_eq!(error.detail, "HTTP 400");
            rx.recv().unwrap();
            let req = rx.recv().unwrap();
            let body = String::from_utf8_lossy(&req.body);
            assert!(body.contains("name=\"language\"\r\n\r\nde\r\n"));
            assert!(!body.contains("amber_cleanup"));
            assert!(rx.recv_timeout(Duration::from_millis(100)).is_err(), "no paid POST retry");
        }
    }

    #[test]
    fn discovery_failure_or_opus_only_catalog_prevents_audio_upload() {
        for response in [json(401, "secret"), json(200, "<html>secret</html>"), json(200, r#"{"amber":{"audio":["opus"]},"data":[]}"#)] {
            let (url, rx) = serve(vec![response]);
            assert!(OpenAiTranscriber::new(&url, "", "m").with_catalog_discovery().transcribe(b"secret audio", &TranscribeOptions::default()).is_err());
            assert!(rx.recv().unwrap().head.starts_with("GET "));
            assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        }
    }

    #[test]
    fn redirects_never_forward_credentials_or_audio() {
        let target = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        target.set_nonblocking(true).unwrap();
        for status in [302, 307, 308] {
            let (url, rx) = testserver::serve_raw(vec![format!("HTTP/1.1 {status} Redirect\r\nLocation: http://{}/stolen\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", target.local_addr().unwrap()).into_bytes()]);
            let error = OpenAiTranscriber::new(&url, "private-key", "m").transcribe(b"private audio", &TranscribeOptions::default()).unwrap_err();
            assert_eq!(error.detail, format!("HTTP {status}"));
            rx.recv().unwrap();
            assert_eq!(target.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
        }
    }

    #[test]
    fn responses_are_bounded_with_and_without_content_length() {
        let (url, _rx) = testserver::serve_raw(vec![b"HTTP/1.1 200 OK\r\nContent-Length: 999999999\r\nConnection: close\r\n\r\n".to_vec()]);
        let error = OpenAiTranscriber::new(&url, "", "m").transcribe(b"", &TranscribeOptions::default()).unwrap_err();
        assert!(error.detail.contains("exceeds"));
        let payload = vec![b'x'; MAX_RESPONSE_BYTES + 1];
        let mut chunked = format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n", payload.len()).into_bytes();
        chunked.extend(payload);
        chunked.extend_from_slice(b"\r\n0\r\n\r\n");
        let (url, _rx) = testserver::serve_raw(vec![chunked]);
        let error = OpenAiTranscriber::new(&url, "", "m").transcribe(b"", &TranscribeOptions::default()).unwrap_err();
        assert!(error.detail.contains("exceeds"));
        let (url, _rx) = serve(vec![(200, "application/json", vec![b'x'; MAX_CATALOG_BYTES + 1])]);
        assert!(OpenAiTranscriber::new(&url, "", "m").catalog().unwrap_err().detail.contains("exceeds"));
    }

    #[test]
    fn diagnostics_never_include_response_or_url_secrets() {
        for body in [r#"{"text":"private transcript","error":{"message":"private credential"}}"#, r#"{"text":{"private credential":"private transcript"}}"#, r#"{"text":\"private credential"#] {
            let error = parse_text(body).unwrap_err();
            assert!(!format!("{error:?} {error}").contains("private"));
        }
        for status in [401, 429, 500] {
            assert!(!status_error(status, "private transcript / private credential").to_string().contains("private"));
        }
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let error = OpenAiTranscriber::new(&format!("http://127.0.0.1:{port}/private-key/v1"), "private-bearer", "m").transcribe(b"", &TranscribeOptions::default()).unwrap_err();
        assert!(!format!("{error:?} {error}").contains("private"));
        assert_eq!(error.kind, ErrorKind::Unreachable);
    }

    #[test]
    fn unsafe_endpoints_fail_before_connecting() {
        for url in ["http://192.168.1.1/v1", "http://localhost/v1", "https://user:private@example.com/v1", "https://example.com/v1?private=key"] {
            let error = OpenAiTranscriber::new(url, "private", "m").transcribe(b"private audio", &TranscribeOptions::default()).unwrap_err();
            assert_eq!(error.detail, "endpoint rejected");
            assert!(!error.to_string().contains("private"));
        }
    }

    #[test]
    fn cancellation_stops_waiting_for_headers_and_partial_bodies() {
        for prefix in [b"".as_slice(), b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n{\"text\":\"partial".as_slice()] {
            let (url, ready, release) = testserver::stalled(prefix);
            let token = CancellationToken::default();
            let worker_token = token.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                let result = OpenAiTranscriber::new(&url, "", "m").transcribe_cancellable(b"wav", &TranscribeOptions::default(), &worker_token);
                tx.send(result).unwrap();
            });
            ready.recv_timeout(Duration::from_secs(2)).unwrap();
            token.cancel();
            assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap().unwrap_err().kind, ErrorKind::Cancelled);
            drop(release);
            worker.join().unwrap();
        }
    }

    #[test]
    fn cancellation_before_a_request_sends_nothing_and_timeout_is_bounded() {
        let token = CancellationToken::default();
        token.cancel();
        let error = OpenAiTranscriber::new("https://not-contacted.invalid/v1", "", "m")
            .transcribe_cancellable(b"", &TranscribeOptions::default(), &token).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
        assert!(!error.kind.service_unavailable());
        let (url, _ready, release) = testserver::stalled(b"");
        let http = client(Duration::from_millis(100), &url);
        assert_eq!(request(&http, http.get(&url), 1024, &CancellationToken::default()).unwrap_err().kind, ErrorKind::Timeout);
        drop(release);
    }

    #[test]
    fn cancelled_http_operation_closes_its_connection() {
        use std::io::{BufRead, BufReader, Read};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (ready_tx, ready) = std::sync::mpsc::channel();
        let (closed_tx, closed) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let mut reader = BufReader::new(stream);
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" { break; }
                if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            reader.read_exact(&mut vec![0; length]).unwrap();
            ready_tx.send(()).unwrap();
            let mut byte = [0];
            closed_tx.send(matches!(reader.read(&mut byte), Ok(0))).unwrap();
        });
        let token = CancellationToken::default();
        let worker_token = token.clone();
        let worker = std::thread::spawn(move || OpenAiTranscriber::new(&url, "", "m")
            .transcribe_cancellable(b"wav", &TranscribeOptions::default(), &worker_token));
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        token.cancel();
        assert_eq!(worker.join().unwrap().unwrap_err().kind, ErrorKind::Cancelled);
        assert!(closed.recv_timeout(Duration::from_secs(3)).unwrap(), "cancellation must close the pending socket, not only hide its result");
        server.join().unwrap();
    }
}
