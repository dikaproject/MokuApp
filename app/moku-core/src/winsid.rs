//! User SIDs, for the pipe: the per-user pipe name (`\.\pipe\moku-<SID>`) and the client's check
//! that the program listening on the pipe runs as the same user (`ipc::send_to`). The only
//! Windows API in this crate.

use std::sync::OnceLock;

use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSidToSidW};
use windows::Win32::Security::{EqualSid, GetLengthSid, GetTokenInformation, IsValidSid, PSID, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::core::{HSTRING, PWSTR};

/// A security identifier (its binary form).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sid(Vec<u8>);

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

impl Sid {
    fn psid(&self) -> PSID {
        PSID(self.0.as_ptr() as *mut _)
    }

    /// The user of an access token (`TokenUser`).
    fn of_token(token: HANDLE) -> Option<Sid> {
        unsafe {
            let mut len = 0u32;
            let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
            if len == 0 || len > 4096 {
                return None;
            }
            let mut buf = vec![0u8; len as usize];
            GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr() as _), len, &mut len).ok()?;
            let user = &*(buf.as_ptr() as *const TOKEN_USER);
            let sid = user.User.Sid;
            if !IsValidSid(sid).as_bool() {
                return None;
            }
            let n = GetLengthSid(sid) as usize;
            Some(Sid(std::slice::from_raw_parts(sid.0 as *const u8, n).to_vec()))
        }
    }

    /// The user this process runs as.
    pub fn current_user() -> Option<Sid> {
        static CURRENT: OnceLock<Option<Sid>> = OnceLock::new();
        CURRENT
            .get_or_init(|| unsafe {
                let mut token = HANDLE::default();
                OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
                let token = Handle(token);
                Sid::of_token(token.0)
            })
            .clone()
    }

    /// The user process `pid` runs as; `None` when it cannot be opened (another user's process,
    /// a protected one, one that is gone).
    pub fn of_process(pid: u32) -> Option<Sid> {
        unsafe {
            // The limited right is enough on current Windows; ask for the full one if it is not.
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).or_else(|_| OpenProcess(PROCESS_QUERY_INFORMATION, false, pid)).ok()?;
            let process = Handle(process);
            let mut token = HANDLE::default();
            OpenProcessToken(process.0, TOKEN_QUERY, &mut token).ok()?;
            let token = Handle(token);
            Sid::of_token(token.0)
        }
    }

    /// `S-1-5-21-…` form (for the pipe name).
    pub fn to_text(&self) -> Option<String> {
        unsafe {
            let mut s = PWSTR::null();
            ConvertSidToStringSidW(self.psid(), &mut s).ok()?;
            let text = s.to_string().ok();
            let _ = LocalFree(Some(HLOCAL(s.0 as _)));
            text
        }
    }

    /// Parse the `S-1-…` form (tests, well-known SIDs).
    pub fn from_text(text: &str) -> Option<Sid> {
        unsafe {
            let mut sid = PSID::default();
            ConvertStringSidToSidW(&HSTRING::from(text), &mut sid).ok()?;
            let n = GetLengthSid(sid) as usize;
            let bytes = std::slice::from_raw_parts(sid.0 as *const u8, n).to_vec();
            let _ = LocalFree(Some(HLOCAL(sid.0 as _)));
            Some(Sid(bytes))
        }
    }

    pub fn equals(&self, other: &Sid) -> bool {
        unsafe { EqualSid(self.psid(), other.psid()).is_ok() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_runs_as_the_current_user() {
        let me = Sid::current_user().expect("the current user");
        let text = me.to_text().expect("text form");
        assert!(text.starts_with("S-1-"), "{text}");
        assert_eq!(Sid::from_text(&text).as_ref(), Some(&me), "the text form round-trips");
        let mine = Sid::of_process(std::process::id()).expect("own process");
        assert!(mine.equals(&me));
    }

    #[test]
    fn other_users_and_missing_processes_are_not_me() {
        let me = Sid::current_user().unwrap();
        let system = Sid::from_text("S-1-5-18").expect("LocalSystem");
        assert!(!system.equals(&me), "the test must not run as SYSTEM");
        assert!(Sid::from_text("not a sid").is_none());
        // Process 0 and a pid nobody has: unknown, never "me".
        assert!(Sid::of_process(0).is_none());
        assert!(Sid::of_process(u32::MAX - 2).is_none());
    }
}
