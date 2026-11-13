//! Windows pseudo-console sessions. Termination goes through the process
//! handle this transport owns, and output keeps draining while the
//! pseudo-console closes, since closing it flushes synchronously.

use std::io::{Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use portable_pty::{CommandBuilder, MasterPty, PtyPair};
use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, WAIT_OBJECT_0};
use windows_sys::Win32::System::IO::CancelSynchronousIo;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, INFINITE, TerminateProcess, WaitForSingleObject,
};

use super::Command;
use super::transport::{
    BATCH, CHUNK, Drain, Ending, Handoff, INPUT, Notify, Output, Queue, STACK, Sink, Slot,
    check_command, pty_size,
};
use crate::memory::{Budget, Lease};

/// A program running on a pseudo-console, with its IO threads.
pub struct Pty {
    master: Option<Box<dyn MasterPty + Send>>,
    process: OwnedHandle,
    pid: u32,
    input: Arc<Queue>,
    ending: Arc<Ending>,
    closing: Arc<AtomicBool>,
    handoff: Arc<Handoff>,
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
    waiter: Option<JoinHandle<()>>,
    notify: Arc<Notify>,
    _slot: Slot,
    _lease: Lease,
}

/// A second handle to the child process, so termination and the blocking
/// exit wait never depend on a numeric process id.
fn duplicate(raw: RawHandle) -> Result<OwnedHandle, String> {
    let mut handle = std::ptr::null_mut();
    // SAFETY: `raw` is the live process handle the child owns; the duplicate
    // is written to `handle` and owned below.
    let duplicated = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            raw,
            GetCurrentProcess(),
            &mut handle,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if duplicated == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    // SAFETY: DuplicateHandle succeeded, so `handle` is a new owned handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

// The exit code is set when termination begins, before the process object
// signals; the program has ended only once it signals.
fn wait_for_exit(process: &OwnedHandle) -> Result<u32, String> {
    // SAFETY: `process` is an owned, open process handle.
    if unsafe { WaitForSingleObject(process.as_raw_handle(), INFINITE) } != WAIT_OBJECT_0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let mut code = 0;
    // SAFETY: `process` is an owned, open process handle and `code` a live u32.
    if unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) } == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(code)
}

/// A pseudo-console opened for a program that has not started yet, so the
/// program can start at a size set after the opening.
pub struct Opened {
    pair: PtyPair,
    builder: CommandBuilder,
    slot: Slot,
    lease: Lease,
    wake: Arc<dyn Fn() + Send + Sync>,
    sink: Sink,
}

impl Opened {
    /// Starts the program on a screen of `rows` x `cols`. The size is set
    /// before the program exists, so it reads it at its start and is never
    /// told of a change.
    pub fn start(self, rows: u16, cols: u16, cell: (u16, u16)) -> Result<Pty, String> {
        let Self {
            pair,
            builder,
            slot,
            lease,
            wake,
            sink,
        } = self;
        pair.master
            .resize(pty_size(rows, cols, cell)?)
            .map_err(|e| e.to_string())?;
        let notify = Arc::new(Notify::new(wake));
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let mut writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        let mut child = pair
            .slave
            .spawn_command(builder)
            .map_err(|e| e.to_string())?;
        let pid = child.process_id().unwrap_or(0);
        let raw = child
            .as_raw_handle()
            .ok_or("the terminal program has no process handle")?;
        let process = match duplicate(raw) {
            Ok(process) => process,
            Err(error) => {
                // SAFETY: `raw` is the child's live process handle.
                unsafe { TerminateProcess(raw, 1) };
                let _ = child.wait();
                return Err(error);
            }
        };
        drop(pair.slave);
        let input = Arc::new(Queue::new(INPUT));
        let ending = Arc::new(Ending::default());
        let closing = Arc::new(AtomicBool::new(false));
        let handoff = Arc::new(Handoff::new());
        let mut pty = Pty {
            master: Some(pair.master),
            process,
            pid,
            input: input.clone(),
            ending: ending.clone(),
            closing: closing.clone(),
            handoff: handoff.clone(),
            reader: None,
            writer: None,
            waiter: None,
            notify: notify.clone(),
            _slot: slot,
            _lease: lease,
        };
        let end = ending.clone();
        let stop = closing.clone();
        let wake = notify.clone();
        let mut output = Output::new(sink, notify.clone(), handoff)?;
        pty.reader = Some(
            thread::Builder::new()
                .name("gui-conpty-read".into())
                .stack_size(STACK)
                .spawn(move || {
                    let mut buffer = vec![0; BATCH];
                    loop {
                        match reader.read(&mut buffer) {
                            Ok(0) => break,
                            // Closing flushes output synchronously; it is read
                            // and discarded so the close can finish.
                            Ok(_) if stop.load(Ordering::Acquire) => (),
                            // A full buffer means more output is waiting;
                            // otherwise the next read will block.
                            Ok(n) => {
                                output.push(&buffer[..n]);
                                if n == buffer.len() {
                                    output.wake();
                                } else {
                                    output.idle();
                                    // The next read waits for output.
                                    output.rested();
                                }
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => (),
                            Err(_) => break,
                        }
                    }
                    // The program's last output is parsed before its end is
                    // known; a closing transport drops what is left.
                    if stop.load(Ordering::Acquire) {
                        drop(output);
                    } else {
                        output.finish();
                    }
                    end.finish(true, None);
                    wake.fire();
                })
                .map_err(|e| e.to_string())?,
        );
        let stop = closing;
        let wake = notify.clone();
        pty.writer = Some(
            thread::Builder::new()
                .name("gui-conpty-write".into())
                .stack_size(STACK)
                .spawn(move || {
                    while let Some(bytes) = input.pop() {
                        if stop.load(Ordering::Acquire) || writer.write_all(&bytes).is_err() {
                            break;
                        }
                        input.finished();
                        // A paste waiting for room goes on from the owner.
                        if input.take_room_wanted() {
                            wake.fire();
                        }
                    }
                })
                .map_err(|e| e.to_string())?,
        );
        let exit = duplicate(pty.process.as_raw_handle())?;
        pty.waiter = Some(
            thread::Builder::new()
                .name("gui-conpty-exit".into())
                .stack_size(STACK)
                .spawn(move || {
                    let result = wait_for_exit(&exit).map_err(|e| e.chars().take(1024).collect());
                    ending.finish(false, Some(result));
                    notify.fire();
                })
                .map_err(|e| e.to_string())?,
        );
        Ok(pty)
    }
}

impl Pty {
    /// Opens a pseudo-console of `rows` x `cols` for `command`; `wake` runs
    /// on an IO thread when output, end of stream or exit arrives once it
    /// starts.
    pub fn open(
        command: &Command,
        rows: u16,
        cols: u16,
        cell: (u16, u16),
        budget: &Budget,
        wake: Arc<dyn Fn() + Send + Sync>,
        sink: Sink,
    ) -> Result<Opened, String> {
        check_command(&command.program, &command.args)?;
        let slot = Slot::acquire()?;
        let lease = budget.reserve(3 * INPUT + 5 * STACK + 4 * CHUNK + 3 * BATCH + 64 * 1024)?;
        let pair = portable_pty::native_pty_system()
            .openpty(pty_size(rows, cols, cell)?)
            .map_err(|e| e.to_string())?;
        let mut builder = CommandBuilder::new(&command.program);
        builder.args(&command.args);
        super::transport::environment(&mut builder, command);
        if !command.cwd.is_empty() {
            builder.cwd(&command.cwd);
        }
        Ok(Opened {
            pair,
            builder,
            slot,
            lease,
            wake,
            sink,
        })
    }

    /// Starts `command` on a new pseudo-console of `rows` x `cols`, as `open`
    /// and `Opened::start` do together.
    #[cfg(test)]
    pub fn spawn(
        command: &Command,
        rows: u16,
        cols: u16,
        cell: (u16, u16),
        budget: &Budget,
        wake: Arc<dyn Fn() + Send + Sync>,
        sink: Sink,
    ) -> Result<Self, String> {
        Self::open(command, rows, cols, cell, budget, wake, sink)?.start(rows, cols, cell)
    }

    /// Queues input for the program without blocking.
    pub fn write(&self, bytes: &[u8]) -> Result<(), String> {
        if bytes.is_empty() {
            return Ok(());
        }
        if bytes.len() > INPUT {
            return Err("terminal input exceeds 64 KiB".into());
        }
        // The pipe's writes block, so input goes through the writer thread.
        self.input.write_or_push(bytes, |_| 0).map(|_| ())
    }

    /// Tells the program its screen's size, in cells and in the pixels of a
    /// `cell`.
    pub fn resize(&self, rows: u16, cols: u16, cell: (u16, u16)) -> Result<(), String> {
        self.master
            .as_ref()
            .ok_or("the terminal is closed")?
            .resize(pty_size(rows, cols, cell)?)
            .map_err(|e| e.to_string())
    }

    pub fn drain(&self) -> Drain {
        super::transport::drain(&self.ending, &self.notify)
    }

    /// The program's state, like `drain`, but leaves the wake off: more
    /// output does not wake the owner until the next `drain`.
    pub fn peek(&self) -> Drain {
        super::transport::peek(&self.ending)
    }

    /// The input bytes a write could queue now.
    pub fn input_room(&self) -> usize {
        self.input.room()
    }

    /// Wakes the owner, as output does, once the program takes queued input.
    pub fn want_input_room(&self) {
        self.input.want_room();
    }

    /// Asks the parser thread to return memory that other threads freed for
    /// it, such as rows dropped when history is compacted. The parser
    /// allocated those rows, so it is the thread that must return them.
    pub fn release_memory(&self) {
        self.handoff.release();
    }

    pub fn has_pending_output(&self) -> bool {
        self.notify.is_pending()
    }

    /// Input bytes waiting for the program.
    pub fn queued_input(&self) -> usize {
        self.input.bytes()
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// A console names no directory for the program that holds it.
    pub fn cwd(&self) -> Option<String> {
        None
    }

    /// A console does not say which program holds it, so no job is reported.
    pub fn busy(&self) -> bool {
        false
    }

    /// A console does not say which program holds it.
    pub fn job(&self) -> Option<String> {
        None
    }

    /// Windows names no job to report.
    pub fn reported_job(&self) -> Option<String> {
        None
    }

    /// Windows names no foreground job, so it never moves.
    pub fn foreground_moved(&self) -> bool {
        false
    }

    /// Windows has no POSIX signals to send to a console program.
    pub fn signal(&self, signum: i32) -> Result<(), String> {
        Err(format!(
            "signal {signum} cannot be sent: Windows has no POSIX signals"
        ))
    }

    /// Terminates the program, closes the pseudo-console and joins every IO
    /// thread.
    pub fn close(&mut self) -> Result<(), String> {
        if self.closing.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.input.close();
        self.handoff.close();
        if let Some(writer) = self.writer.take() {
            // SAFETY: the writer thread is alive until joined below; cancelling
            // its synchronous write only ends that write.
            unsafe { CancelSynchronousIo(writer.as_raw_handle()) };
            let _ = writer.join();
        }
        // Dropping the master closes the pseudo-console, as closing a console
        // window does: its programs hear CTRL_CLOSE_EVENT, so a shell writes
        // its history before it ends, and the system ends any that stays. The
        // close blocks until its output is read; the reader is still running
        // to read it.
        if let Some(master) = self.master.take() {
            let close = thread::Builder::new()
                .name("gui-conpty-close".into())
                .stack_size(STACK)
                .spawn(move || drop(master))
                .map_err(|e| e.to_string())?;
            let _ = close.join();
        }
        if let Some(thread) = self.reader.take() {
            let _ = thread.join();
        }
        // The program ends in its own time; a thread of its own waits for
        // it, so closing never waits on it.
        let Some(waiter) = self.waiter.take() else {
            return Ok(());
        };
        thread::Builder::new()
            .name("gui-conpty-reap".into())
            .stack_size(STACK)
            .spawn(move || {
                let _ = waiter.join();
            })
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_conpty_runs_exits_and_releases_queues_and_threads() {
        let budget = Budget::new(8 * 1024 * 1024);
        let (send, receive) = std::sync::mpsc::channel();
        let command = Command {
            program: "cmd.exe".into(),
            args: vec![
                "/d".into(),
                "/c".into(),
                "echo conpty-native&exit /b 7".into(),
            ],
            ..Command::default()
        };
        let kept = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let into = kept.clone();
        let mut pty = Pty::spawn(
            &command,
            24,
            80,
            (0, 0),
            &budget,
            Arc::new(move || {
                let _ = send.send(());
            }),
            Box::new(move |bytes: &[u8]| into.lock().extend_from_slice(bytes)),
        )
        .unwrap();
        let mut answered = 0;
        loop {
            let got = pty.drain();
            let text = String::from_utf8_lossy(&kept.lock()).into_owned();
            // The pseudo-console inherits the cursor: it asks where the cursor
            // is and renders nothing until the terminal answers, as a
            // session's engine does.
            let asked = text.matches("\x1b[6n").count();
            for _ in answered..asked {
                pty.write(b"\x1b[1;1R").unwrap();
            }
            answered = asked;
            // Once the program has exited and the stream ended, nothing more
            // arrives.
            let shown = text.contains("conpty-native");
            if got.exit.is_some() && (shown || got.eof) {
                assert!(shown, "the program's output never arrived: {text:?}");
                assert_eq!(got.exit, Some(Ok(7)));
                break;
            }
            receive.recv().unwrap();
        }
        pty.close().unwrap();
        drop(pty);
        assert_eq!(budget.usage().0, 0);
    }

    #[test]
    fn closing_a_running_program_ends_it_and_joins() {
        let budget = Budget::new(8 * 1024 * 1024);
        let command = Command {
            program: "cmd.exe".into(),
            args: vec![
                "/d".into(),
                "/c".into(),
                "ping -n 30 127.0.0.1 > nul".into(),
            ],
            ..Command::default()
        };
        for _ in 0..5 {
            let mut pty = Pty::spawn(
                &command,
                24,
                80,
                (0, 0),
                &budget,
                Arc::new(|| {}),
                Box::new(|_: &[u8]| {}),
            )
            .unwrap();
            pty.close().unwrap();
            // Closing the pseudo-console ends the program, in the system's time.
            assert!(wait_for_exit(&pty.process).is_ok());
            drop(pty);
            assert_eq!(budget.usage().0, 0);
        }
    }
}
