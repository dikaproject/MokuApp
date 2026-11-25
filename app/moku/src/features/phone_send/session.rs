//! The state of one transfer session: the one-time token, its expiry, the one-device lock, the
//! approval the user gives on the PC, and what the phone announced. Pure (the clock is passed in),
//! so every rule is unit-tested; the server keeps it behind a mutex.
//!
//! The rules, in the order a request meets them:
//!
//! 1. **Token.** 128 random bits (CNG), base64url in the first path segment, compared in constant
//!    time. A wrong or missing token is answered like any unknown path.
//! 2. **Expiry.** 10 minutes from the start (hard limit), shortened to a minute (receive) or 20 s
//!    (send) after a transfer completed. A transfer that is running is never cut off by it, and
//!    keeps the session alive while it runs.
//! 3. **One device.** The first IP that presents the valid token owns the session; every other IP
//!    is refused from then on. A wrong token never locks anything.
//! 4. **Approval.** Nothing is sent or stored before the user pressed "Terima" on the island. "Tolak"
//!    (or no answer for 90 s) ends the session.

use std::net::IpAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use windows::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};

use super::names;

/// How long a session lives (hard limit).
pub const TTL: Duration = Duration::from_secs(10 * 60);
/// What is left of the session after a completed transfer.
pub const LINGER_RECEIVE: Duration = Duration::from_secs(60);
pub const LINGER_SEND: Duration = Duration::from_secs(20);
/// How long the island waits for "Terima" / "Tolak".
pub const APPROVAL_WAIT: Duration = Duration::from_secs(90);
/// Largest single file accepted (2 GiB).
pub const MAX_FILE: u64 = 2 * 1024 * 1024 * 1024;
/// Largest announced batch, and most files in one.
pub const MAX_BATCH: u64 = 4 * 1024 * 1024 * 1024;
pub const MAX_FILES: usize = 50;
/// Longest text accepted from the phone (bytes).
pub const MAX_TEXT: usize = 100 * 1024;

// ---- token ----------------------------------------------------------------------------------------

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// base64url without padding: 16 bytes -> 22 characters.
pub fn encode_token(bytes: &[u8; 16]) -> String {
    let mut out = String::with_capacity(22);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        let chars = chunk.len() + 1; // 1 byte -> 2 chars, 2 -> 3, 3 -> 4
        for i in 0..chars {
            out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

/// A fresh token from the system RNG (128 bits).
pub fn new_token() -> Result<String, String> {
    let mut b = [0u8; 16];
    let st = unsafe { BCryptGenRandom(None, &mut b, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if st.is_err() {
        return Err(format!("BCryptGenRandom failed (0x{:08X})", st.0));
    }
    Ok(encode_token(&b))
}

/// Constant-time equality (the time depends on the length only, never on where the first
/// difference is).
pub fn token_matches(expected: &str, presented: &str) -> bool {
    let (a, b) = (expected.as_bytes(), presented.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

// ---- model ----------------------------------------------------------------------------------------

/// A file the PC offers to the phone.
#[derive(Clone, Debug, PartialEq)]
pub struct SendFile {
    pub path: PathBuf,
    /// Sanitised name (what the phone saves it as).
    pub name: String,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Mode {
    /// "Kirim ke HP": the phone may download exactly these files.
    Send(Vec<SendFile>),
    /// "Terima dari HP": the phone may upload files and text.
    Receive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Approval {
    /// Nobody asked yet.
    None,
    Pending,
    Allowed,
    Denied,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileStatus {
    Waiting,
    Receiving,
    Done,
    Failed,
}

/// A file the phone announced.
#[derive(Clone, Debug, PartialEq)]
pub struct Incoming {
    /// Sanitised name.
    pub name: String,
    pub size: u64,
    pub status: FileStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndReason {
    Expired,
    Denied,
    /// The user pressed stop / started another session.
    Stopped,
    /// Nobody answered the approval card.
    ApprovalTimeout,
}

/// Why a request was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    BadToken,
    Expired,
    OtherDevice,
    NotApproved,
    WrongMode,
    BadOffer(&'static str),
    NotFound,
    /// The thing asked for is already in progress or done.
    Conflict,
}

/// What the island must show after an offer.
#[derive(Clone, Debug, PartialEq)]
pub struct OfferSummary {
    pub ip: IpAddr,
    /// Names are sanitised; sizes are what the phone announced.
    pub files: Vec<(String, u64)>,
    pub text: bool,
    /// False when the device was approved earlier in this session (nothing to ask).
    pub needs_approval: bool,
}

#[derive(Debug)]
pub struct Session {
    token: String,
    mode: Mode,
    hard_deadline: Instant,
    deadline: Instant,
    device: Option<IpAddr>,
    approval: Approval,
    asked_at: Option<Instant>,
    active: u32,
    incoming: Vec<Incoming>,
    /// The phone announced text and it has not arrived yet.
    text_wanted: bool,
    ended: Option<EndReason>,
}

impl Session {
    pub fn new(token: String, mode: Mode, now: Instant) -> Session {
        Session {
            token,
            mode,
            hard_deadline: now + TTL,
            deadline: now + TTL,
            device: None,
            approval: Approval::None,
            asked_at: None,
            active: 0,
            incoming: vec![],
            text_wanted: false,
            ended: None,
        }
    }

    pub fn token(&self) -> &str {
        &self.token
    }
    pub fn mode(&self) -> &Mode {
        &self.mode
    }
    pub fn device(&self) -> Option<IpAddr> {
        self.device
    }
    pub fn approval(&self) -> Approval {
        self.approval
    }
    pub fn incoming(&self) -> &[Incoming] {
        &self.incoming
    }
    pub fn ended(&self) -> Option<EndReason> {
        self.ended
    }

    /// Ended or past its deadline with nothing running.
    pub fn is_expired(&self, now: Instant) -> bool {
        self.ended.is_some() || (now >= self.deadline && self.active == 0)
    }

    /// Time left on the clock (zero while expired; a running transfer does not hold it above zero).
    pub fn remaining(&self, now: Instant) -> Duration {
        self.deadline.saturating_duration_since(now)
    }

    /// Steps 1-3 for every request: the token, the expiry and the one-device lock.
    pub fn admit(&mut self, presented: &str, ip: IpAddr, now: Instant) -> Result<(), Reject> {
        // A wrong token learns nothing and locks nothing.
        if !token_matches(&self.token, presented) {
            return Err(Reject::BadToken);
        }
        if self.is_expired(now) {
            return Err(Reject::Expired);
        }
        match self.device {
            None => {
                self.device = Some(ip);
                Ok(())
            }
            Some(d) if d == ip => Ok(()),
            Some(_) => Err(Reject::OtherDevice),
        }
    }

    /// Housekeeping, about once a second: an unanswered approval ends the session; so does the clock.
    /// Returns the reason the first time the session ends.
    pub fn tick(&mut self, now: Instant) -> Option<EndReason> {
        if self.ended.is_some() {
            return None;
        }
        if self.approval == Approval::Pending && self.asked_at.is_some_and(|t| now.duration_since(t) >= APPROVAL_WAIT) {
            self.approval = Approval::Denied;
            self.ended = Some(EndReason::ApprovalTimeout);
        } else if now >= self.deadline && self.active == 0 {
            self.ended = Some(EndReason::Expired);
        }
        self.ended
    }

    pub fn stop(&mut self) {
        if self.ended.is_none() {
            self.ended = Some(EndReason::Stopped);
        }
    }

    // ---- approval ---------------------------------------------------------------------------------

    /// The user pressed "Terima".
    pub fn approve(&mut self) {
        if self.approval == Approval::Pending {
            self.approval = Approval::Allowed;
        }
    }

    /// The user pressed "Tolak": the session is over (the device must start again with a new QR).
    pub fn deny(&mut self) {
        if self.approval == Approval::Pending {
            self.approval = Approval::Denied;
            self.ended = Some(EndReason::Denied);
        }
    }

    fn ask(&mut self, now: Instant) -> bool {
        match self.approval {
            Approval::Allowed => false,
            Approval::None => {
                self.approval = Approval::Pending;
                self.asked_at = Some(now);
                true
            }
            Approval::Pending | Approval::Denied => true,
        }
    }

    // ---- the phone sends ----------------------------------------------------------------------------

    /// The phone announces files (and/or a text). Names are sanitised here; sizes and counts are
    /// checked. Returns what the approval card must say.
    pub fn offer(&mut self, ip: IpAddr, files: &[(String, u64)], text: bool, now: Instant) -> Result<OfferSummary, Reject> {
        if self.mode != Mode::Receive {
            return Err(Reject::WrongMode);
        }
        if files.is_empty() && !text {
            return Err(Reject::BadOffer("nothing offered"));
        }
        if files.len() > MAX_FILES {
            return Err(Reject::BadOffer("too many files"));
        }
        if files.iter().any(|(_, s)| *s > MAX_FILE) {
            return Err(Reject::BadOffer("file too large"));
        }
        if files.iter().map(|(_, s)| *s).sum::<u64>() > MAX_BATCH {
            return Err(Reject::BadOffer("batch too large"));
        }
        // Not while the previous batch is still running.
        if self.incoming.iter().any(|f| matches!(f.status, FileStatus::Waiting | FileStatus::Receiving)) || self.text_wanted {
            return Err(Reject::Conflict);
        }
        let clean: Vec<(String, u64)> = files.iter().map(|(n, s)| (names::sanitize(n), *s)).collect();
        self.incoming = clean.iter().map(|(n, s)| Incoming { name: n.clone(), size: *s, status: FileStatus::Waiting }).collect();
        self.text_wanted = text;
        self.deadline = self.hard_deadline;
        let needs_approval = self.ask(now);
        Ok(OfferSummary { ip, files: clean, text, needs_approval })
    }

    /// The phone wants to download (send mode): opens the approval.
    pub fn request_download(&mut self, ip: IpAddr, now: Instant) -> Result<OfferSummary, Reject> {
        let Mode::Send(files) = &self.mode else { return Err(Reject::WrongMode) };
        let files = files.iter().map(|f| (f.name.clone(), f.size)).collect();
        let needs_approval = self.ask(now);
        Ok(OfferSummary { ip, files, text: false, needs_approval })
    }

    /// Start receiving file `i`: it must be announced, approved and not started yet (a failed one
    /// may be tried again).
    pub fn start_upload(&mut self, i: usize) -> Result<Incoming, Reject> {
        if self.mode != Mode::Receive {
            return Err(Reject::WrongMode);
        }
        if self.approval != Approval::Allowed {
            return Err(Reject::NotApproved);
        }
        let f = self.incoming.get_mut(i).ok_or(Reject::NotFound)?;
        match f.status {
            FileStatus::Waiting | FileStatus::Failed => {
                f.status = FileStatus::Receiving;
                self.active += 1;
                self.deadline = self.hard_deadline;
                Ok(self.incoming[i].clone())
            }
            FileStatus::Receiving | FileStatus::Done => Err(Reject::Conflict),
        }
    }

    pub fn finish_upload(&mut self, i: usize, ok: bool, now: Instant) {
        if let Some(f) = self.incoming.get_mut(i)
            && f.status == FileStatus::Receiving
        {
            f.status = if ok { FileStatus::Done } else { FileStatus::Failed };
            self.active = self.active.saturating_sub(1);
            self.after_receive(now);
        }
    }

    /// The text arrived (or was refused): it is no longer waited for.
    pub fn take_text(&mut self) -> Result<(), Reject> {
        if self.mode != Mode::Receive {
            return Err(Reject::WrongMode);
        }
        if self.approval != Approval::Allowed {
            return Err(Reject::NotApproved);
        }
        if !self.text_wanted {
            return Err(Reject::Conflict);
        }
        Ok(())
    }

    pub fn finish_text(&mut self, now: Instant) {
        self.text_wanted = false;
        self.after_receive(now);
    }

    fn after_receive(&mut self, now: Instant) {
        let busy = self.incoming.iter().any(|f| matches!(f.status, FileStatus::Waiting | FileStatus::Receiving)) || self.text_wanted;
        if !busy {
            self.deadline = self.deadline.min(now + LINGER_RECEIVE);
        }
    }

    /// Everything announced has been dealt with.
    pub fn batch_finished(&self) -> bool {
        !self.incoming.is_empty() && !self.text_wanted && self.incoming.iter().all(|f| matches!(f.status, FileStatus::Done | FileStatus::Failed))
    }

    // ---- the phone downloads ----------------------------------------------------------------------

    /// Start serving file `i` of a send session (approved only).
    pub fn start_download(&mut self, i: usize) -> Result<SendFile, Reject> {
        let Mode::Send(files) = &self.mode else { return Err(Reject::WrongMode) };
        if self.approval != Approval::Allowed {
            return Err(Reject::NotApproved);
        }
        let f = files.get(i).cloned().ok_or(Reject::NotFound)?;
        self.active += 1;
        self.deadline = self.hard_deadline;
        Ok(f)
    }

    pub fn finish_download(&mut self, ok: bool, now: Instant) {
        self.active = self.active.saturating_sub(1);
        if ok && self.active == 0 {
            self.deadline = self.deadline.min(now + LINGER_SEND);
        }
    }

    /// The state the phone's page polls (no names: it knows what it announced; the PC's file
    /// names are in the page already).
    pub fn state_json(&self) -> String {
        let state = match (self.ended, self.approval) {
            (Some(_), Approval::Denied) => "denied",
            (Some(_), _) => "ended",
            (None, Approval::None) => "idle",
            (None, Approval::Pending) => "pending",
            (None, Approval::Allowed) => "allowed",
            (None, Approval::Denied) => "denied",
        };
        let files: Vec<&str> = self
            .incoming
            .iter()
            .map(|f| match f.status {
                FileStatus::Waiting => "wait",
                FileStatus::Receiving => "recv",
                FileStatus::Done => "done",
                FileStatus::Failed => "fail",
            })
            .collect();
        serde_json::json!({ "state": state, "files": files, "text": self.text_wanted }).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::from([a, b, c, d])
    }
    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }
    fn recv(now: Instant) -> Session {
        Session::new("TOKENtokenTOKENtoken12".into(), Mode::Receive, now)
    }
    fn files(n: usize) -> Vec<(String, u64)> {
        (0..n).map(|i| (format!("f{i}.jpg"), 1000)).collect()
    }
    const PHONE: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 23));

    // ---- token -----------------------------------------------------------------------------------

    #[test]
    fn tokens_are_22_url_safe_characters_and_never_repeat() {
        let a = new_token().unwrap();
        let b = new_token().unwrap();
        assert_eq!(a.len(), 22);
        assert_ne!(a, b);
        for t in [&a, &b] {
            assert!(t.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'), "{t}");
        }
        // 128 bits: 64 draws never collide and spread over the whole alphabet.
        let set: std::collections::HashSet<String> = (0..64).map(|_| new_token().unwrap()).collect();
        assert_eq!(set.len(), 64);
    }

    #[test]
    fn base64url_matches_known_vectors() {
        assert_eq!(encode_token(&[0; 16]), "AAAAAAAAAAAAAAAAAAAAAA");
        assert_eq!(encode_token(&[0xff; 16]), "_____________________w");
        let mut b = [0u8; 16];
        for (i, x) in b.iter_mut().enumerate() {
            *x = i as u8;
        }
        // 00 01 02 03 ... 0f  (RFC 4648 base64 of the bytes, '+' -> '-', '/' -> '_', no padding)
        assert_eq!(encode_token(&b), "AAECAwQFBgcICQoLDA0ODw");
    }

    #[test]
    fn token_check_is_exact_and_length_safe() {
        assert!(token_matches("abcDEF123456abcDEF1234", "abcDEF123456abcDEF1234"));
        for bad in ["", "abcDEF123456abcDEF123", "abcDEF123456abcDEF12345", "abcDEF123456abcDEF1235", "ABCDEF123456abcDEF1234", " abcDEF123456abcDEF1234"] {
            assert!(!token_matches("abcDEF123456abcDEF1234", bad), "{bad:?}");
        }
        assert!(!token_matches("", "x"));
        assert!(token_matches("", ""), "only reachable with an empty token, which new_token never makes");
    }

    // ---- admit: token, expiry, one device -----------------------------------------------------------

    #[test]
    fn a_wrong_token_is_refused_and_locks_nothing() {
        let t0 = Instant::now();
        let mut s = recv(t0);
        assert_eq!(s.admit("nope", PHONE, t0), Err(Reject::BadToken));
        assert_eq!(s.admit("", PHONE, t0), Err(Reject::BadToken));
        assert_eq!(s.device(), None, "a wrong token must not take the lock");
        // ... so the right phone still gets in afterwards.
        assert_eq!(s.admit("TOKENtokenTOKENtoken12", PHONE, t0), Ok(()));
        assert_eq!(s.device(), Some(PHONE));
    }

    #[test]
    fn the_first_ip_with_the_token_owns_the_session() {
        let t0 = Instant::now();
        let mut s = recv(t0);
        let other = ip(192, 168, 1, 99);
        assert_eq!(s.admit(s.token().to_string().as_str(), PHONE, t0), Ok(()));
        assert_eq!(s.admit(&s.token().to_string(), other, at(t0, 1)), Err(Reject::OtherDevice));
        // The owner keeps working, and the intruder stays out (even with a wrong token it is BadToken first).
        assert_eq!(s.admit(&s.token().to_string(), PHONE, at(t0, 2)), Ok(()));
        assert_eq!(s.admit(&s.token().to_string(), other, at(t0, 3)), Err(Reject::OtherDevice));
        assert_eq!(s.admit("x", other, at(t0, 3)), Err(Reject::BadToken));
        assert_eq!(s.device(), Some(PHONE));
    }

    #[test]
    fn the_session_expires_after_ten_minutes() {
        let t0 = Instant::now();
        let mut s = recv(t0);
        let tok = s.token().to_string();
        assert_eq!(s.admit(&tok, PHONE, at(t0, 599)), Ok(()));
        assert!(!s.is_expired(at(t0, 599)));
        assert_eq!(s.remaining(at(t0, 540)), Duration::from_secs(60));
        // The deadline itself is already expired.
        assert!(s.is_expired(at(t0, 600)));
        assert_eq!(s.admit(&tok, PHONE, at(t0, 600)), Err(Reject::Expired));
        assert_eq!(s.remaining(at(t0, 700)), Duration::ZERO);
        assert_eq!(s.tick(at(t0, 600)), Some(EndReason::Expired));
        // An expired session does not hand the lock to a latecomer either.
        let mut s2 = recv(t0);
        let tok2 = s2.token().to_string();
        assert_eq!(s2.admit(&tok2, ip(10, 0, 0, 5), at(t0, 601)), Err(Reject::Expired));
        assert_eq!(s2.device(), None);
    }

    #[test]
    fn a_running_transfer_keeps_the_session_open_past_the_deadline() {
        let t0 = Instant::now();
        let mut s = recv(t0);
        let tok = s.token().to_string();
        s.admit(&tok, PHONE, t0).unwrap();
        s.offer(PHONE, &files(1), false, t0).unwrap();
        s.approve();
        s.start_upload(0).unwrap();
        assert!(!s.is_expired(at(t0, 5000)), "a transfer in progress is never cut off by the clock");
        assert_eq!(s.tick(at(t0, 5000)), None);
        s.finish_upload(0, true, at(t0, 5000));
        // Finished long after the deadline: now it is over (the linger never extends).
        assert!(s.is_expired(at(t0, 5000)));
        assert_eq!(s.tick(at(t0, 5000)), Some(EndReason::Expired));
    }

    #[test]
    fn a_completed_transfer_shortens_the_session() {
        let t0 = Instant::now();
        let mut s = recv(t0);
        let tok = s.token().to_string();
        s.admit(&tok, PHONE, t0).unwrap();
        s.offer(PHONE, &files(2), false, at(t0, 10)).unwrap();
        s.approve();
        s.start_upload(0).unwrap();
        s.finish_upload(0, true, at(t0, 20));
        assert_eq!(s.remaining(at(t0, 20)), TTL - Duration::from_secs(20), "one file is still to come: no shortening yet");
        s.start_upload(1).unwrap();
        s.finish_upload(1, true, at(t0, 30));
        assert!(s.batch_finished());
        assert_eq!(s.remaining(at(t0, 30)), LINGER_RECEIVE);
        assert!(s.is_expired(at(t0, 90)));
        // A new batch inside the linger reopens the window (up to the hard limit).
        let mut s = recv(t0);
        let tok = s.token().to_string();
        s.admit(&tok, PHONE, t0).unwrap();
        s.offer(PHONE, &files(1), false, at(t0, 10)).unwrap();
        s.approve();
        s.start_upload(0).unwrap();
        s.finish_upload(0, true, at(t0, 12));
        s.offer(PHONE, &files(1), false, at(t0, 40)).unwrap();
        assert_eq!(s.remaining(at(t0, 40)), TTL - Duration::from_secs(40));
        // Send mode: 20 s after the download.
        let f = SendFile { path: "C:\\x\\a.pdf".into(), name: "a.pdf".into(), size: 5 };
        let mut s = Session::new("T".into(), Mode::Send(vec![f]), t0);
        s.request_download(PHONE, t0).unwrap();
        s.approve();
        s.start_download(0).unwrap();
        s.finish_download(true, at(t0, 100));
        assert_eq!(s.remaining(at(t0, 100)), LINGER_SEND);
    }

    // ---- approval ---------------------------------------------------------------------------------

    #[test]
    fn nothing_moves_before_the_user_says_yes() {
        let t0 = Instant::now();
        let mut s = recv(t0);
        let sum = s.offer(PHONE, &files(2), false, t0).unwrap();
        assert!(sum.needs_approval);
        assert_eq!(sum.files.len(), 2);
        assert_eq!(s.approval(), Approval::Pending);
        assert_eq!(s.start_upload(0), Err(Reject::NotApproved));
        assert_eq!(s.take_text(), Err(Reject::NotApproved));
        s.approve();
        assert_eq!(s.approval(), Approval::Allowed);
        assert!(s.start_upload(0).is_ok());
    }

    #[test]
    fn deny_ends_the_session_and_the_phone_is_told() {
        let t0 = Instant::now();
        let mut s = recv(t0);
        s.offer(PHONE, &files(1), false, t0).unwrap();
        s.deny();
        assert_eq!(s.ended(), Some(EndReason::Denied));
        assert!(s.is_expired(t0));
        assert_eq!(s.start_upload(0), Err(Reject::NotApproved));
        assert!(s.state_json().contains("\"denied\""));
        // Deny / approve with nothing pending do nothing.
        let mut s = recv(t0);
        s.deny();
        s.approve();
        assert_eq!((s.ended(), s.approval()), (None, Approval::None));
    }

    #[test]
    fn an_unanswered_card_times_out_after_90_seconds() {
        let t0 = Instant::now();
        let mut s = recv(t0);
        s.offer(PHONE, &files(1), false, at(t0, 5)).unwrap();
        assert_eq!(s.tick(at(t0, 94)), None);
        assert_eq!(s.tick(at(t0, 95)), Some(EndReason::ApprovalTimeout));
        assert_eq!(s.approval(), Approval::Denied);
        assert_eq!(s.start_upload(0), Err(Reject::NotApproved));
    }

    #[test]
    fn a_second_batch_from_the_approved_phone_is_not_asked_again() {
        let t0 = Instant::now();
        let mut s = recv(t0);
        assert!(s.offer(PHONE, &files(1), false, t0).unwrap().needs_approval);
        s.approve();
        s.start_upload(0).unwrap();
        s.finish_upload(0, true, at(t0, 1));
        let again = s.offer(PHONE, &files(3), true, at(t0, 2)).unwrap();
        assert!(!again.needs_approval, "the first transfer was approved: the device is trusted for this session");
        assert_eq!(s.incoming().len(), 3);
    }

    // ---- offers -----------------------------------------------------------------------------------

    #[test]
    fn offers_are_validated_and_names_are_sanitised() {
        let t0 = Instant::now();
        let mut s = recv(t0);
        assert_eq!(s.offer(PHONE, &[], false, t0), Err(Reject::BadOffer("nothing offered")));
        assert!(s.offer(PHONE, &files(MAX_FILES + 1), false, t0).is_err());
        assert!(s.offer(PHONE, &[("big.bin".into(), MAX_FILE + 1)], false, t0).is_err());
        assert!(s.offer(PHONE, &[("a".into(), MAX_FILE), ("b".into(), MAX_FILE), ("c".into(), 1)], false, t0).is_err(), "more than the batch cap");
        assert_eq!(s.approval(), Approval::None, "a refused offer never opens an approval card");
        let sum = s.offer(PHONE, &[("../../Windows/evil.dll".into(), 10), ("CON".into(), 1), ("ok.txt".into(), 2)], true, t0).unwrap();
        let names: Vec<&str> = sum.files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["evil.dll", "_CON", "ok.txt"]);
        assert!(sum.text);
        assert_eq!(s.incoming()[0].name, "evil.dll");
        // A text-only offer is fine.
        let mut s = recv(t0);
        assert!(s.offer(PHONE, &[], true, t0).is_ok());
        // The largest allowed single file is accepted.
        let mut s = recv(t0);
        assert!(s.offer(PHONE, &[("x.iso".into(), MAX_FILE)], false, t0).is_ok());
    }

    #[test]
    fn a_new_offer_waits_for_the_running_batch() {
        let t0 = Instant::now();
        let mut s = recv(t0);
        s.offer(PHONE, &files(2), false, t0).unwrap();
        s.approve();
        assert_eq!(s.offer(PHONE, &files(1), false, t0), Err(Reject::Conflict));
        s.start_upload(0).unwrap();
        s.finish_upload(0, true, t0);
        s.start_upload(1).unwrap();
        s.finish_upload(1, false, t0);
        assert!(s.batch_finished());
        assert!(s.offer(PHONE, &files(1), false, t0).is_ok());
    }

    #[test]
    fn uploads_follow_the_approved_offer_only() {
        let t0 = Instant::now();
        let mut s = recv(t0);
        assert_eq!(s.start_upload(0), Err(Reject::NotApproved), "no offer, no approval");
        s.offer(PHONE, &files(2), false, t0).unwrap();
        s.approve();
        assert_eq!(s.start_upload(2), Err(Reject::NotFound), "an index that was never announced");
        assert_eq!(s.start_upload(0).unwrap().name, "f0.jpg");
        assert_eq!(s.start_upload(0), Err(Reject::Conflict), "no second copy while it is being received");
        s.finish_upload(0, false, t0);
        assert!(s.start_upload(0).is_ok(), "a failed file can be tried again");
        s.finish_upload(0, true, t0);
        assert_eq!(s.start_upload(0), Err(Reject::Conflict), "no overwriting a finished one");
        assert_eq!(s.state_json(), r#"{"files":["done","wait"],"state":"allowed","text":false}"#);
    }

    #[test]
    fn text_needs_an_announcement_and_approval() {
        let t0 = Instant::now();
        let mut s = recv(t0);
        assert_eq!(s.take_text(), Err(Reject::NotApproved));
        s.offer(PHONE, &[], true, t0).unwrap();
        s.approve();
        assert!(s.take_text().is_ok());
        s.finish_text(t0);
        assert_eq!(s.take_text(), Err(Reject::Conflict), "one text per announcement");
        // No announcement, no text.
        let mut s = recv(t0);
        s.offer(PHONE, &files(1), false, t0).unwrap();
        s.approve();
        assert_eq!(s.take_text(), Err(Reject::Conflict));
    }

    #[test]
    fn modes_do_not_mix() {
        let t0 = Instant::now();
        let f = SendFile { path: "C:\\x\\a.pdf".into(), name: "a.pdf".into(), size: 5 };
        let mut send = Session::new("T".into(), Mode::Send(vec![f]), t0);
        assert_eq!(send.offer(PHONE, &files(1), false, t0), Err(Reject::WrongMode));
        assert_eq!(send.start_upload(0), Err(Reject::WrongMode));
        assert_eq!(send.take_text(), Err(Reject::WrongMode));
        assert_eq!(send.start_download(0), Err(Reject::NotApproved));
        let sum = send.request_download(PHONE, t0).unwrap();
        assert_eq!((sum.files, sum.needs_approval), (vec![("a.pdf".to_string(), 5)], true));
        send.approve();
        assert_eq!(send.start_download(0).unwrap().name, "a.pdf");
        assert_eq!(send.start_download(1), Err(Reject::NotFound));
        let mut r = recv(t0);
        assert_eq!(r.request_download(PHONE, t0), Err(Reject::WrongMode));
        assert_eq!(r.start_download(0), Err(Reject::WrongMode));
    }

    #[test]
    fn stop_ends_it_once() {
        let t0 = Instant::now();
        let mut s = recv(t0);
        s.stop();
        assert_eq!(s.ended(), Some(EndReason::Stopped));
        let tok = s.token().to_string();
        assert_eq!(s.admit(&tok, PHONE, t0), Err(Reject::Expired));
        s.stop();
        assert_eq!(s.ended(), Some(EndReason::Stopped));
        assert_eq!(s.tick(t0), None, "the end is reported by stop, not again by tick");
    }
}
