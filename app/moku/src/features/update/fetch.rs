//! HTTPS GET for the updater only, over WinHTTP. Unlike `crate::net::get` it
//!
//! * refuses any URL outside `moku_core::release::ALLOWED_HOSTS` (HTTPS only),
//! * turns automatic redirects off and follows them itself (at most [`MAX_REDIRECTS`]), checking
//!   every `Location` against the same allowlist,
//! * streams the body to a callback with a hard size limit (the installer is far bigger than
//!   `net::MAX_BODY`), and stops early when asked to.
//!
//! Blocking: service / worker threads only.

use std::time::Duration;

use moku_core::release::is_allowed_url;
use windows::Win32::Networking::WinHttp::*;
use windows::core::{HSTRING, PCWSTR, w};

pub const MAX_REDIRECTS: usize = 5;

struct H(*mut core::ffi::c_void);
impl Drop for H {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// What the body callback is told with each chunk.
pub struct Progress {
    /// Bytes received so far (including this chunk).
    pub received: u64,
    /// `Content-Length`, when the server sent one.
    pub total: Option<u64>,
}

/// GET `url`, following allowed redirects, and hand the body to `sink` chunk by chunk.
/// `stop()` is polled between chunks. A body over `max_bytes` (announced or actual) is an error.
pub fn get(
    url: &str,
    timeout: Duration,
    max_bytes: u64,
    stop: &dyn Fn() -> bool,
    mut sink: impl FnMut(&[u8], &Progress) -> Result<(), String>,
) -> Result<(), String> {
    let agent = HSTRING::from(format!("Moku/{} (updater)", moku_core::VERSION));
    let session = H(unsafe { WinHttpOpen(&agent, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0) });
    if session.0.is_null() {
        return Err("WinHttpOpen failed".into());
    }
    let mut url = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        if !is_allowed_url(&url) {
            return Err(format!("URL not allowed: {}", redact(&url)));
        }
        match request(&session, &url, timeout, max_bytes, stop, &mut sink)? {
            Step::Done => return Ok(()),
            Step::Redirect(next) => url = resolve(&url, &next),
        }
    }
    Err("too many redirects".into())
}

enum Step {
    Done,
    Redirect(String),
}

fn request(
    session: &H,
    url: &str,
    timeout: Duration,
    max_bytes: u64,
    stop: &dyn Fn() -> bool,
    sink: &mut impl FnMut(&[u8], &Progress) -> Result<(), String>,
) -> Result<Step, String> {
    unsafe {
        let wurl: Vec<u16> = url.encode_utf16().collect();
        let mut host = vec![0u16; 256];
        let mut path = vec![0u16; 8192];
        let mut uc = URL_COMPONENTS {
            dwStructSize: size_of::<URL_COMPONENTS>() as u32,
            lpszHostName: windows::core::PWSTR(host.as_mut_ptr()),
            dwHostNameLength: host.len() as u32,
            lpszUrlPath: windows::core::PWSTR(path.as_mut_ptr()),
            dwUrlPathLength: path.len() as u32,
            ..Default::default()
        };
        WinHttpCrackUrl(&wurl, 0, &mut uc).map_err(|e| format!("bad url: {e}"))?;
        if uc.nScheme != WINHTTP_INTERNET_SCHEME_HTTPS {
            return Err("not HTTPS".into());
        }
        let host_s = String::from_utf16_lossy(&host[..uc.dwHostNameLength as usize]);
        let path_s = String::from_utf16_lossy(&path[..uc.dwUrlPathLength as usize]);

        let conn = H(WinHttpConnect(session.0, &HSTRING::from(host_s.as_str()), uc.nPort, 0));
        if conn.0.is_null() {
            return Err("connect failed".into());
        }
        let req = H(WinHttpOpenRequest(conn.0, w!("GET"), &HSTRING::from(path_s), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE));
        if req.0.is_null() {
            return Err("open request failed".into());
        }
        let t = timeout.as_millis().min(i32::MAX as u128) as i32;
        let _ = WinHttpSetTimeouts(req.0, t, t, t, t);
        // Redirects are followed by `get`, after the allowlist check.
        let never = WINHTTP_OPTION_REDIRECT_POLICY_NEVER.to_le_bytes();
        WinHttpSetOption(Some(req.0), WINHTTP_OPTION_REDIRECT_POLICY, Some(&never)).map_err(|e| format!("redirect policy: {e}"))?;
        if host_s.eq_ignore_ascii_case("api.github.com") {
            let h: Vec<u16> = "Accept: application/vnd.github+json\r\nX-GitHub-Api-Version: 2022-11-28".encode_utf16().collect();
            let _ = WinHttpAddRequestHeaders(req.0, &h, WINHTTP_ADDREQ_FLAG_ADD);
        }
        WinHttpSendRequest(req.0, None, None, 0, 0, 0).map_err(|e| format!("send: {e}"))?;
        WinHttpReceiveResponse(req.0, std::ptr::null_mut()).map_err(|e| format!("receive: {e}"))?;

        let status = query_number(&req, WINHTTP_QUERY_STATUS_CODE).unwrap_or(0);
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            let loc = query_string(&req, WINHTTP_QUERY_LOCATION).ok_or("redirect without Location")?;
            return Ok(Step::Redirect(loc));
        }
        if status != 200 {
            return Err(format!("HTTP {status}"));
        }
        let total = query_number(&req, WINHTTP_QUERY_CONTENT_LENGTH).map(u64::from);
        if total.is_some_and(|t| t > max_bytes) {
            return Err("response too large".into());
        }
        let mut received = 0u64;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            if stop() {
                return Err("stopped".into());
            }
            let mut got = 0u32;
            WinHttpReadData(req.0, buf.as_mut_ptr() as _, buf.len() as u32, &mut got).map_err(|e| format!("read: {e}"))?;
            if got == 0 {
                break;
            }
            received += got as u64;
            if received > max_bytes {
                return Err("response too large".into());
            }
            sink(&buf[..got as usize], &Progress { received, total })?;
        }
        if total.is_some_and(|t| t != received) {
            return Err(format!("download cut short ({received} of {} bytes)", total.unwrap_or(0)));
        }
        Ok(Step::Done)
    }
}

unsafe fn query_number(req: &H, what: u32) -> Option<u32> {
    let mut v = 0u32;
    let mut len = size_of::<u32>() as u32;
    unsafe { WinHttpQueryHeaders(req.0, what | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some(&mut v as *mut u32 as *mut _), &mut len, std::ptr::null_mut()) }
        .ok()
        .map(|_| v)
}

unsafe fn query_string(req: &H, what: u32) -> Option<String> {
    let mut buf = vec![0u16; 8192];
    let mut len = (buf.len() * 2) as u32;
    unsafe { WinHttpQueryHeaders(req.0, what, PCWSTR::null(), Some(buf.as_mut_ptr() as *mut _), &mut len, std::ptr::null_mut()) }.ok()?;
    Some(String::from_utf16_lossy(&buf[..(len as usize / 2).min(buf.len())]))
}

/// A `Location` may be relative to the current URL's origin.
fn resolve(current: &str, location: &str) -> String {
    let location = location.trim();
    if location.starts_with('/') && !location.starts_with("//") {
        if let Some(host) = moku_core::release::https_host(current) {
            return format!("https://{host}{location}");
        }
    }
    location.to_string()
}

/// URLs for the log: scheme, host and path only (signed CDN links carry tokens in the query).
pub fn redact(url: &str) -> String {
    url.split(['?', '#']).next().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_redirects_stay_on_the_origin() {
        assert_eq!(resolve("https://github.com/a/b", "/c/d"), "https://github.com/c/d");
        assert_eq!(resolve("https://github.com/a", "https://objects.githubusercontent.com/x?sig=1"), "https://objects.githubusercontent.com/x?sig=1");
        // Protocol-relative links are not treated as same-origin; the allowlist then rejects them.
        assert_eq!(resolve("https://github.com/a", "//evil.com/x"), "//evil.com/x");
        assert!(!is_allowed_url(&resolve("https://github.com/a", "//evil.com/x")));
        assert_eq!(redact("https://objects.githubusercontent.com/x?X-Amz-Signature=abc"), "https://objects.githubusercontent.com/x");
    }

    /// No network: a disallowed URL fails before anything is sent.
    #[test]
    fn refuses_urls_outside_the_allowlist() {
        for u in ["http://github.com/x", "https://example.com/moku-setup.exe", "https://127.0.0.1/x"] {
            let e = get(u, Duration::from_secs(1), 1024, &|| false, |_, _| Ok(())).unwrap_err();
            assert!(e.contains("not allowed"), "{u}: {e}");
        }
    }
}
