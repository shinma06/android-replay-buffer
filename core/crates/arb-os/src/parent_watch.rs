//! Parent-bound process lifecycle: exit when the parent closes our stdin pipe.
//!
//! A parent (Android Studio's ARB plugin) launches `arbd` / `arb-tray` with `--exit-on-stdin-eof`
//! and keeps the write end of the child's stdin pipe open for as long as it lives. The OS kernel
//! closes that write end when the parent exits for *any* reason — normal exit, crash, `kill -9`,
//! or an explicit close — and the child's blocking `read` then returns end-of-file. This is the
//! same on Linux, macOS and Windows (a broken anonymous pipe reads as EOF), needs no PID polling,
//! and therefore has no PID-reuse race.
//!
//! The watcher only *observes*; callers decide how to shut down (for `arbd` the same graceful
//! path as `system.shutdown`, for `arb-tray` the UI's quit path).

use std::io::{self, ErrorKind, Read};
use std::thread::{self, JoinHandle};

/// Why a parent watch ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParentGone {
    /// The read returned end-of-file: every writer (the parent) closed the pipe.
    Eof,
    /// The read failed with a non-retryable error (treated as the parent being gone).
    ReadError(ErrorKind),
}

/// Spawns a named thread that blocks reading `reader` and calls `on_parent_gone` exactly once,
/// when the read returns end-of-file or a non-retryable error.
///
/// Bytes the parent writes are read and discarded, so a parent may also use the pipe as a
/// keep-alive channel. `Interrupted` reads are retried.
///
/// # Errors
///
/// Returns the OS error if the watcher thread cannot be spawned.
pub fn spawn_parent_watch<R, F>(
    thread_name: &str,
    mut reader: R,
    on_parent_gone: F,
) -> io::Result<JoinHandle<ParentGone>>
where
    R: Read + Send + 'static,
    F: FnOnce(ParentGone) + Send + 'static,
{
    thread::Builder::new()
        .name(thread_name.to_owned())
        .spawn(move || {
            let gone = wait_for_parent_gone(&mut reader);
            on_parent_gone(gone);
            gone
        })
}

/// Spawns [`spawn_parent_watch`] on this process's stdin.
///
/// # Errors
///
/// Returns the OS error if the watcher thread cannot be spawned.
pub fn spawn_stdin_parent_watch<F>(
    thread_name: &str,
    on_parent_gone: F,
) -> io::Result<JoinHandle<ParentGone>>
where
    F: FnOnce(ParentGone) + Send + 'static,
{
    spawn_parent_watch(thread_name, io::stdin(), on_parent_gone)
}

/// Blocks until `reader` reports end-of-file or a non-retryable error.
fn wait_for_parent_gone<R: Read>(reader: &mut R) -> ParentGone {
    let mut buf = [0_u8; 256];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return ParentGone::Eof,
            Ok(_) => {}
            Err(err) if err.kind() == ErrorKind::Interrupted => {}
            Err(err) => return ParentGone::ReadError(err.kind()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};
    use std::sync::mpsc;
    use std::time::Duration;

    use super::{ParentGone, spawn_parent_watch};

    #[test]
    fn real_pipe_stays_quiet_while_writer_lives_and_fires_on_writer_drop() {
        let (reader, mut writer) = io::pipe().expect("anonymous pipe");
        let (tx, rx) = mpsc::channel();
        let watch = spawn_parent_watch("arb-test-parent-watch", reader, move |gone| {
            let _ = tx.send(gone);
        })
        .expect("spawn watcher");

        writer.write_all(b"keep-alive\n").expect("write keep-alive");
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "data from a live parent must not end the watch"
        );

        drop(writer);
        let gone = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("dropping the only writer must end the watch within 2 s");
        assert_eq!(gone, ParentGone::Eof, "writer drop must read as EOF");
        assert_eq!(
            watch.join().expect("watcher thread must not panic"),
            ParentGone::Eof,
            "the thread result must match the callback"
        );
    }

    struct FailingReader;

    impl io::Read for FailingReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    #[test]
    fn read_error_is_treated_as_parent_gone() {
        let (tx, rx) = mpsc::channel();
        spawn_parent_watch("arb-test-parent-watch-err", FailingReader, move |gone| {
            let _ = tx.send(gone);
        })
        .expect("spawn watcher");
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2))
                .expect("a read error must end the watch"),
            ParentGone::ReadError(io::ErrorKind::BrokenPipe),
            "the error kind must be reported"
        );
    }
}
