//! Playing the adzan: `Windows.Media.Playback.MediaPlayer` (wav, mp3, m4a, aac, wma, flac).
//!
//! Loaded only when a sound is played (nothing of Media Foundation is in the island otherwise),
//! on the adzan thread, an MTA thread. The system media transport (the volume flyout, the media
//! keys, the island's own media pill) is switched off for this player, so the adzan never shows
//! up as a "song". It ends when the file ends, when the user stops it, when it cannot be played
//! (the caller then falls back to the chime) or when `max` has passed.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use windows::Foundation::{IClosable, TypedEventHandler};
use windows::Media::Core::MediaSource;
use windows::Media::Playback::{IMediaPlaybackSource, MediaPlayer};
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};
use windows::Storage::StorageFile;
use windows::core::{HSTRING, IInspectable, Interface};

use super::media::wait_op;

/// What to play.
pub enum Playable {
    /// Audio held in memory (the generated chime), with its MIME type.
    Bytes(Vec<u8>, &'static str),
    File(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// It played to its end.
    Finished,
    /// Stopped by the user, or cut at `max`.
    Stopped,
    /// The player could not open or play it.
    Failed,
}

enum Msg {
    Ended,
    Failed,
}

/// How often the stop flag is looked at.
const POLL: Duration = Duration::from_millis(100);
/// Longest wait for one WinRT call (`wait_op!` reads this name).
const OP_TIMEOUT: Duration = Duration::from_secs(5);

/// Play `what` at `volume` (0..1) until it ends, `stop` is set or `max` has passed. Blocks; call
/// it from the adzan thread only.
pub fn play(what: &Playable, volume: f32, stop: &AtomicBool, max: Duration) -> Outcome {
    match play_inner(what, volume, stop, max) {
        Ok(o) => o,
        Err(e) => {
            log_warn!("adzan: playback failed: {e}");
            Outcome::Failed
        }
    }
}

fn source_of(what: &Playable) -> windows::core::Result<MediaSource> {
    match what {
        Playable::Bytes(bytes, mime) => {
            let stream = InMemoryRandomAccessStream::new()?;
            let writer = DataWriter::CreateDataWriter(&stream)?;
            writer.WriteBytes(bytes)?;
            (|| wait_op!(writer.StoreAsync()?))()?;
            let _ = writer.DetachStream()?;
            stream.Seek(0)?;
            MediaSource::CreateFromStream(&stream, &HSTRING::from(*mime))
        }
        Playable::File(path) => {
            let file = (|| wait_op!(StorageFile::GetFileFromPathAsync(&HSTRING::from(path.as_os_str()))?))()?;
            MediaSource::CreateFromStorageFile(&file)
        }
    }
}

fn play_inner(what: &Playable, volume: f32, stop: &AtomicBool, max: Duration) -> windows::core::Result<Outcome> {
    let source = source_of(what)?;
    let player = MediaPlayer::new()?;
    // No system media session for this player.
    if let Ok(cm) = player.CommandManager() {
        let _ = cm.SetIsEnabled(false);
    }
    player.SetAutoPlay(false)?;
    player.SetVolume(volume.clamp(0.0, 1.0) as f64)?;
    let (tx, rx) = mpsc::channel::<Msg>();
    let tx_end = tx.clone();
    let end_token = player.MediaEnded(&TypedEventHandler::<MediaPlayer, IInspectable>::new(move |_, _| {
        let _ = tx_end.send(Msg::Ended);
        Ok(())
    }))?;
    let fail_token = player.MediaFailed(&TypedEventHandler::<MediaPlayer, windows::Media::Playback::MediaPlayerFailedEventArgs>::new(move |_, _| {
        let _ = tx.send(Msg::Failed);
        Ok(())
    }))?;
    player.SetSource(&source.cast::<IMediaPlaybackSource>()?)?;
    player.Play()?;
    let started = Instant::now();
    let outcome = loop {
        match rx.recv_timeout(POLL) {
            Ok(Msg::Ended) => break Outcome::Finished,
            Ok(Msg::Failed) => break Outcome::Failed,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if stop.load(Ordering::Relaxed) || started.elapsed() >= max {
                    break Outcome::Stopped;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break Outcome::Failed,
        }
    };
    let _ = player.Pause();
    let _ = player.RemoveMediaEnded(end_token);
    let _ = player.RemoveMediaFailed(fail_token);
    let _ = player.cast::<IClosable>().and_then(|c| c.Close());
    let _ = source.cast::<IClosable>().and_then(|c| c.Close());
    Ok(outcome)
}
