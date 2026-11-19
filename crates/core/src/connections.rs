//! Saved endpoint identities, credential binding, and explicit, non-billable model discovery.

use std::collections::HashSet;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::engine::{self, CancellationToken, ErrorKind, SttError};
use crate::providers::{self, Provider};
use crate::settings::Settings;

pub const MAX_CONNECTIONS: usize = 64;
const MAX_MODELS: usize = 1024;
const MAX_CATALOG_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub base_url: String,
}

impl Connection {
    pub fn normalized(&self) -> Result<Self, String> {
        if !valid_id(&self.id) { return Err("A connection needs a stable conn_ identifier (at most 80 characters).".into()); }
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > 80 || name.chars().any(hidden) {
            return Err("Give the connection a single-line name of at most 80 characters.".into());
        }
        let provider = providers::by_id(&self.provider).ok_or("Unknown connection adapter.")?;
        let base = self.base_url.trim().trim_end_matches('/');
        if base.len() > 2048 { return Err("The connection address is too long.".into()); }
        let base_url = if provider.id == "custom" {
            providers::check_base_url(base)?.as_str().trim_end_matches('/').to_string()
        } else {
            if !base.is_empty() && providers::check_base_url(base)? != providers::check_base_url(provider.base_url)? {
                return Err("This adapter uses its fixed provider address. Choose Own server for another OpenAI-compatible endpoint.".into());
            }
            provider.base_url.to_string()
        };
        Ok(Self { id: self.id.clone(), name: name.to_string(), provider: provider.id.to_string(), base_url })
    }

    /// Names are display-only. Even a same-origin path change invalidates an old snapshot.
    pub fn same_endpoint(&self, other: &Self) -> bool {
        match (self.normalized(), other.normalized()) {
            (Ok(a), Ok(b)) => a.id == b.id && a.provider == b.provider && a.base_url == b.base_url,
            _ => false,
        }
    }

    pub fn same_credential_binding(&self, other: &Self) -> bool {
        match (self.normalized(), other.normalized()) {
            (Ok(a), Ok(b)) => a.id == b.id && a.provider == b.provider
                && providers::same_credential_origin(&a.base_url, &b.base_url),
            _ => false,
        }
    }

    pub fn credential_slot(&self) -> Result<String, String> {
        if !valid_id(&self.id) { return Err("Invalid connection identifier.".into()); }
        Ok(format!("connection:{}", self.id))
    }
}

fn valid_id(id: &str) -> bool {
    id.strip_prefix("conn_").is_some_and(|suffix| !suffix.is_empty() && id.len() <= 80
        && suffix.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-')))
}

fn hidden(c: char) -> bool {
    c.is_control() || matches!(c, '\u{00ad}' | '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}')
}

pub fn validate(connections: &[Connection]) -> Result<(), String> {
    if connections.len() > MAX_CONNECTIONS { return Err("At most 64 saved connections are supported.".into()); }
    let mut ids = HashSet::new();
    for connection in connections {
        connection.normalized()?;
        if !ids.insert(&connection.id) { return Err("Connection identifiers must be unique.".into()); }
    }
    Ok(())
}

/// Do not discard invalid identities or duplicate IDs: resolution must fail closed rather
/// than accidentally choosing the first duplicate or falling back to a legacy provider.
pub fn sanitize(connections: Vec<Connection>) -> Vec<Connection> {
    connections.into_iter().map(|c| c.normalized().unwrap_or(c)).collect()
}

pub fn find<'a>(connections: &'a [Connection], id: &str) -> Result<&'a Connection, String> {
    if !valid_id(id) { return Err("Invalid saved connection identifier; choose a connection again.".into()); }
    let mut matches = connections.iter().filter(|c| c.id == id);
    let connection = matches.next().ok_or("The selected connection no longer exists. Choose a connection explicitly; no other service was used.")?;
    if matches.next().is_some() { return Err("Duplicate connection identifiers; no service was used.".into()); }
    connection.normalized()?;
    Ok(connection)
}

pub struct Resolved<'a> {
    pub provider: &'static Provider,
    pub base_url: String,
    pub connection: Option<&'a Connection>,
}

impl Resolved<'_> {
    pub fn cleaner(&self, key: &str, model: &str) -> Result<crate::cleanup::ChatCleaner, SttError> {
        // Saved connections use one exact API base for models, speech and cleanup. Keep
        // legacy root/full-chat-URL normalization only for the existing concrete settings.
        let base = if self.connection.is_some() && self.provider.id == "custom" {
            format!("{}/chat/completions", self.base_url)
        } else { self.base_url.clone() };
        crate::cleanup::ChatCleaner::for_provider(self.provider, &base, key, model)
    }
}

fn resolve<'a>(connections: &'a [Connection], id: Option<&str>, legacy_provider: &str, legacy_base: &str, capability: Capability) -> Result<Resolved<'a>, String> {
    let connection = id.map(|id| find(connections, id)).transpose()?;
    let (provider, base_url) = if let Some(c) = connection {
        let c = c.normalized()?;
        (providers::by_id(&c.provider).ok_or("Unknown connection adapter.")?, c.base_url)
    } else {
        let p = providers::by_id(legacy_provider).ok_or("Unknown provider.")?;
        (p, providers::effective_base_url(p, legacy_base))
    };
    if !capability.supported_by(provider) { return Err("The selected connection adapter does not support this operation.".into()); }
    providers::check_base_url(&base_url)?;
    Ok(Resolved { provider, base_url, connection })
}

pub fn resolve_speech(s: &Settings) -> Result<Resolved<'_>, String> {
    resolve(&s.connections, s.server.connection_id.as_deref(), &s.server.provider, &s.server.base_url, Capability::Speech)
}

pub fn resolve_cleanup(s: &Settings) -> Result<Resolved<'_>, String> {
    let base = if s.cleanup.base_url.trim().is_empty() { &s.server.base_url } else { &s.cleanup.base_url };
    resolve(&s.connections, s.cleanup.connection_id.as_deref(), &s.cleanup.provider, base, Capability::Cleanup)
}

pub fn validate_settings(s: &Settings) -> Result<(), String> {
    validate(&s.connections)?;
    if s.server.connection_id.is_some() { resolve_speech(s)?; }
    if s.cleanup.connection_id.is_some() { resolve_cleanup(s)?; }
    Ok(())
}

/// Serialized only inside the encrypted credential store, never in Settings or IPC DTOs.
/// One slot per stable ID prevents old adapter/origin credentials reappearing after re-save.
#[derive(Serialize, Deserialize)]
pub struct Credential {
    connection_id: String,
    adapter: String,
    origin: String,
    key: String,
}

impl Credential {
    pub fn new(connection: &Connection, key: &str) -> Result<Self, String> {
        let connection = connection.normalized()?;
        let key = validate_key(key)?;
        Ok(Self { connection_id: connection.id, adapter: connection.provider, origin: providers::credential_origin(&connection.base_url)?, key })
    }

    pub fn key_for(&self, connection: &Connection) -> Option<&str> {
        let c = connection.normalized().ok()?;
        (self.connection_id == c.id && self.adapter == c.provider && providers::credential_origin(&c.base_url).ok().as_deref() == Some(self.origin.as_str()))
            .then_some(self.key.as_str()).filter(|key| !key.is_empty())
    }
}

pub fn validate_key(key: &str) -> Result<String, String> {
    let key = key.trim();
    if key.len() > 8192 || !key.bytes().all(|c| (0x20..=0x7e).contains(&c)) {
        return Err("The API key must contain only printable ASCII characters and be at most 8192 bytes.".into());
    }
    Ok(key.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Capability { Speech, Cleanup }

impl Capability {
    fn supported_by(self, provider: &Provider) -> bool {
        match self { Self::Speech => provider.supports_speech(), Self::Cleanup => provider.supports_cleanup() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredModel {
    pub id: String,
    pub label: String,
    /// Empty means unclassified, not compatible with both operations.
    pub capabilities: Vec<Capability>,
    pub note: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Discovery {
    pub models: Vec<DiscoveredModel>,
    pub truncated: bool,
    pub notice: String,
}

/// Exactly one bounded GET, only on explicit caller action. Never invokes inference,
/// follows pagination/redirects, or probes an alternative origin when a catalog fails.
pub fn discover(connection: &Connection, key: &str) -> Result<Discovery, SttError> {
    let c = connection.normalized().map_err(configuration_error)?;
    let provider = providers::by_id(&c.provider).ok_or_else(|| configuration_error("Unknown adapter."))?;
    discover_at(provider, &c.base_url, key)
}

fn configuration_error(message: impl Into<String>) -> SttError {
    SttError::new(ErrorKind::Server, message, "connection configuration rejected")
}

fn discover_at(provider: &Provider, base: &str, key: &str) -> Result<Discovery, SttError> {
    providers::check_base_url(base).map_err(configuration_error)?;
    let key = validate_key(key).map_err(configuration_error)?;
    let url = match provider.id {
        "gemini" => format!("{}/models", base.strip_suffix("/openai").ok_or_else(|| configuration_error("Invalid Gemini catalog base."))?),
        "ollama" => format!("{}/api/tags", base.strip_suffix("/v1").ok_or_else(|| configuration_error("Invalid Ollama catalog base."))?),
        _ => format!("{}/models", base.trim_end_matches('/')),
    };
    let http = engine::client(Duration::from_secs(8), base);
    let mut req = http.get(url);
    match provider.id {
        "anthropic" => { req = req.header("anthropic-version", "2023-06-01").header("x-api-key", &key); }
        "deepgram" => { req = req.header("Authorization", format!("Token {key}")); }
        "elevenlabs" => { req = req.header("xi-api-key", &key); }
        "gemini" => { req = req.header("x-goog-api-key", &key); }
        _ if !key.is_empty() => { req = req.bearer_auth(&key); }
        _ => (),
    }
    let (status, body) = engine::request(&http, req, MAX_CATALOG_BYTES, &CancellationToken::default())?;
    parse_catalog(provider, &engine::response_json(status, &body)?)
}

fn parse_catalog(provider: &Provider, root: &Value) -> Result<Discovery, SttError> {
    let invalid = || SttError::new(ErrorKind::BadResponse, "The server did not send the expected model catalog.", "invalid model catalog");
    if root.get("error").is_some_and(|v| !v.is_null()) || root.get("err_code").is_some() { return Err(invalid()); }
    let entries = match provider.id {
        "deepgram" => root.get("stt"),
        "elevenlabs" => Some(root),
        "gemini" | "ollama" => root.get("models"),
        _ => root.get("data"),
    }.and_then(Value::as_array).ok_or_else(invalid)?;
    let mut result = Discovery {
        models: Vec::new(),
        truncated: root.get("has_more").and_then(Value::as_bool) == Some(true)
            || root.get("nextPageToken").and_then(Value::as_str).is_some_and(|s| !s.is_empty()),
        notice: "Catalog GET only: reachability is not proof of key validity, model access, or inference compatibility. Unclassified models require an explicit manual choice; model-specific parameters may be needed.".into(),
    };
    if provider.id == "elevenlabs" {
        result.notice.push_str(" ElevenLabs' model catalog primarily lists voice-generation models; missing Scribe models can still be selected from Amber's curated speech list.");
    }
    let mut seen = HashSet::new();
    for entry in entries {
        if result.models.len() == MAX_MODELS { result.truncated = true; break; }
        let raw_id = match provider.id {
            "deepgram" => entry.get("canonical_name").or_else(|| entry.get("name")),
            "elevenlabs" => entry.get("model_id"),
            "gemini" | "ollama" => entry.get("name"),
            _ => entry.get("id"),
        }.and_then(Value::as_str);
        let Some(raw_id) = raw_id else { continue };
        let id = if provider.id == "gemini" { raw_id.strip_prefix("models/").unwrap_or(raw_id) } else { raw_id };
        if !valid_model_id(id) || !seen.insert(id.to_string()) { continue; }
        let mut capabilities = Vec::new();
        // Curated entries are compatibility evidence; model-name guesses are not.
        if provider.models.iter().any(|m| m.id == id) { capabilities.push(Capability::Speech); }
        if provider.chat_models.iter().any(|m| m.id == id) { capabilities.push(Capability::Cleanup); }
        let advertised = match provider.id {
            "custom" if root.get("amber").is_some_and(Value::is_object) => match entry.pointer("/amber/kind").and_then(Value::as_str) {
                Some("speech") => Some(Capability::Speech), Some("cleanup") => Some(Capability::Cleanup), _ => None,
            },
            "deepgram" if entry.get("batch").and_then(Value::as_bool) == Some(true) => Some(Capability::Speech),
            "anthropic" => Some(Capability::Cleanup),
            "mistral" => {
                if entry.pointer("/capabilities/audio_transcription").and_then(Value::as_bool) == Some(true) { capabilities.push(Capability::Speech); }
                (entry.pointer("/capabilities/completion_chat").and_then(Value::as_bool) == Some(true)).then_some(Capability::Cleanup)
            }
            // The native Gemini API also lists embeddings, image and audio generation.
            // Only curated IDs establish compatible OpenAI cleanup parameter behavior.
            "openrouter" if has_string(entry.pointer("/architecture/input_modalities"), "text")
                && has_string(entry.pointer("/architecture/output_modalities"), "text")
                && has_string(entry.get("supported_parameters"), "temperature")
                && has_string(entry.get("supported_parameters"), "max_tokens") => Some(Capability::Cleanup),
            _ => None,
        };
        if let Some(capability) = advertised { capabilities.push(capability); }
        capabilities.retain(|c| c.supported_by(provider));
        capabilities.sort_by_key(|c| match c { Capability::Speech => 0, Capability::Cleanup => 1 });
        capabilities.dedup();
        let label = entry.get("display_name").or_else(|| entry.get("displayName")).or_else(|| entry.get("name"))
            .or_else(|| entry.pointer("/amber/label")).and_then(Value::as_str).unwrap_or(id);
        let label = label.chars().filter(|c| !hidden(*c)).take(120).collect::<String>().trim().to_string();
        let note = if capabilities.is_empty() {
            "Unclassified for Amber. Choose manually only after checking the model's API and required parameters."
        } else { "Capability identified from Amber's curated catalog or provider metadata; no inference was performed." };
        result.models.push(DiscoveredModel { id: id.to_string(), label: if label.is_empty() { id.to_string() } else { label }, capabilities, note: note.into() });
    }
    Ok(result)
}

fn has_string(value: Option<&Value>, needle: &str) -> bool {
    value.and_then(Value::as_array).is_some_and(|items| items.iter().any(|v| v.as_str() == Some(needle)))
}

fn valid_model_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 200 && !id.starts_with('/') && !id.contains("..")
        && id.bytes().all(|c| c.is_ascii_alphanumeric() || b"._-:/@+".contains(&c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::testserver::{serve, serve_raw};
    use serde_json::json;

    fn connection(id: &str, provider: &str, base: &str) -> Connection {
        Connection { id: id.into(), name: "My server".into(), provider: provider.into(), base_url: base.into() }
    }

    #[test]
    fn connection_validation_is_strict_and_never_repairs_identity_or_destination() {
        let c = connection("conn_a", "custom", "https://example.com/v1/");
        assert_eq!(c.normalized().unwrap().base_url, "https://example.com/v1");
        for id in ["", "groq", "conn_", "conn_a/path", "conn_a\n", "conn_\u{00e4}"] {
            assert!(connection(id, "custom", &c.base_url).normalized().is_err());
        }
        for base in ["http://private.lan/v1", "http://8.8.8.8/v1", "https://user:key@example.com/v1", "https://example.com/v1?key=secret"] {
            assert!(connection("conn_a", "custom", base).normalized().is_err());
        }
        assert!(connection("conn_a", "openai", "https://other.example/v1").normalized().is_err());
        assert_eq!(connection("conn_a", "openai", "").normalized().unwrap().base_url, providers::OPENAI.base_url);
        assert!(validate(&vec![c.clone(); 2]).is_err());
        assert!(find(&vec![c.clone(); 2], "conn_a").is_err());
        let invalid = connection("conn_invalid", "future", "https://old.example/v1");
        assert_eq!(sanitize(vec![invalid.clone()]), vec![invalid]);
    }

    #[test]
    fn credentials_are_separate_and_bound_to_adapter_and_origin() {
        let c = connection("conn_a", "custom", "https://example.com/v1");
        let credential = Credential::new(&c, " secret ").unwrap();
        assert_eq!(credential.key_for(&c), Some("secret"));
        let path = connection("conn_a", "custom", "https://example.com/new/v1");
        assert_eq!(credential.key_for(&path), Some("secret"));
        assert!(!c.same_endpoint(&path));
        for base in ["https://elsewhere.example/v1", "https://example.com:8443/v1", "http://127.0.0.1/v1"] {
            assert!(credential.key_for(&connection("conn_a", "custom", base)).is_none());
        }
        let openai = connection("conn_a", "openai", "");
        let compatible = connection("conn_a", "custom", providers::OPENAI.base_url);
        assert!(Credential::new(&compatible, "secret").unwrap().key_for(&openai).is_none());
        let other = connection("conn_b", "custom", &c.base_url);
        assert!(credential.key_for(&other).is_none());
        assert_ne!(other.credential_slot().unwrap(), c.credential_slot().unwrap());
        assert_ne!(c.credential_slot().unwrap(), "custom:https://example.com");
        assert!(Credential::new(&c, "key\r\nInjected: x").is_err());
    }

    #[test]
    fn old_settings_round_trip_without_connection_migration_or_rerouting() {
        let s: Settings = serde_json::from_value(json!({"server":{"provider":"custom","baseUrl":"https://old.example/v1","model":"old-speech"},
            "cleanup":{"provider":"anthropic","model":"claude-haiku-4-5"}})).unwrap();
        let s = s.sanitized();
        assert!(s.connections.is_empty());
        assert!(s.server.connection_id.is_none() && s.cleanup.connection_id.is_none());
        assert_eq!(resolve_speech(&s).unwrap().base_url, "https://old.example/v1");
        let restored: Settings = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
        assert_eq!(restored, s);
        let mut missing = s.clone();
        missing.server.connection_id = Some("conn_missing".into());
        let missing = missing.sanitized();
        assert_eq!(missing.server.connection_id.as_deref(), Some("conn_missing"));
        assert!(validate_settings(&missing).is_err());
        assert!(resolve_speech(&missing).is_err());
    }

    #[test]
    fn selected_connections_override_legacy_fields_without_mutating_the_snapshot() {
        let mut s = Settings::default();
        s.connections = vec![connection("conn_speech", "deepgram", ""), connection("conn_chat", "anthropic", "")];
        s.server.connection_id = Some("conn_speech".into());
        s.cleanup.connection_id = Some("conn_chat".into());
        let snapshot = s.clone().sanitized();
        assert_eq!(resolve_speech(&snapshot).unwrap().provider.id, "deepgram");
        assert_eq!(resolve_cleanup(&snapshot).unwrap().provider.id, "anthropic");
        s.connections[0] = connection("conn_speech", "custom", "https://changed.example/v1");
        assert!(!snapshot.connections[0].same_endpoint(&s.connections[0]));
        assert_eq!(resolve_speech(&snapshot).unwrap().base_url, providers::DEEPGRAM.base_url);
        s.server.connection_id = Some("conn_chat".into());
        assert!(resolve_speech(&s).is_err());
    }

    #[test]
    fn saved_custom_cleanup_and_discovery_share_the_exact_api_base() {
        let (base, rx) = serve(vec![
            (200, "application/json", br#"{"data":[]}"#.to_vec()),
            (200, "application/json", br#"{"choices":[{"message":{"content":"Hello."},"finish_reason":"stop"}]}"#.to_vec()),
        ]);
        let c = connection("conn_custom", "custom", &format!("{base}/prefix/v2"));
        discover(&c, "test-key").unwrap();
        let mut s = Settings::default();
        s.connections.push(c);
        s.cleanup.connection_id = Some("conn_custom".into());
        resolve_cleanup(&s).unwrap().cleaner("test-key", "manual-model").unwrap()
            .forward(&json!({"messages":[{"role":"user","content":"Hello."}]})).unwrap();
        assert!(rx.recv().unwrap().head.starts_with("GET /prefix/v2/models "));
        assert!(rx.recv().unwrap().head.starts_with("POST /prefix/v2/chat/completions "));
    }

    #[test]
    fn compatible_catalog_never_guesses_capabilities_from_model_names() {
        let result = parse_catalog(&providers::CUSTOM, &json!({"data":[{"id":"whisper-1"},{"id":"gpt-4o-mini"},{"id":"anything","capabilities":["speech","cleanup"]}]})).unwrap();
        assert_eq!(result.models.len(), 3);
        assert!(result.models.iter().all(|m| m.capabilities.is_empty()));
        let result = parse_catalog(&providers::OPENAI, &json!({"data":[{"id":"whisper-1"},{"id":"gpt-4o-mini"},{"id":"o3"},{"id":"gpt-5"}]})).unwrap();
        assert_eq!(result.models[0].capabilities, [Capability::Speech]);
        assert_eq!(result.models[1].capabilities, [Capability::Cleanup]);
        assert!(result.models[2..].iter().all(|m| m.capabilities.is_empty()));
    }

    #[test]
    fn metadata_ids_labels_and_catalog_size_are_bounded() {
        let result = parse_catalog(&providers::CUSTOM, &json!({"amber":{}, "data":[
            {"id":"speech/one", "amber":{"kind":"speech","label":"Speech\n\u{202e} one"}},
            {"id":"chat", "amber":{"kind":"cleanup"}}, {"id":"../bad"}, {"id":"/bad"}, {"id":"line\nbreak"},
            {"id":"speech/one"}, {"id":"a".repeat(201)}, {"id":"future", "amber":{"kind":"embedding"}}
        ]})).unwrap();
        assert_eq!(result.models.len(), 3);
        assert_eq!(result.models[0].label, "Speech one");
        assert_eq!(result.models[0].capabilities, [Capability::Speech]);
        assert_eq!(result.models[1].capabilities, [Capability::Cleanup]);
        assert!(result.models[2].capabilities.is_empty());
        let data: Vec<_> = (0..MAX_MODELS + 10).map(|n| json!({"id":format!("model-{n}")})).collect();
        let result = parse_catalog(&providers::CUSTOM, &json!({"data":data})).unwrap();
        assert_eq!(result.models.len(), MAX_MODELS);
        assert!(result.truncated);
    }

    #[test]
    fn native_capability_metadata_is_not_confused_with_tts_embeddings_or_streaming() {
        let deepgram = parse_catalog(&providers::DEEPGRAM, &json!({"stt":[{"canonical_name":"batch-model","batch":true},{"canonical_name":"stream-model","batch":false}],"tts":[{"canonical_name":"tts"}]})).unwrap();
        assert_eq!(deepgram.models[0].capabilities, [Capability::Speech]);
        assert!(deepgram.models[1].capabilities.is_empty());
        let mistral = parse_catalog(&providers::MISTRAL, &json!({"data":[{"id":"speech","capabilities":{"audio_transcription":true}}, {"id":"chat","capabilities":{"completion_chat":true}}, {"id":"embed","capabilities":{"vision":true}}]})).unwrap();
        assert_eq!(mistral.models[0].capabilities, [Capability::Speech]);
        assert_eq!(mistral.models[1].capabilities, [Capability::Cleanup]);
        assert!(mistral.models[2].capabilities.is_empty());
        let eleven = parse_catalog(&providers::ELEVENLABS, &json!([{"model_id":"tts","can_do_text_to_speech":true}])).unwrap();
        assert!(eleven.models[0].capabilities.is_empty());
        assert!(eleven.notice.contains("Scribe"));
        let gemini = parse_catalog(&providers::GEMINI, &json!({"models":[{"name":"models/embedding","supportedGenerationMethods":["embedContent"]},{"name":"models/gemini-3.1-flash-lite"}],"nextPageToken":"not-followed"})).unwrap();
        assert!(gemini.models[0].capabilities.is_empty());
        assert_eq!(gemini.models[1].id, "gemini-3.1-flash-lite");
        assert_eq!(gemini.models[1].capabilities, [Capability::Cleanup]);
        assert!(gemini.truncated);
        let router = parse_catalog(&providers::OPENROUTER, &json!({"data":[{"id":"vendor/chat","architecture":{"input_modalities":["text"],"output_modalities":["text"]},"supported_parameters":["temperature","max_tokens"]},{"id":"vendor/image","architecture":{"output_modalities":["image"]}}]})).unwrap();
        assert_eq!(router.models[0].capabilities, [Capability::Cleanup]);
        assert!(router.models[1].capabilities.is_empty());
    }

    #[test]
    fn discovery_uses_one_get_with_native_auth_and_catalog_paths() {
        for (provider, suffix, path, auth, body) in [
            (&providers::CUSTOM, "/v1", "/v1/models", "authorization: bearer test-key", json!({"data":[]})),
            (&providers::ANTHROPIC, "/v1", "/v1/models", "x-api-key: test-key", json!({"data":[{"id":"claude-new"}],"has_more":true})),
            (&providers::DEEPGRAM, "/v1", "/v1/models", "authorization: token test-key", json!({"stt":[]})),
            (&providers::ELEVENLABS, "/v1", "/v1/models", "xi-api-key: test-key", json!([])),
            (&providers::GEMINI, "/v1beta/openai", "/v1beta/models", "x-goog-api-key: test-key", json!({"models":[]})),
            (&providers::OLLAMA, "/v1", "/api/tags", "authorization: bearer test-key", json!({"models":[]})),
            (&providers::OPENROUTER, "/api/v1", "/api/v1/models", "authorization: bearer test-key", json!({"data":[]})),
        ] {
            let (base, rx) = serve(vec![(200, "application/json", serde_json::to_vec(&body).unwrap())]);
            discover_at(provider, &format!("{base}{suffix}"), " test-key ").unwrap();
            let req = rx.recv().unwrap();
            assert!(req.head.starts_with(&format!("GET {path} HTTP/1.1\r\n")), "{}", req.head);
            assert!(req.head.to_lowercase().contains(auth));
            assert!(req.body.is_empty());
            if provider.id == "anthropic" { assert!(req.head.contains("2023-06-01")); }
            assert!(!req.head.contains("?"));
            assert!(rx.try_recv().is_err());
        }
    }

    #[test]
    fn discovery_refuses_redirects_oversized_bodies_and_error_envelopes_without_leaking() {
        for response in [
            b"HTTP/1.1 302 Found\r\nLocation: https://elsewhere.example/PRIVATE_KEY\r\nContent-Length: 0\r\n\r\n".to_vec(),
            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", MAX_CATALOG_BYTES + 1).into_bytes(),
            b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 11\r\n\r\nPRIVATE_KEY".to_vec(),
        ] {
            let (base, rx) = serve_raw(vec![response]);
            let error = discover_at(&providers::CUSTOM, &base, "PRIVATE_KEY").unwrap_err();
            assert!(!error.to_string().contains("PRIVATE_KEY"));
            assert!(rx.recv().unwrap().head.starts_with("GET /models "));
            assert!(rx.try_recv().is_err());
        }
        for body in [json!({"data":[],"error":{"message":"PRIVATE_KEY"}}), json!({"data":null}), json!({"data":"PRIVATE_KEY"})] {
            let error = parse_catalog(&providers::CUSTOM, &body).unwrap_err();
            assert!(!error.to_string().contains("PRIVATE_KEY"));
        }
    }
}
