//! Servers that speak the OpenAI audio API. Known services have a fixed address and a list
//! of models; an own server (Speaches, LocalAI, whisper.cpp, ...) gets free fields.

use reqwest::Url;

pub struct SpeechModel {
    pub id: &'static str,
    pub label: &'static str,
    pub note: &'static str,
}

/// A chat model for the smart cleanup.
pub struct ChatModel {
    pub id: &'static str,
    pub label: &'static str,
    pub note: &'static str,
    /// Extra request fields as JSON, e.g. how hard a reasoning model may think.
    pub extra: Option<&'static str>,
}

pub struct Provider {
    pub id: &'static str,
    pub name: &'static str,
    /// Empty for an own server.
    pub base_url: &'static str,
    pub models: &'static [SpeechModel],
    pub chat_models: &'static [ChatModel],
    /// Where to get a key.
    pub key_url: &'static str,
    pub needs_key: bool,
}

impl Provider {
    pub fn default_model(&self) -> &'static str {
        self.models.first().map(|m| m.id).unwrap_or("whisper-1")
    }

    pub fn chat_model(&self, id: &str) -> Option<&'static ChatModel> {
        self.chat_models.iter().find(|m| m.id == id)
    }
}

// Prices from the providers' public pages, October 2026.
pub const GROQ: Provider = Provider {
    id: "groq",
    name: "Groq",
    base_url: "https://api.groq.com/openai/v1",
    models: &[
        SpeechModel { id: "whisper-large-v3-turbo", label: "Whisper large-v3-turbo", note: "fast · $0.04 per hour" },
        SpeechModel { id: "whisper-large-v3", label: "Whisper large-v3", note: "a little more accurate · $0.11 per hour" },
    ],
    chat_models: &[
        // Reasons first; "low" keeps that short and cheap.
        ChatModel { id: "openai/gpt-oss-20b", label: "gpt-oss 20B", note: "cheap and quick", extra: Some(r#"{"reasoning_effort":"low"}"#) },
        ChatModel { id: "llama-3.3-70b-versatile", label: "Llama 3.3 70B", note: "no reasoning, steady", extra: None },
        ChatModel { id: "llama-3.1-8b-instant", label: "Llama 3.1 8B", note: "fastest, simplest", extra: None },
    ],
    key_url: "https://console.groq.com/keys",
    needs_key: true,
};

pub const OPENAI: Provider = Provider {
    id: "openai",
    name: "OpenAI",
    base_url: "https://api.openai.com/v1",
    models: &[
        SpeechModel { id: "gpt-4o-mini-transcribe", label: "GPT-4o mini Transcribe", note: "$0.003 per minute" },
        SpeechModel { id: "gpt-4o-transcribe", label: "GPT-4o Transcribe", note: "most accurate · $0.006 per minute" },
        SpeechModel { id: "whisper-1", label: "Whisper", note: "$0.006 per minute" },
    ],
    // Not the GPT-5 family: reasoning models refuse temperature 0.
    chat_models: &[
        ChatModel { id: "gpt-4.1-nano", label: "GPT-4.1 nano", note: "cheapest", extra: None },
        ChatModel { id: "gpt-4o-mini", label: "GPT-4o mini", note: "cheap", extra: None },
        ChatModel { id: "gpt-4.1-mini", label: "GPT-4.1 mini", note: "smarter", extra: None },
    ],
    key_url: "https://platform.openai.com/api-keys",
    needs_key: true,
};

pub const CUSTOM: Provider = Provider {
    id: "custom",
    name: "Own server",
    base_url: "",
    models: &[],
    chat_models: &[],
    key_url: "",
    needs_key: false,
};

pub const ALL: [&Provider; 3] = [&GROQ, &OPENAI, &CUSTOM];

pub fn by_id(id: &str) -> Option<&'static Provider> {
    ALL.iter().copied().find(|p| p.id == id)
}

/// The address requests go to: fixed for known services, the user's for an own server.
pub fn effective_base_url(provider: &Provider, custom: &str) -> String {
    if provider.base_url.is_empty() {
        custom.trim().trim_end_matches('/').to_string()
    } else {
        provider.base_url.to_string()
    }
}

/// HTTPS by default. An explicit loopback IP may use HTTP for a local helper or test server;
/// names (including localhost) are not a guarantee about the address DNS will connect to.
pub fn check_base_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw.trim()).map_err(|_| "Not a valid address (e.g. https://example.com/v1).".to_string())?;
    if url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Err("Use a server address without user information, a query, or a fragment; enter its key separately.".into());
    }
    match url.scheme() {
        "https" => Ok(url),
        "http" if is_loopback_host(url.host_str()) => Ok(url),
        "http" => Err("Use https://; http:// is allowed only for an explicit loopback IP address.".into()),
        _ => Err("The address has to start with https://.".into()),
    }
}

pub(crate) fn is_loopback_host(host: Option<&str>) -> bool {
    host.and_then(|h| h.trim_matches(['[', ']']).parse::<std::net::IpAddr>().ok()).is_some_and(|ip| match ip {
        std::net::IpAddr::V4(v4) => v4.is_loopback(),
        std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback()),
    })
}

/// The credential's binding, excluding paths (which may themselves contain a legacy key).
/// Persist this when accepting a key, and reject/reconfirm it when the endpoint origin changes.
pub fn credential_origin(raw: &str) -> Result<String, String> {
    Ok(check_base_url(raw)?.origin().ascii_serialization())
}

pub fn same_credential_origin(a: &str, b: &str) -> bool {
    match (credential_origin(a), credential_origin(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AmberModel {
    pub id: String,
    pub label: String,
    pub note: String,
    pub recommended: bool,
}

/// An outgoing Amber provider's OpenAI model list, not a listener or pairing service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AmberCatalog {
    pub service: String,
    pub version: String,
    pub speech: Vec<AmberModel>,
    pub cleanup: Vec<AmberModel>,
    pub audio: Vec<String>,
}

impl AmberCatalog {
    pub fn parse(root: &serde_json::Value) -> Option<Self> {
        let amber = root.get("amber")?.as_object()?;
        // Display metadata is untrusted. Keep it single-line and strip control/bidi markers.
        let shown = |s: &str| -> String {
            s.chars().filter(|c| !c.is_control() && !matches!(*c, '\u{00ad}' | '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}'))
                .collect::<String>().trim().chars().take(80).collect()
        };
        let mut catalog = Self {
            service: shown(amber.get("service").and_then(|v| v.as_str()).unwrap_or("")),
            version: shown(amber.get("version").and_then(|v| v.as_str()).unwrap_or("")),
            speech: Vec::new(), cleanup: Vec::new(), audio: Vec::new(),
        };
        if let Some(audio) = amber.get("audio").and_then(|v| v.as_array()) {
            for format in audio.iter().filter_map(|v| v.as_str()).filter(|f| matches!(*f, "wav" | "opus")) {
                if !catalog.audio.iter().any(|f| f == format) {
                    catalog.audio.push(format.to_string());
                }
            }
        }
        // Older Amber catalogs omit formats; unknown-only lists also mean the WAV baseline.
        if catalog.audio.is_empty() {
            catalog.audio.push("wav".into());
        }
        for entry in root.get("data").and_then(|v| v.as_array()).into_iter().flatten() {
            let Some(id) = entry.get("id").and_then(|v| v.as_str()) else { continue };
            if id.is_empty() || id.chars().count() > 128 || id.starts_with('/') || id.contains("..")
                || !id.chars().all(|c| c.is_alphanumeric() || "._-:/@+".contains(c)) {
                continue;
            }
            let about = &entry["amber"];
            let list = match about["kind"].as_str() {
                Some("speech") => &mut catalog.speech,
                Some("cleanup") => &mut catalog.cleanup,
                _ => continue,
            };
            if list.iter().any(|m| m.id == id) { continue; }
            let label = shown(about["label"].as_str().unwrap_or(id));
            list.push(AmberModel {
                id: id.to_string(),
                label: if label.is_empty() { id.to_string() } else { label },
                note: shown(about["note"].as_str().unwrap_or("")),
                recommended: about["default"].as_bool() == Some(true) && !list.iter().any(|m| m.recommended),
            });
        }
        Some(catalog)
    }

    pub fn default_speech_model(&self) -> Option<&str> {
        self.speech.iter().find(|m| m.recommended).or_else(|| self.speech.first()).map(|m| m.id.as_str())
    }
}

/// Loopback, private ranges, CGNAT (Tailscale), link-local, single-label and .local names.
pub fn is_private_host(host: Option<&str>) -> bool {
    let Some(h) = host.map(|h| h.trim_matches(|c| c == '[' || c == ']').to_lowercase()) else {
        return false;
    };
    if h.is_empty() {
        return false;
    }
    if let Ok(ip) = h.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => {
                let [a, b, ..] = v4.octets();
                v4.is_loopback() || v4.is_private() || v4.is_link_local() || (a == 100 && (64..=127).contains(&b))
            }
            std::net::IpAddr::V6(v6) => {
                if let Some(v4) = v6.to_ipv4_mapped() {
                    return is_private_host(Some(&v4.to_string()));
                }
                let first = v6.segments()[0];
                v6.is_loopback() || (0xfc00..=0xfdff).contains(&first) || (0xfe80..=0xfebf).contains(&first)
            }
        };
    }
    !h.contains('.') || [".local", ".lan", ".home.arpa", ".internal", ".ts.net"].iter().any(|s| h.ends_with(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_consistent() {
        for p in ALL {
            assert_eq!(by_id(p.id).map(|q| q.id), Some(p.id));
            if !p.base_url.is_empty() {
                assert!(check_base_url(p.base_url).is_ok(), "{}", p.id);
                assert!(!p.models.is_empty());
            }
        }
        assert_eq!(GROQ.default_model(), "whisper-large-v3-turbo");
        for p in ALL {
            for m in p.chat_models {
                if let Some(extra) = m.extra {
                    assert!(serde_json::from_str::<serde_json::Value>(extra).unwrap().is_object(), "{}", m.id);
                }
            }
        }
        assert!(GROQ.chat_model("openai/gpt-oss-20b").unwrap().extra.is_some());
        assert_eq!(CUSTOM.default_model(), "whisper-1");
    }

    #[test]
    fn effective_url() {
        assert_eq!(effective_base_url(&GROQ, "https://evil.example"), GROQ.base_url);
        assert_eq!(effective_base_url(&CUSTOM, " http://nas:8000/v1/ "), "http://nas:8000/v1");
    }

    #[test]
    fn http_only_on_explicit_loopback() {
        for ok in ["http://127.0.0.1:8000/v1", "http://127.1.2.3/v1", "http://[::1]:9000", "http://[::ffff:127.0.0.1]/v1", "https://api.example.com/v1"] {
            assert!(check_base_url(ok).is_ok(), "{ok}");
        }
        for bad in ["http://api.example.com/v1", "http://8.8.8.8", "http://[2001:db8::1]/", "ftp://nas", "nonsense",
                    "http://[::ffff:8.8.8.8]/", "http://192.168.1.20/v1", "http://nas:8000", "http://box.local/v1",
                    "http://100.100.1.2/v1", "http://[fd12::1]/v1", "http://localhost/v1", "http://127.0.0.1.evil.test/v1",
                    "https://user:secret@example.com/v1", "https://example.com/v1?key=secret", "https://example.com/#secret"] {
            assert!(check_base_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn credentials_are_bound_to_scheme_host_and_effective_port() {
        assert!(same_credential_origin("https://EXAMPLE.com:443/v1", "https://example.com/new/v1"));
        assert_eq!(credential_origin("https://example.com/path-secret/v1").unwrap(), "https://example.com");
        for (a, b) in [
            ("https://example.com/v1", "https://other.example/v1"),
            ("https://example.com/v1", "https://example.com:8443/v1"),
            ("http://127.0.0.1:9000/v1", "http://127.0.0.1:9001/v1"),
            ("https://127.0.0.1/v1", "http://127.0.0.1/v1"),
            ("invalid", "invalid"),
        ] { assert!(!same_credential_origin(a, b)); }
    }

    #[test]
    fn amber_catalog_preserves_ids_kinds_and_first_default() {
        let catalog = AmberCatalog::parse(&serde_json::json!({
            "amber": {"service":"amber-server", "version":"1.0", "audio":["opus", "wav", "opus", "other"]},
            "data": [
                {"id":"speech/one", "amber":{"kind":"speech", "label":"Speech\n\u{202e} one", "default":true}},
                {"id":"speech/one", "amber":{"kind":"speech"}},
                {"id":"speech:two+q8", "amber":{"kind":"speech", "default":true}},
                {"id":"cleanup", "amber":{"kind":"cleanup", "default":true}},
                {"id":"../bad", "amber":{"kind":"speech"}},
                {"id":"/bad", "amber":{"kind":"speech"}},
                {"id":"line\nbreak", "amber":{"kind":"speech"}},
                {"id":"unknown", "amber":{"kind":"future"}}
            ]
        })).unwrap();
        assert_eq!(catalog.audio, ["opus", "wav"]);
        assert_eq!(catalog.speech.len(), 2);
        assert_eq!(catalog.speech[0].label, "Speech one");
        assert!(!catalog.speech[1].recommended);
        assert_eq!(catalog.default_speech_model(), Some("speech/one"));
        assert_eq!(catalog.cleanup[0].id, "cleanup");
        assert_eq!(catalog.service, "amber-server");
    }

    #[test]
    fn amber_audio_advertisement_uses_exact_known_formats() {
        for audio in [serde_json::Value::Null, serde_json::json!([]), serde_json::json!(["WAV", "audio/wav", 1])] {
            assert_eq!(AmberCatalog::parse(&serde_json::json!({"amber":{"audio":audio}})).unwrap().audio, ["wav"]);
        }
        assert_eq!(AmberCatalog::parse(&serde_json::json!({"amber":{"audio":["opus"]}})).unwrap().audio, ["opus"]);
        assert!(AmberCatalog::parse(&serde_json::json!({"data":[]})).is_none());
        assert!(AmberCatalog::parse(&serde_json::json!({"amber":"not an object"})).is_none());
    }
}
