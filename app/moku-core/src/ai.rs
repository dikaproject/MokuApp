//! "Tanya AI" (v0.3 track M): what the island and the settings app both need to know about
//! providers. Pure Rust, no network and no Windows API.
//!
//! * the presets (base URL + a suggested model) of the OpenAI-compatible providers,
//! * base-URL handling: parsing, the host shown on every card, loopback detection, the
//!   `/chat/completions` and `/models` URLs, and the rule that a key only travels over HTTPS
//!   (plain `http://` is accepted for loopback only),
//! * the Credential Manager name of the API key: **one key per provider host**, so switching
//!   provider can never send the old provider's key to the new host,
//! * the privacy note (Indonesian and English), shown in the first-run setup, on the AI page and
//!   on the card.

/// One provider preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    pub base_url: &'static str,
    /// A model that exists on that provider at the time of writing; the user can change it.
    pub model: &'static str,
    /// Runs on this PC: nothing leaves it, and no key is needed.
    pub local: bool,
}

/// The presets, in the order the settings app lists them.
pub const PRESETS: [Preset; 7] = [
    Preset { id: "openai", name: "OpenAI", base_url: "https://api.openai.com/v1", model: "gpt-4o-mini", local: false },
    Preset { id: "openrouter", name: "OpenRouter", base_url: "https://openrouter.ai/api/v1", model: "openai/gpt-4o-mini", local: false },
    Preset { id: "groq", name: "Groq", base_url: "https://api.groq.com/openai/v1", model: "llama-3.3-70b-versatile", local: false },
    Preset { id: "gemini", name: "Gemini (endpoint OpenAI)", base_url: "https://generativelanguage.googleapis.com/v1beta/openai", model: "gemini-2.0-flash", local: false },
    Preset { id: "deepseek", name: "DeepSeek", base_url: "https://api.deepseek.com/v1", model: "deepseek-chat", local: false },
    Preset { id: "ollama", name: "Ollama (di PC ini)", base_url: "http://localhost:11434/v1", model: "llama3.2", local: true },
    Preset { id: "lmstudio", name: "LM Studio (di PC ini)", base_url: "http://localhost:1234/v1", model: "local-model", local: true },
];

/// The preset with this id (`"custom"` and unknown ids: none).
pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// The preset whose base URL is `url` (trailing slash and case ignored), if any.
pub fn preset_for_url(url: &str) -> Option<&'static Preset> {
    let u = url.trim().trim_end_matches('/').to_ascii_lowercase();
    PRESETS.iter().find(|p| p.base_url.to_ascii_lowercase() == u)
}

/// The privacy note, Indonesian.
pub const PRIVACY_NOTE_ID: &str = "Memori Moku hanya disimpan di PC ini dan dienkripsi untuk akun Windows-mu. Saat kamu memakai Tanya AI, teks yang kamu pilih dan konteks yang kamu izinkan dikirim ke penyedia AI pilihanmu (host ditampilkan). Setelah itu, data mengikuti kebijakan penyedia tersebut; sebagian paket gratis memakai data untuk meningkatkan produk mereka. Moku tidak punya server dan tidak mengirim apa pun ke kami. Pakai Ollama atau LM Studio agar tidak ada data yang keluar dari PC.";
/// The privacy note, English.
pub const PRIVACY_NOTE_EN: &str = "Your memory stays on this PC, encrypted for your Windows account. When you use Ask AI, the selected text and the context you allow are sent to the provider you chose (host shown). From there, privacy depends on your AI provider; some free plans use data to improve their products. Moku has no server and sends nothing to us. Use Ollama or LM Studio to keep everything on this PC.";

/// A parsed base URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseUrl {
    pub https: bool,
    /// Lower-case host without brackets.
    pub host: String,
    pub port: Option<u16>,
    /// Path without a trailing slash (`"/v1"`, or `""`).
    pub path: String,
}

/// Parse and check a base URL: `https://host[:port][/path]`, or `http://` for loopback only. No
/// user name or password in the URL, no query or fragment.
pub fn parse_base_url(url: &str) -> Result<BaseUrl, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("Alamat kosong".into());
    }
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("Alamat tidak boleh berisi spasi".into());
    }
    let lower = url.to_ascii_lowercase();
    let (https, rest) = if let Some(r) = lower.strip_prefix("https://") {
        (true, &url[url.len() - r.len()..])
    } else if let Some(r) = lower.strip_prefix("http://") {
        (false, &url[url.len() - r.len()..])
    } else {
        return Err("Alamat harus diawali https:// (atau http:// untuk PC ini)".into());
    };
    if rest.contains(['?', '#']) {
        return Err("Alamat dasar tidak boleh memakai ? atau #".into());
    }
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if authority.contains('@') {
        return Err("Jangan taruh nama atau kata sandi di alamat; kunci diisi terpisah".into());
    }
    let (host, port) = split_host_port(authority)?;
    if host.is_empty() || host.len() > 253 {
        return Err("Alamat tidak punya nama host".into());
    }
    if !host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | ':' | '_')) {
        return Err("Nama host tidak valid".into());
    }
    let b = BaseUrl { https, host: host.to_ascii_lowercase(), port, path: path.trim_end_matches('/').to_string() };
    if !https && !is_local_host(&b.host) {
        return Err("Alamat http:// hanya boleh untuk PC ini (localhost); pakai https:// agar kunci tidak terkirim polos".into());
    }
    Ok(b)
}

fn split_host_port(authority: &str) -> Result<(String, Option<u16>), String> {
    if let Some(rest) = authority.strip_prefix('[') {
        // [::1]:8080
        let (h, after) = rest.split_once(']').ok_or("Alamat IPv6 tidak lengkap")?;
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p.parse::<u16>().map_err(|_| "Port tidak valid")?),
            None if after.is_empty() => None,
            None => return Err("Alamat tidak valid".into()),
        };
        return Ok((h.to_string(), port));
    }
    match authority.rsplit_once(':') {
        Some((h, p)) => Ok((h.to_string(), Some(p.parse::<u16>().ok().filter(|p| *p != 0).ok_or("Port tidak valid")?))),
        None => Ok((authority.to_string(), None)),
    }
}

/// Is this host the PC itself: exactly `localhost`, `127.0.0.0/8`, `::1`? Not `*.localhost`: a name under
/// `localhost` is resolved by the system's DNS resolver on some setups (RFC 6761 only recommends that it
/// stays local), and "local" here means "nothing leaves this PC" (no key needed, no proxy, http allowed).
pub fn is_local_host(host: &str) -> bool {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
    if h == "localhost" || h == "::1" {
        return true;
    }
    let parts: Vec<&str> = h.split('.').collect();
    parts.len() == 4 && parts[0] == "127" && parts.iter().all(|p| !p.is_empty() && p.len() <= 3 && p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u16>().is_ok_and(|n| n <= 255))
}

/// Is the provider on this PC (so nothing leaves it)? An unusable URL is *not* local.
pub fn is_local(base_url: &str) -> bool {
    parse_base_url(base_url).is_ok_and(|b| is_local_host(&b.host))
}

/// The host as shown on the card, "Dikirim ke <host>": `api.openai.com`, `localhost:11434`.
pub fn host_label(base_url: &str) -> Option<String> {
    let b = parse_base_url(base_url).ok()?;
    let host = if b.host.contains(':') { format!("[{}]", b.host) } else { b.host.clone() };
    Some(match b.port {
        Some(p) if !(p == 443 && b.https) && !(p == 80 && !b.https) => format!("{host}:{p}"),
        _ => host,
    })
}

fn join(base_url: &str, tail: &str) -> Result<String, String> {
    let b = parse_base_url(base_url)?;
    let scheme = if b.https { "https" } else { "http" };
    let host = if b.host.contains(':') { format!("[{}]", b.host) } else { b.host.clone() };
    let port = b.port.map(|p| format!(":{p}")).unwrap_or_default();
    Ok(format!("{scheme}://{host}{port}{}{tail}", b.path))
}

/// `<base>/chat/completions`.
pub fn chat_url(base_url: &str) -> Result<String, String> {
    join(base_url, "/chat/completions")
}

/// `<base>/models` (the connection test).
pub fn models_url(base_url: &str) -> Result<String, String> {
    join(base_url, "/models")
}

/// The Credential Manager name (see `secrets`) of the key for this provider host:
/// `ai/api.openai.com` (the target is then `Moku/ai/api.openai.com`). One key per host, so a
/// key typed for one provider is never sent to another.
pub fn key_name(base_url: &str) -> Option<String> {
    Some(format!("ai/{}", host_label(base_url)?))
}

/// Does `model` (as configured) appear in the ids of a `/models` answer? Some providers prefix
/// ids with `models/` (Gemini), so a trailing match counts.
pub fn model_listed(ids: &[String], model: &str) -> bool {
    let m = model.trim();
    !m.is_empty() && ids.iter().any(|id| id == m || id.ends_with(&format!("/{m}")))
}

/// The ids in a `/models` JSON answer (`{"data":[{"id":"…"}]}`), at most 2000.
pub fn parse_model_ids(json: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return vec![] };
    v.get("data")
        .and_then(|d| d.as_array())
        .map(|a| a.iter().filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(str::to_string)).take(2000).collect())
        .unwrap_or_default()
}

/// A short message for an HTTP status of a provider (Indonesian), for the connection test and
/// the card.
pub fn status_message(status: u32) -> &'static str {
    match status {
        200..=299 => "Tersambung",
        400 => "Permintaan ditolak penyedia (cek nama model)",
        401 | 403 => "Kunci API ditolak (salah, kedaluwarsa, atau tidak punya akses)",
        404 => "Alamat atau model tidak ditemukan (cek base URL dan nama model)",
        408 | 504 => "Penyedia terlalu lama menjawab",
        429 => "Terlalu banyak permintaan atau kuota habis",
        500..=599 => "Penyedia sedang bermasalah",
        300..=399 => "Alamat dialihkan; Moku tidak mengikuti pengalihan. Pakai alamat yang tepat",
        _ => "Penyedia menjawab dengan kesalahan",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_valid_and_unique() {
        let mut ids = std::collections::HashSet::new();
        for p in PRESETS {
            assert!(ids.insert(p.id), "duplicate id {}", p.id);
            let b = parse_base_url(p.base_url).unwrap_or_else(|e| panic!("{}: {e}", p.id));
            assert_eq!(is_local_host(&b.host), p.local, "{}", p.id);
            assert!(!p.model.is_empty());
            assert!(chat_url(p.base_url).unwrap().ends_with("/chat/completions"));
            assert_eq!(preset(p.id), Some(&p));
            assert_eq!(preset_for_url(&format!("{}/", p.base_url.to_uppercase())), Some(&p), "{}: slash and case ignored", p.id);
        }
        assert_eq!(PRESETS.len(), 7);
        let names: Vec<&str> = PRESETS.iter().map(|p| p.id).collect();
        assert_eq!(names, ["openai", "openrouter", "groq", "gemini", "deepseek", "ollama", "lmstudio"]);
        assert_eq!(preset("ollama").unwrap().base_url, "http://localhost:11434/v1");
        assert_eq!(preset("lmstudio").unwrap().base_url, "http://localhost:1234/v1");
        assert_eq!(preset("custom"), None);
    }

    #[test]
    fn localhost_detection() {
        for h in ["localhost", "LOCALHOST", "127.0.0.1", "127.255.0.9", "::1", "[::1]"] {
            assert!(is_local_host(h), "{h}");
        }
        // `*.localhost` is a name that something else may resolve: not "this PC".
        for h in ["ai.localhost", "a.b.localhost", "localhost.", ".localhost"] {
            assert!(!is_local_host(h), "{h}");
        }
        assert!(!is_local("http://ai.localhost:11434/v1"));
        for h in ["api.openai.com", "localhost.evil.com", "127.0.0.1.evil.com", "128.0.0.1", "10.0.0.5", "192.168.1.2", "0.0.0.0", "127.0.0", "127.0.0.256", "evil-localhost", ""] {
            assert!(!is_local_host(h), "{h}");
        }
        assert!(is_local("http://localhost:11434/v1"));
        assert!(is_local("http://127.0.0.1:8080"));
        assert!(is_local("https://localhost/v1"));
        assert!(is_local("http://[::1]:1234/v1"));
        assert!(!is_local("https://api.openai.com/v1"));
        // An address that is not usable is never "local".
        assert!(!is_local("not a url"));
        assert!(!is_local(""));
        // A trick: userinfo that looks like localhost.
        assert!(!is_local("https://localhost@evil.com/v1"));
        assert!(!is_local("https://evil.com/localhost"));
    }

    #[test]
    fn base_urls_are_parsed_and_checked() {
        let b = parse_base_url(" HTTPS://API.OpenAI.com/v1/ ").unwrap();
        assert_eq!((b.https, b.host.as_str(), b.port, b.path.as_str()), (true, "api.openai.com", None, "/v1"));
        let b = parse_base_url("http://localhost:11434/v1").unwrap();
        assert_eq!((b.https, b.host.as_str(), b.port), (false, "localhost", Some(11434)));
        let b = parse_base_url("http://[::1]:8080").unwrap();
        assert_eq!((b.host.as_str(), b.port, b.path.as_str()), ("::1", Some(8080), ""));
        for bad in [
            "",
            "   ",
            "api.openai.com/v1",
            "ftp://x.com",
            "http://api.openai.com/v1",         // a key over plain http to a remote host
            "https://user:pass@api.openai.com", // credentials in the URL
            "https://a b.com",
            "https://api.openai.com/v1?x=1",
            "https://api.openai.com/v1#frag",
            "https://",
            "https://:443",
            "https://host:0",
            "https://host:99999",
            "https://host:abc",
            "https://[::1",
            "https://ho$t.com",
        ] {
            assert!(parse_base_url(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn urls_and_host_labels() {
        assert_eq!(chat_url("https://api.openai.com/v1").unwrap(), "https://api.openai.com/v1/chat/completions");
        assert_eq!(chat_url("https://api.openai.com/v1/").unwrap(), "https://api.openai.com/v1/chat/completions");
        assert_eq!(models_url("http://localhost:1234/v1").unwrap(), "http://localhost:1234/v1/models");
        assert_eq!(chat_url("https://api.deepseek.com").unwrap(), "https://api.deepseek.com/chat/completions");
        assert_eq!(chat_url("http://[::1]:8080/v1").unwrap(), "http://[::1]:8080/v1/chat/completions");
        assert!(chat_url("http://evil.example/v1").is_err());
        assert_eq!(host_label("https://api.openai.com/v1").as_deref(), Some("api.openai.com"));
        assert_eq!(host_label("https://api.openai.com:443/v1").as_deref(), Some("api.openai.com"), "default port hidden");
        assert_eq!(host_label("http://localhost:11434/v1").as_deref(), Some("localhost:11434"));
        assert_eq!(host_label("https://gw.example.com:8443").as_deref(), Some("gw.example.com:8443"));
        assert_eq!(host_label("nonsense"), None);
    }

    #[test]
    fn one_key_per_provider_host() {
        let a = key_name("https://api.openai.com/v1").unwrap();
        let b = key_name("https://openrouter.ai/api/v1").unwrap();
        assert_eq!(a, "ai/api.openai.com");
        assert_ne!(a, b, "switching provider never reuses the other provider's key");
        assert_eq!(key_name("https://API.OPENAI.COM/v1/"), Some(a), "same host, same key");
        assert_ne!(key_name("http://localhost:11434/v1"), key_name("http://localhost:1234/v1"));
        assert_eq!(key_name("garbage"), None);
        assert_eq!(crate::secrets::target(&key_name("https://api.openai.com/v1").unwrap()), "Moku/ai/api.openai.com");
    }

    #[test]
    fn privacy_note_says_what_the_plan_says() {
        for (t, must) in [
            (PRIVACY_NOTE_ID, ["dienkripsi", "penyedia AI pilihanmu", "tidak punya server", "Ollama"]),
            (PRIVACY_NOTE_EN, ["encrypted", "privacy depends on your AI provider", "no server", "Ollama"]),
        ] {
            for m in must {
                assert!(t.contains(m), "{m:?} missing in {t:?}");
            }
        }
    }

    #[test]
    fn models_list_parsing() {
        let ids = parse_model_ids(r#"{"object":"list","data":[{"id":"gpt-4o-mini"},{"id":"models/gemini-2.0-flash"},{"x":1}]}"#);
        assert_eq!(ids, ["gpt-4o-mini", "models/gemini-2.0-flash"]);
        assert!(model_listed(&ids, "gpt-4o-mini"));
        assert!(model_listed(&ids, "gemini-2.0-flash"), "Gemini prefixes ids with models/");
        assert!(!model_listed(&ids, "mini"));
        assert!(!model_listed(&ids, ""));
        assert!(parse_model_ids("not json").is_empty());
        assert!(parse_model_ids(r#"{"data":"x"}"#).is_empty());
    }

    #[test]
    fn status_messages() {
        assert_eq!(status_message(200), "Tersambung");
        assert!(status_message(401).contains("Kunci"));
        assert!(status_message(404).contains("base URL"));
        assert!(status_message(429).contains("kuota"));
        assert!(status_message(302).contains("pengalihan"));
        assert!(status_message(503).contains("bermasalah"));
    }
}
