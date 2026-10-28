//! Windows pseudo-console sessions. Termination goes through the process
//! handle this transport owns, and output keeps draining while the
//! pseudo-console closes, since closing it flushes synchronously.

use std::io::{Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use portable_pty::{CommandBuilder, MasterPty};
use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, WAIT_OBJECT_0};
use windows_sys::Win32::System::IO::CancelSynchronousIo;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, TerminateProcess, WaitForSingleObject,
};

use super::Command;
use super::transport::{
    CHUNK, Drain, INPUT, Notify, OUTPUT, Queue, STACK, Slot, check_command, pty_size,
};
use crate::memory::{Budget, Lease};

/// A program running on a pseudo-console, with its IO threads.
pub struct Pty {
    master: Option<Box<dyn MasterPty + Send>>,
    process: OwnedHandle,
    pid: u32,
    input: Arc<Queue>,
    output: Arc<Queue>,
    closing: Arc<AtomicBool>,
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

fn exited(process: &OwnedHandle) -> bool {
    // SAFETY: `process` is an owned, open process handle.
    unsafe { WaitForSingleObject(process.as_raw_handle(), 0) == WAIT_OBJECT_0 }
}

fn terminate(process: &OwnedHandle) -> Result<(), String> {
    if exited(process) {
        return Ok(());
    }
    // A nonzero BOOL is success; an exit racing the call also counts.
    // SAFETY: `process` is an owned, open process handle.
    if unsafe { TerminateProcess(process.as_raw_handle(), 1) } != 0 || exited(process) {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

impl Pty {
    /// Starts `command` on a new pseudo-console of `rows` x `cols`; `wake`
    /// runs on an IO thread when output, end of stream or exit arrives.
    pub fn spawn(
        command: &Command,
        rows: u16,
        cols: u16,
        budget: &Budget,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, String> {
        check_command(&command.program, &command.args)?;
        let slot = Slot::acquire()?;
        let notify = Arc::new(Notify::new(wake));
        let lease = budget.reserve(3 * INPUT + 2 * OUTPUT + 4 * STACK + 4 * CHUNK + 64 * 1024)?;
        let pair = portable_pty::native_pty_system()
            .openpty(pty_size(rows, cols)?)
            .map_err(|e| e.to_string())?;
        let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let mut writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        let mut builder = CommandBuilder::new(&command.program);
        builder.args(&command.args);
        builder.env("TERM", "xterm-256color");
        builder.env("COLORTERM", "truecolor");
        for (key, value) in &command.env {
            builder.env(key, value);
        }
        if !command.cwd.is_empty() {
            builder.cwd(&command.cwd);
        }
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
        let output = Arc::new(Queue::new(OUTPUT));
        let closing = Arc::new(AtomicBool::new(false));
        let mut pty = Self {
            master: Some(pair.master),
            process,
            pid,
            input: input.clone(),
            output: output.clone(),
            closing: closing.clone(),
            reader: None,
            writer: None,
            waiter: None,
            notify: notify.clone(),
            _slot: slot,
            _lease: lease,
        };
        let out = output.clone();
        let stop = closing.clone();
        let wake = notify.clone();
        pty.reader = Some(
            thread::Builder::new()
                .name("gui-conpty-read".into())
                .stack_size(STACK)
                .spawn(move || {
                    let mut buffer = [0; CHUNK];
                    loop {
                        match reader.read(&mut buffer) {
                            Ok(0) => break,
                            // Closing flushes output synchronously; it is read
                            // and discarded so the close can finish.
                            Ok(_) if stop.load(Ordering::Acquire) => (),
                            Ok(n) => match out.push(buffer[..n].to_vec(), true) {
                                Ok(true) => wake.fire(),
                                Ok(false) => (),
                                Err(_) if stop.load(Ordering::Acquire) => (),
                                Err(_) => break,
                            },
                            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => (),
                            Err(_) => break,
                        }
                    }
                    out.finish(true, None);
                    wake.fire();
                })
                .map_err(|e| e.to_string())?,
        );
        let stop = closing;
        pty.writer = Some(
            thread::Builder::new()
                .name("gui-conpty-write".into())
                .stack_size(STACK)
                .spawn(move || {
                    while let Some(bytes) = input.pop() {
                        if stop.load(Ordering::Acquire) || writer.write_all(&bytes).is_err() {
                            break;
                        }
                    }
                })
                .map_err(|e| e.to_string())?,
        );
        pty.waiter = Some(
            thread::Builder::new()
                .name("gui-conpty-exit".into())
                .stack_size(STACK)
                .spawn(move || {
                    let result = child
                        .wait()
                        .map(|status| status.exit_code())
                        .map_err(|e| e.to_string().chars().take(1024).collect());
                    output.finish(false, Some(result));
                    notify.fire();
                })
                .map_err(|e| e.to_string())?,
        );
        Ok(pty)
    }

    /// Queues input for the program without blocking.
    pub fn write(&self, bytes: &[u8]) -> Result<(), String> {
        if bytes.is_empty() {
            return Ok(());
        }
        if bytes.len() > INPUT {
            return Err("terminal input exceeds 64 KiB".into());
        }
        self.input.push(bytes.to_vec(), false).map(|_| ())
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), String> {
        self.master
            .as_ref()
            .ok_or("the terminal is closed")?
            .resize(pty_size(rows, cols)?)
            .map_err(|e| e.to_string())
    }

    pub fn drain(&self, limit: usize) -> Drain {
        super::transport::drain(&self.output, &self.notify, limit)
    }

    pub fn has_pending_output(&self) -> bool {
        self.notify.is_pending()
    }

    /// Bytes waiting in the input and output queues.
    pub fn queued_bytes(&self) -> (usize, usize) {
        (self.input.bytes(), self.output.bytes())
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Terminates the program, closes the pseudo-console and joins every IO
    /// thread.
    pub fn close(&mut self) -> Result<(), String> {
        if self.closing.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.input.close();
        self.output.close();
        let terminated = terminate(&self.process);
        if let Some(writer) = self.writer.take() {
            // SAFETY: the writer thread is alive until joined below; cancelling
            // its synchronous write only ends that write.
            unsafe { CancelSynchronousIo(writer.as_raw_handle()) };
            let _ = writer.join();
        }
        // Dropping the master closes the pseudo-console, which blocks until
        // its output is read; the reader is still running to read it.
        if let Some(master) = self.master.take() {
            let close = thread::Builder::new()
                .name("gui-conpty-close".into())
                .stack_size(STACK)
                .spawn(move || drop(master))
                .map_err(|e| e.to_string())?;
            let _ = close.join();
        }
        for thread in [&mut self.reader, &mut self.waiter] {
            if let Some(thread) = thread.take() {
                let _ = thread.join();
            }
        }
        terminated
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
        let mut pty = Pty::spawn(
            &command,
            24,
            80,
            &budget,
            Arc::new(move || {
                let _ = send.send(());
            }),
        )
        .unwrap();
        let mut output = Vec::new();
        loop {
            let got = pty.drain(OUTPUT);
            output.extend(got.bytes);
            if got.exit.is_some() && String::from_utf8_lossy(&output).contains("conpty-native") {
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
    fn closing_a_running_program_terminates_it_and_joins() {
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
            let mut pty = Pty::spawn(&command, 24, 80, &budget, Arc::new(|| {})).unwrap();
            pty.close().unwrap();
            assert!(exited(&pty.process));
            drop(pty);
            assert_eq!(budget.usage().0, 0);
        }
    }
}
