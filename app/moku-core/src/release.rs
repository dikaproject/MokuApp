//! Releases and updates: the GitHub repository, the public key releases are signed with, version
//! comparison, the "latest release" JSON, which URLs the updater may touch, and the `.sha256` /
//! `.sig` file formats. Pure Rust (no network, no Windows API): the island's `features/update`
//! does the I/O and the signature check (Windows CNG), `moku-setup` and the release tooling use
//! the asset names.

use serde::Deserialize;

/// The GitHub repository (`owner/name`) releases are published to. **The one place to change.**
///
/// While it is still the placeholder, the updater stays off (see [`updater_available`]).
pub const REPO: &str = "dikaproject/MokuApp";

/// The placeholder [`REPO`] ships with until the real repository exists.
pub const PLACEHOLDER_REPO: &str = "OWNER/moku";

/// The public key the installers of releases are signed with (ECDSA P-256, see
/// `app/tools/release-key.ps1`): the 64 bytes X || Y of the point as 128 hex digits.
///
/// Made with `app/tools/release-key.ps1 new -Out <a path OUTSIDE the repo>`; the private half is
/// the maintainer's (and the GitHub secret `MOKU_SIGNING_KEY` for release.yml). With `None` the
/// updater stays off (like the placeholder [`REPO`]) and `release.ps1` refuses to package a
/// release (`-Dev` skips that).
pub const UPDATE_PUBLIC_KEY: Option<&str> = Some("14017b6a71da1fced909f0665a7dc0cb73da109e0dbdf50c484c44f98235f1f6d9f65826542ed81d252c78d6ca680ea82775d4488bb0b55a18efdbcd48265cad");

/// The installer asset attached to every release.
pub const SETUP_ASSET: &str = "moku-setup.exe";
/// Its checksum file: `<64 hex>  moku-setup.exe` (the `sha256sum` / `Get-FileHash` format).
pub const SHA256_ASSET: &str = "moku-setup.exe.sha256";
/// Its detached signature: 128 hex digits, the ECDSA P-256 signature (r || s) over the 32-byte
/// SHA-256 of the installer, made with the private half of [`UPDATE_PUBLIC_KEY`].
pub const SIG_ASSET: &str = "moku-setup.exe.sig";

/// Hosts the updater may talk to, HTTPS only. `api.github.com` answers the release query,
/// `github.com` serves the asset links, which redirect to GitHub's download CDN
/// (`objects.githubusercontent.com`, and `release-assets.githubusercontent.com` since 2025).
pub const ALLOWED_HOSTS: [&str; 4] = ["api.github.com", "github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"];

/// True when `repo` is not a usable `owner/name` (the placeholder, empty, malformed).
pub fn is_placeholder(repo: &str) -> bool {
    let Some((owner, name)) = repo.split_once('/') else { return true };
    let ok = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    repo == PLACEHOLDER_REPO || owner.eq_ignore_ascii_case("OWNER") || !ok(owner) || !ok(name)
}

/// Is `hex` a usable public key: 128 hex digits (X || Y of a P-256 point)?
pub fn parse_public_key(hex: &str) -> Option<[u8; 64]> {
    decode_hex::<64>(hex.trim())
}

/// Does this build have everything an updater needs: a real repository AND a public key to check
/// signatures with? (False for a build of the unreleased source: the Settings → Tentang switch
/// then says "Belum aktif di build ini".)
pub fn updater_built_in(repo: &str, public_key: Option<&str>) -> bool {
    !is_placeholder(repo) && public_key.is_some_and(|k| parse_public_key(k).is_some())
}

/// Whether the updater may run at all: the user setting is on, the repository is real and the
/// build knows the key releases are signed with.
pub fn updater_available(check_setting: bool, repo: &str, public_key: Option<&str>) -> bool {
    check_setting && updater_built_in(repo, public_key)
}

/// `https://api.github.com/repos/<repo>/releases/latest`.
pub fn latest_release_api(repo: &str) -> String {
    format!("https://api.github.com/repos/{repo}/releases/latest")
}

/// `https://github.com/<repo>` (shown in Apps & Features as the support link).
pub fn repo_url(repo: &str) -> String {
    format!("https://github.com/{repo}")
}

// ------------------------------------------------------------------------------------- semver

/// A semantic version `MAJOR.MINOR.PATCH[-PRERELEASE][+BUILD]` (a leading `v` is accepted).
/// Build metadata is ignored for ordering, as semver says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Dot-separated pre-release identifiers (`["beta", "2"]`); empty for a release.
    pub pre: Vec<String>,
}

impl Version {
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim();
        let s = s.strip_prefix('v').or_else(|| s.strip_prefix('V')).unwrap_or(s);
        let s = s.split_once('+').map_or(s, |(v, _)| v);
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (s, None),
        };
        let mut it = core.split('.');
        let num = |p: Option<&str>| -> Option<u64> {
            let p = p?;
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) || (p.len() > 1 && p.starts_with('0')) {
                return None;
            }
            p.parse().ok()
        };
        let (major, minor, patch) = (num(it.next())?, num(it.next())?, num(it.next())?);
        if it.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => vec![],
            Some(p) => {
                let ids: Vec<String> = p.split('.').map(str::to_string).collect();
                if ids.iter().any(|i| i.is_empty() || !i.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')) {
                    return None;
                }
                ids
            }
        };
        Some(Version { major, minor, patch, pre })
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

impl Ord for Version {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering::*;
        let c = (self.major, self.minor, self.patch).cmp(&(o.major, o.minor, o.patch));
        if c != Equal {
            return c;
        }
        // A release ranks above any of its pre-releases.
        match (self.pre.is_empty(), o.pre.is_empty()) {
            (true, true) => return Equal,
            (true, false) => return Greater,
            (false, true) => return Less,
            _ => {}
        }
        for (a, b) in self.pre.iter().zip(&o.pre) {
            let c = match (a.parse::<u64>(), b.parse::<u64>()) {
                (Ok(x), Ok(y)) => x.cmp(&y),
                (Ok(_), Err(_)) => Less, // numeric identifiers rank below alphanumeric ones
                (Err(_), Ok(_)) => Greater,
                (Err(_), Err(_)) => a.cmp(b),
            };
            if c != Equal {
                return c;
            }
        }
        self.pre.len().cmp(&o.pre.len())
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

/// Is `candidate` newer than `current`? Unparsable input is never newer.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (Version::parse(candidate), Version::parse(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

// ------------------------------------------------------------------------------ release JSON

/// What the updater needs from `releases/latest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub version: Version,
    /// The release page (opened in the browser for portable copies).
    pub html_url: String,
    /// `browser_download_url` of [`SETUP_ASSET`].
    pub setup_url: String,
    /// `browser_download_url` of [`SHA256_ASSET`].
    pub sha256_url: String,
    /// `browser_download_url` of [`SIG_ASSET`].
    pub sig_url: String,
    /// Asset size in bytes as GitHub reports it (0 if missing).
    pub setup_size: u64,
}

#[derive(Deserialize)]
struct RawRelease {
    tag_name: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<RawAsset>,
}

#[derive(Deserialize)]
struct RawAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

/// Parse the body of `GET /repos/<repo>/releases/latest`. Drafts, pre-releases, unparsable tags,
/// missing assets and asset links outside [`ALLOWED_HOSTS`] are errors (the updater then stays
/// quiet).
pub fn parse_latest_release(json: &str) -> Result<Release, String> {
    let raw: RawRelease = serde_json::from_str(json).map_err(|e| format!("release JSON: {e}"))?;
    if raw.draft || raw.prerelease {
        return Err("latest release is a draft or pre-release".into());
    }
    let version = Version::parse(&raw.tag_name).ok_or_else(|| format!("tag {:?} is not a version", raw.tag_name))?;
    let asset = |name: &str| raw.assets.iter().find(|a| a.name == name);
    let setup = asset(SETUP_ASSET).ok_or("release has no moku-setup.exe")?;
    let sha = asset(SHA256_ASSET).ok_or("release has no moku-setup.exe.sha256")?;
    // An unsigned release is never offered: the installer would not be trusted anyway.
    let sig = asset(SIG_ASSET).ok_or("release has no moku-setup.exe.sig (unsigned)")?;
    for u in [&setup.browser_download_url, &sha.browser_download_url, &sig.browser_download_url] {
        if !is_allowed_url(u) {
            return Err(format!("asset URL not allowed: {u}"));
        }
    }
    let html_url = if is_allowed_url(&raw.html_url) { raw.html_url.clone() } else { String::new() };
    Ok(Release {
        tag: raw.tag_name.clone(),
        version,
        html_url,
        setup_url: setup.browser_download_url.clone(),
        sha256_url: sha.browser_download_url.clone(),
        sig_url: sig.browser_download_url.clone(),
        setup_size: setup.size,
    })
}

// ------------------------------------------------------------------------------ URL allowlist

/// The host of an `https://` URL, lower-cased; `None` for any other scheme, user info, a port
/// other than 443, or an empty host.
pub fn https_host(url: &str) -> Option<String> {
    let rest = url.get(..8).filter(|s| s.eq_ignore_ascii_case("https://")).map(|_| &url[8..])?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') || authority.contains('\\') {
        return None;
    }
    let host = match authority.rsplit_once(':') {
        Some((h, "443")) => h,
        Some(_) => return None,
        None => authority,
    };
    if host.is_empty() || !host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
        return None;
    }
    Some(host.trim_end_matches('.').to_ascii_lowercase())
}

/// May the updater fetch `url`? HTTPS on one of [`ALLOWED_HOSTS`] (exact host match), nothing
/// else. Checked for the API call, both asset links and every redirect.
pub fn is_allowed_url(url: &str) -> bool {
    if url.chars().any(|c| c.is_control() || c == ' ') {
        return false;
    }
    https_host(url).is_some_and(|h| ALLOWED_HOSTS.contains(&h.as_str()))
}

// ------------------------------------------------------------------------------ .sha256 files

/// The digest in a checksum file: the first token must be 64 hex digits; a file name after it,
/// if present, must be `expected_name` (optionally `*`-prefixed, the binary-mode marker).
pub fn parse_sha256_file(text: &str, expected_name: &str) -> Option<[u8; 32]> {
    let text = text.trim_start_matches('\u{feff}');
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let mut parts = line.split_whitespace();
    let hex = parts.next()?;
    if let Some(name) = parts.next() {
        let name = name.trim_start_matches('*');
        if !name.eq_ignore_ascii_case(expected_name) {
            return None;
        }
    }
    from_hex(hex)
}

/// 64 hex digits → 32 bytes.
pub fn from_hex(hex: &str) -> Option<[u8; 32]> {
    decode_hex::<32>(hex)
}

/// `2 * N` hex digits → `N` bytes.
fn decode_hex<const N: usize>(hex: &str) -> Option<[u8; N]> {
    if hex.len() != 2 * N || !hex.is_ascii() {
        return None;
    }
    let mut out = [0u8; N];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

// ------------------------------------------------------------------------------- .sig files

/// The signature in a `.sig` file: 128 hex digits (the 64 bytes r || s of an ECDSA P-256
/// signature), a BOM and surrounding whitespace tolerated. Anything else is `None`.
pub fn parse_signature_file(text: &str) -> Option<[u8; 64]> {
    decode_hex::<64>(text.trim_start_matches('\u{feff}').trim())
}

/// 32 bytes → 64 lower-case hex digits.
pub fn to_hex(d: &[u8; 32]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A well-formed key (not a real one): 64 bytes as 128 hex digits, first byte 0x7f, second 0x0c,
    /// last 0xab.
    fn key() -> String {
        format!("7f0c{}ab", "b1".repeat(61))
    }

    #[test]
    fn placeholder_slug_means_updater_off() {
        // The placeholder constant, not REPO: setting the real slug later must not break this.
        assert!(is_placeholder(PLACEHOLDER_REPO), "the placeholder is the placeholder");
        assert!(!updater_available(true, PLACEHOLDER_REPO, Some(&key())));
        assert!(!updater_available(true, "owner/moku", Some(&key())));
        assert!(!updater_available(true, "", Some(&key())));
        assert!(!updater_available(true, "moku", Some(&key())));
        assert!(!updater_available(true, "a/b/c", Some(&key())));
        assert!(!updater_available(true, "evil.com/x?y", Some(&key())));
        assert!(updater_available(true, "moku-app/moku", Some(&key())));
        assert!(!updater_available(false, "moku-app/moku", Some(&key())), "the setting turns it off");
        assert_eq!(latest_release_api("moku-app/moku"), "https://api.github.com/repos/moku-app/moku/releases/latest");
    }

    #[test]
    fn no_public_key_means_updater_off_too() {
        // Without the key releases are signed with, nothing could be verified: off, like the
        // placeholder repository.
        assert!(!updater_available(true, "moku-app/moku", None));
        assert!(!updater_built_in("moku-app/moku", None));
        assert!(!updater_available(true, "moku-app/moku", Some("not hex")));
        assert!(!updater_available(true, "moku-app/moku", Some(&key()[..126])), "a short key is no key");
        assert!(!updater_built_in(PLACEHOLDER_REPO, Some(&key())));
        assert!(updater_built_in("moku-app/moku", Some(&key())));
        // This source tree: either may still be unset (the maintainer's job), but the pair is
        // consistent with the rule.
        assert_eq!(updater_built_in(REPO, UPDATE_PUBLIC_KEY), !is_placeholder(REPO) && UPDATE_PUBLIC_KEY.is_some());
    }

    #[test]
    fn public_keys_and_signature_files() {
        let k = parse_public_key(&key()).expect("a 128-digit key");
        assert_eq!((k[0], k[63]), (0x7f, 0xab));
        assert_eq!(parse_public_key(&format!("  {}\r\n", key())).map(|k| k[1]), Some(0x0c), "whitespace is trimmed");
        assert!(parse_public_key("").is_none());
        assert!(parse_public_key(&format!("04{}", key())).is_none(), "the 0x04 uncompressed marker is not part of the constant");
        assert!(parse_public_key(&key().replace('7', "g")).is_none());
        // The signature file: 128 hex digits, BOM and a trailing newline tolerated.
        let sig = "ab".repeat(64);
        assert_eq!(parse_signature_file(&format!("{sig}\n")).map(|s| (s[0], s[63])), Some((0xab, 0xab)));
        assert!(parse_signature_file(&format!("\u{feff}{}\r\n", sig.to_uppercase())).is_some());
        assert!(parse_signature_file(&sig[..126]).is_none() && parse_signature_file("").is_none());
        assert!(parse_signature_file(&format!("{sig}{sig}")).is_none());
        assert!(parse_signature_file(&format!("{sig}  moku-setup.exe")).is_none(), "a .sig holds the signature only");
        assert!(parse_signature_file("\u{e9}\u{e9}").is_none(), "non-ASCII input never panics");
    }

    #[test]
    fn semver_compare() {
        let v = |s: &str| Version::parse(s).unwrap_or_else(|| panic!("{s}"));
        assert_eq!(v("v0.2.0"), Version { major: 0, minor: 2, patch: 0, pre: vec![] });
        assert!(v("0.2.1") > v("0.2.0"));
        assert!(v("0.10.0") > v("0.9.9"), "numeric, not lexical");
        assert!(v("1.0.0") > v("0.99.99"));
        assert!(v("1.0.0") > v("1.0.0-rc.1"), "release beats its pre-release");
        assert!(v("1.0.0-rc.1") > v("1.0.0-beta.11"));
        assert!(v("1.0.0-beta.11") > v("1.0.0-beta.2"), "numeric identifiers compare as numbers");
        assert!(v("1.0.0-alpha.1") > v("1.0.0-alpha"), "longer wins when the prefix is equal");
        assert!(v("1.0.0-alpha.beta") > v("1.0.0-alpha.1"), "alphanumeric beats numeric");
        assert_eq!(v("1.2.3+build.5"), v("1.2.3"), "build metadata is ignored");
        assert_eq!(v("1.2.3-rc.1").to_string(), "1.2.3-rc.1");
        for bad in ["", "1", "1.2", "1.2.3.4", "01.2.3", "1.2.x", "1.2.3-", "1.2.3-a..b", "v", "-1.2.3"] {
            assert!(Version::parse(bad).is_none(), "{bad:?}");
        }
        assert!(is_newer("v0.3.0", "0.2.0"));
        assert!(!is_newer("v0.2.0", "0.2.0"));
        assert!(!is_newer("v0.1.9", "0.2.0"));
        assert!(!is_newer("garbage", "0.2.0"));
        assert!(Version::parse(crate::VERSION).is_some(), "our own version parses");
    }

    fn release_json(tag: &str, setup_url: &str, extra: &str) -> String {
        let sig = format!(
            r#",
                {{ "name": "moku-setup.exe.sig", "size": 129,
                   "browser_download_url": "https://github.com/moku-app/moku/releases/download/{tag}/moku-setup.exe.sig" }}"#
        );
        release_json_with(tag, setup_url, extra, &sig)
    }

    fn release_json_with(tag: &str, setup_url: &str, extra: &str, sig: &str) -> String {
        format!(
            r#"{{
              "url": "https://api.github.com/repos/moku-app/moku/releases/1",
              "html_url": "https://github.com/moku-app/moku/releases/tag/{tag}",
              "tag_name": "{tag}", "name": "Moku {tag}", "draft": false, {extra}
              "created_at": "2026-12-01T10:00:00Z", "body": "notes",
              "assets": [
                {{ "name": "Moku-0.3.0-portable.zip", "size": 9000000,
                   "browser_download_url": "https://github.com/moku-app/moku/releases/download/{tag}/Moku-0.3.0-portable.zip" }},
                {{ "name": "moku-setup.exe", "size": 15000000, "content_type": "application/x-msdownload",
                   "browser_download_url": "{setup_url}" }},
                {{ "name": "moku-setup.exe.sha256", "size": 81,
                   "browser_download_url": "https://github.com/moku-app/moku/releases/download/{tag}/moku-setup.exe.sha256" }}{sig}
              ]
            }}"#
        )
    }

    #[test]
    fn release_json_parsing() {
        let url = "https://github.com/moku-app/moku/releases/download/v0.3.0/moku-setup.exe";
        let r = parse_latest_release(&release_json("v0.3.0", url, r#""prerelease": false,"#)).unwrap();
        assert_eq!(r.tag, "v0.3.0");
        assert_eq!(r.version, Version::parse("0.3.0").unwrap());
        assert_eq!(r.setup_url, url);
        assert!(r.sha256_url.ends_with("/moku-setup.exe.sha256"));
        assert!(r.sig_url.ends_with("/moku-setup.exe.sig"));
        assert_eq!(r.setup_size, 15_000_000);
        assert_eq!(r.html_url, "https://github.com/moku-app/moku/releases/tag/v0.3.0");

        // No signature asset: never offered.
        let e = parse_latest_release(&release_json_with("v0.3.0", url, r#""prerelease": false,"#, "")).unwrap_err();
        assert!(e.contains("unsigned"), "{e}");
        // Pre-releases and drafts are skipped.
        assert!(parse_latest_release(&release_json("v0.3.0", url, r#""prerelease": true,"#)).is_err());
        // A tag that is not a version, an asset link elsewhere, missing assets, broken JSON.
        assert!(parse_latest_release(&release_json("nightly", url, "")).is_err());
        assert!(parse_latest_release(&release_json("v0.3.0", "https://evil.example/moku-setup.exe", "")).is_err());
        assert!(parse_latest_release(&release_json("v0.3.0", "http://github.com/x/moku-setup.exe", "")).is_err());
        assert!(parse_latest_release(r#"{"tag_name": "v1.0.0", "assets": []}"#).is_err());
        assert!(parse_latest_release("{not json").is_err());
        assert!(parse_latest_release(r#"{"message": "Not Found"}"#).is_err());
    }

    #[test]
    fn url_allowlist() {
        for ok in [
            "https://api.github.com/repos/a/b/releases/latest",
            "https://github.com/a/b/releases/download/v1.0.0/moku-setup.exe",
            "https://objects.githubusercontent.com/github-production-release-asset/1?X-Amz=1",
            "https://release-assets.githubusercontent.com/github-production-release-asset/1?sp=r",
            "HTTPS://GitHub.com/a/b",
            "https://github.com:443/a",
        ] {
            assert!(is_allowed_url(ok), "{ok}");
        }
        for bad in [
            "http://github.com/a/b",
            "https://github.com.evil.com/a",
            "https://evilgithub.com/a",
            "https://evil.com/github.com",
            "https://github.com@evil.com/a",
            "https://user:pw@github.com/a",
            "https://github.com:8443/a",
            "https://raw.githubusercontent.com/a/b/main/x",
            "https://gist.github.com/a",
            "file:///C:/x.exe",
            "ftp://github.com/x",
            "https://github.com/a b",
            "https://github.com\\@evil.com",
            "https:/github.com/a",
            "https://",
            "",
        ] {
            assert!(!is_allowed_url(bad), "{bad}");
        }
    }

    #[test]
    fn sha256_file_formats() {
        let hex = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let d = from_hex(hex).unwrap();
        assert_eq!(to_hex(&d), hex);
        assert_eq!(parse_sha256_file(&format!("{hex}  moku-setup.exe\n"), SETUP_ASSET), Some(d));
        assert_eq!(parse_sha256_file(&format!("{}\r\n", hex.to_uppercase()), SETUP_ASSET), Some(d));
        assert_eq!(parse_sha256_file(&format!("\u{feff}{hex} *moku-setup.exe"), SETUP_ASSET), Some(d));
        assert_eq!(parse_sha256_file(&format!("{hex}  other.exe"), SETUP_ASSET), None, "checksum of another file");
        assert_eq!(parse_sha256_file(&hex[..63], SETUP_ASSET), None);
        assert_eq!(parse_sha256_file(&format!("{}zz", &hex[..62]), SETUP_ASSET), None);
        assert_eq!(parse_sha256_file("", SETUP_ASSET), None);
    }
}
