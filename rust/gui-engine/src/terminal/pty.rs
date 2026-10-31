//! Unix pseudo-terminal sessions. Blocking readiness waits are interrupted
//! through a socket pair, and fixed reservations cover the queues, IO buffers
//! and the three IO threads' stacks.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use portable_pty::{Child, CommandBuilder, MasterPty};

use super::Command;
use super::transport::{
    BATCH, CHUNK, Drain, INPUT, Notify, OUTPUT, Queue, STACK, Slot, check_command, pty_size,
};
use crate::memory::{Budget, Lease};

/// A program running on a pseudo-terminal, with its IO threads.
pub struct Pty {
    master: Option<Box<dyn MasterPty + Send>>,
    input: Arc<Queue>,
    output: Arc<Queue>,
    cancel: UnixStream,
    stopping: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
    waiter: Option<JoinHandle<()>>,
    pid: u32,
    child: Option<Box<dyn Child + Send + Sync>>,
    notify: Arc<Notify>,
    _slot: Slot,
    _lease: Lease,
}

/// The working directory of process `pid`.
#[cfg(target_os = "linux")]
fn process_cwd(pid: i32) -> Option<String> {
    std::fs::read_link(format!("/proc/{pid}/cwd"))
        .ok()?
        .into_os_string()
        .into_string()
        .ok()
}

/// The working directory of process `pid`.
#[cfg(target_os = "macos")]
fn process_cwd(pid: i32) -> Option<String> {
    // SAFETY: the record is plain old data, which all zeros initializes.
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    // SAFETY: `info` is a writable record of `size` bytes.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            (&raw mut info).cast(),
            size,
        )
    };
    if read != size {
        return None;
    }
    // SAFETY: the kernel writes a NUL-terminated path into the record.
    let path = unsafe { std::ffi::CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr().cast()) };
    path.to_str().ok().map(str::to_owned)
}

/// Other systems name no process's directory to an unprivileged reader.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_cwd(_pid: i32) -> Option<String> {
    None
}

/// Waits until `fd` is ready for `events` or `cancel` is readable; answers
/// false for cancellation. The thread hands back the memory it freed first.
fn readiness(fd: i32, cancel: i32, events: i16) -> Result<bool, String> {
    let mut fds = [
        libc::pollfd {
            fd,
            events,
            revents: 0,
        },
        libc::pollfd {
            fd: cancel,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        let bound = windowing::idle::thread_idle().map_or(-1, |due| {
            i32::try_from(due.as_micros().div_ceil(1000)).unwrap_or(i32::MAX)
        });
        // SAFETY: `fds` holds two initialized pollfd records; the PTY
        // descriptor stays open until both IO threads have joined and the
        // cancel descriptor is owned by the calling thread.
        let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, bound) };
        if result == 0 {
            continue;
        }
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.to_string());
        }
        if fds[1].revents != 0 {
            return Ok(false);
        }
        return Ok(fds[0].revents != 0);
    }
}

/// How long a reader lets a streaming program's output gather between
/// passes: short against a frame, long against the program's writes.
const STREAM_GATHER: std::time::Duration = std::time::Duration::from_micros(250);

fn would_block(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
    )
}

impl Pty {
    /// Starts `command` on a new pseudo-terminal of `rows` x `cols`; `wake`
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
        let dimensions = pty_size(rows, cols)?;
        let lease =
            budget.reserve(3 * INPUT + 2 * OUTPUT + 3 * STACK + 4 * CHUNK + BATCH + 64 * 1024)?;
        let pair = portable_pty::native_pty_system()
            .openpty(dimensions)
            .map_err(|e| e.to_string())?;
        let fd = pair
            .master
            .as_raw_fd()
            .ok_or("the terminal has no native descriptor")?;
        // The reader and writer share one open file description; nonblocking
        // IO under poll lets cancellation interrupt both a read and a partly
        // written input chunk.
        // SAFETY: `fd` is the open master descriptor `pair` owns.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        // SAFETY: as above; only the status flags change.
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
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
        let (cancel, signal) = UnixStream::pair().map_err(|e| e.to_string())?;
        let write_signal = signal.try_clone().map_err(|e| e.to_string())?;
        let child = pair
            .slave
            .spawn_command(builder)
            .map_err(|e| e.to_string())?;
        let pid = child
            .process_id()
            .ok_or("the terminal program has no process identity")?;
        drop(pair.slave);
        let input = Arc::new(Queue::new(INPUT));
        let output = Arc::new(Queue::new(OUTPUT));
        let stopping = Arc::new(AtomicBool::new(false));
        let mut pty = Self {
            master: Some(pair.master),
            input: input.clone(),
            output: output.clone(),
            cancel,
            stopping: stopping.clone(),
            reader: None,
            writer: None,
            waiter: None,
            pid,
            child: Some(child),
            notify: notify.clone(),
            _slot: slot,
            _lease: lease,
        };
        let out = output.clone();
        let stop = stopping.clone();
        let wake = notify.clone();
        pty.reader = Some(
            thread::Builder::new()
                .name("gui-pty-read".into())
                .stack_size(STACK)
                .spawn(move || {
                    let mut buffer = [0; CHUNK];
                    // Output is read once the owner has taken what was queued:
                    // meanwhile the kernel holds what the program writes, and
                    // one pass collects it whole rather than a write at a time.
                    'reading: while !stop.load(Ordering::Acquire) && out.wait_empty() {
                        if readiness(fd, signal.as_raw_fd(), libc::POLLIN) != Ok(true) {
                            break;
                        }
                        let mut batch = Vec::new();
                        let ended = loop {
                            match reader.read(&mut buffer) {
                                Ok(0) => break true,
                                Ok(n) => {
                                    batch.extend_from_slice(&buffer[..n]);
                                    if batch.len() >= BATCH {
                                        break false;
                                    }
                                }
                                Err(error) if would_block(&error) => break false,
                                Err(_) => break true,
                            }
                        };
                        let streaming = !batch.is_empty();
                        if streaming {
                            match out.push(batch, true) {
                                Ok(true) => wake.fire(),
                                Ok(false) => (),
                                Err(_) => break 'reading,
                            }
                        }
                        if ended {
                            break;
                        }
                        // The kernel hands a terminal's output over a write at
                        // a time, at a fixed cost a read. While a program
                        // streams, a moment's pause lets it gather, so a pass
                        // reads it in fewer, fuller reads.
                        if streaming {
                            thread::sleep(STREAM_GATHER);
                        }
                    }
                    out.finish(true, None);
                    wake.fire();
                })
                .map_err(|e| e.to_string())?,
        );
        let stop = stopping.clone();
        pty.writer = Some(
            thread::Builder::new()
                .name("gui-pty-write".into())
                .stack_size(STACK)
                .spawn(move || {
                    while let Some(bytes) = input.pop() {
                        let mut offset = 0;
                        while offset < bytes.len() && !stop.load(Ordering::Acquire) {
                            if readiness(fd, write_signal.as_raw_fd(), libc::POLLOUT) != Ok(true) {
                                return;
                            }
                            match writer.write(&bytes[offset..]) {
                                Ok(0) => return,
                                Ok(n) => offset += n,
                                Err(error) if would_block(&error) => (),
                                Err(_) => return,
                            }
                        }
                    }
                })
                .map_err(|e| e.to_string())?,
        );
        pty.waiter = Some(
            thread::Builder::new()
                .name("gui-pty-exit".into())
                .stack_size(STACK)
                .spawn(move || {
                    // Observes the exit without reaping: the owned child keeps
                    // the process identity until close, so cancellation cannot
                    // signal a reused process id.
                    let result = loop {
                        let mut status = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
                        // SAFETY: `status` is a zeroed siginfo_t the call fills.
                        let result = unsafe {
                            libc::waitid(
                                libc::P_PID,
                                pid,
                                status.as_mut_ptr(),
                                libc::WEXITED | libc::WNOWAIT,
                            )
                        };
                        if result == 0 {
                            // SAFETY: waitid succeeded and initialized `status`.
                            let status = unsafe { status.assume_init() };
                            // SAFETY: the record describes a child state change.
                            let code = unsafe { status.si_status() };
                            break Ok(if status.si_code == libc::CLD_EXITED {
                                code as u32
                            } else {
                                128 + code as u32
                            });
                        }
                        let error = std::io::Error::last_os_error();
                        if error.kind() != std::io::ErrorKind::Interrupted {
                            break Err(error.to_string());
                        }
                    };
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

    /// Whether a job other than the program runs in the terminal's
    /// foreground, as a shell's command does.
    pub fn busy(&self) -> bool {
        let Some(fd) = self.master.as_ref().and_then(|m| m.as_raw_fd()) else {
            return false;
        };
        // SAFETY: `fd` is the open master descriptor.
        let foreground = unsafe { libc::tcgetpgrp(fd) };
        foreground > 0 && foreground != self.pid as i32
    }

    /// The working directory of the job in the terminal's foreground, or of
    /// the program when none runs there or its directory cannot be read.
    pub fn cwd(&self) -> Option<String> {
        let foreground = self
            .master
            .as_ref()
            .and_then(|m| m.as_raw_fd())
            // SAFETY: `fd` is the open master descriptor.
            .map(|fd| unsafe { libc::tcgetpgrp(fd) })
            .filter(|pgid| *pgid > 0);
        foreground
            .and_then(process_cwd)
            .or_else(|| process_cwd(self.pid as i32))
    }

    /// Sends signal `signum` to the terminal's foreground process group,
    /// the job the user is running, or to the program itself when the
    /// terminal has no foreground group.
    pub fn signal(&self, signum: i32) -> Result<(), String> {
        if !(1..=64).contains(&signum) {
            return Err(format!("{signum} is not a signal number"));
        }
        let foreground = match self.master.as_ref().and_then(|m| m.as_raw_fd()) {
            // SAFETY: `fd` is the open master descriptor.
            Some(fd) => unsafe { libc::tcgetpgrp(fd) },
            None => -1,
        };
        let target = if foreground > 0 {
            -foreground
        } else {
            self.pid as i32
        };
        // SAFETY: kill(2) checks the target and signal and reports failure.
        if unsafe { libc::kill(target, signum) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error().to_string())
        }
    }

    /// Ends the program's process group and joins every IO thread.
    pub fn close(&mut self) -> Result<(), String> {
        if self.stopping.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.input.close();
        self.output.close();
        let _ = (&self.cancel).write_all(&[1]);
        if let Some(master) = &self.master
            && let Some(fd) = master.as_raw_fd()
        {
            // The foreground process group and the session leader end
            // whatever descriptors to the terminal they still hold.
            // SAFETY: `fd` is the open master descriptor.
            let foreground = unsafe { libc::tcgetpgrp(fd) };
            if foreground > 0 {
                // SAFETY: signals a process group of this terminal.
                unsafe { libc::kill(-foreground, libc::SIGKILL) };
            }
        }
        if let Some(child) = &mut self.child
            && child.try_wait().ok().flatten().is_none()
        {
            // SAFETY: the unreaped child still owns this process id.
            unsafe { libc::kill(self.pid as i32, libc::SIGKILL) };
        }
        for thread in [&mut self.reader, &mut self.writer] {
            if let Some(thread) = thread.take() {
                let _ = thread.join();
            }
        }
        // An exiting program's last close of the terminal waits for its unread
        // output to drain while the master is open (macOS), so the master is
        // released before waiting for the exit.
        self.master = None;
        if let Some(thread) = self.waiter.take() {
            let _ = thread.join();
        }
        match self.child.take() {
            Some(mut child) => child.wait().map(|_| ()).map_err(|e| e.to_string()),
            None => Ok(()),
        }
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
    use std::sync::mpsc;

    fn command(script: &str) -> Command {
        Command {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            ..Command::default()
        }
    }

    fn waker() -> (Arc<dyn Fn() + Send + Sync>, mpsc::Receiver<()>) {
        let (send, receive) = mpsc::channel();
        (
            Arc::new(move || {
                let _ = send.send(());
            }),
            receive,
        )
    }

    /// Drains until `done` holds for the collected output and drain state.
    fn collect(
        pty: &Pty,
        wakes: &mpsc::Receiver<()>,
        mut done: impl FnMut(&str, &Drain) -> bool,
    ) -> String {
        let mut output = Vec::new();
        loop {
            let got = pty.drain(OUTPUT);
            output.extend_from_slice(&got.bytes);
            if done(&String::from_utf8_lossy(&output), &got) {
                return String::from_utf8_lossy(&output).into_owned();
            }
            wakes.recv().unwrap();
        }
    }

    #[test]
    fn real_pty_has_tty_unicode_input_resize_exit_and_all_threads_join() {
        let budget = Budget::new(8 * 1024 * 1024);
        let (wake, wakes) = waker();
        let script = "test -t 0 || exit 8; printf 'ready\\r\\n'; read word; \
                      printf 'got:%s\\r\\n' \"$word\"; stty size; exit 7";
        let mut pty = Pty::spawn(&command(script), 12, 44, &budget, wake).unwrap();
        pty.resize(18, 66).unwrap();
        let mut sent = false;
        let text = collect(&pty, &wakes, |text, got| {
            if !sent && text.contains("ready") {
                pty.write("héllo 界\n".as_bytes()).unwrap();
                sent = true;
            }
            got.eof && got.exit.is_some()
        });
        assert!(text.contains("got:héllo 界"), "{text}");
        assert!(text.contains("18 66"), "{text}");
        assert_eq!(pty.drain(0).exit, Some(Ok(7)));
        pty.close().unwrap();
        drop(pty);
        assert_eq!(budget.usage().0, 0);
    }

    #[test]
    fn control_c_reaches_the_real_foreground_process_group() {
        let budget = Budget::new(8 * 1024 * 1024);
        let (wake, wakes) = waker();
        let mut pty = Pty::spawn(
            &command("printf ready; exec sleep 20"),
            24,
            80,
            &budget,
            wake,
        )
        .unwrap();
        let mut sent = false;
        collect(&pty, &wakes, |text, got| {
            if !sent && text.contains("ready") {
                pty.write(b"\x03").unwrap();
                sent = true;
            }
            got.exit.is_some()
        });
        assert_eq!(pty.drain(0).exit, Some(Ok(130)));
        pty.close().unwrap();
        drop(pty);
        assert_eq!(budget.usage().0, 0);
    }

    #[test]
    fn a_sent_signal_reaches_the_foreground_job_and_not_the_shell() {
        let budget = Budget::new(8 * 1024 * 1024);
        let (wake, wakes) = waker();
        // An interactive shell runs `sleep` as its foreground job; the shell
        // itself survives the job's SIGTERM and reports its status.
        // The job is in the foreground before it prints.
        let script = "set -m; sh -c 'printf ready; exec sleep 20'; echo \"job:$?\"; exit 4";
        let mut pty = Pty::spawn(&command(script), 24, 80, &budget, wake).unwrap();
        let mut sent = false;
        let text = collect(&pty, &wakes, |text, got| {
            if !sent && text.contains("ready") {
                let fd = pty.master.as_ref().and_then(|m| m.as_raw_fd()).unwrap();
                // SAFETY: `fd` is the open master descriptor.
                assert_ne!(unsafe { libc::tcgetpgrp(fd) }, pty.pid() as i32);
                assert!(pty.busy());
                pty.signal(libc::SIGTERM).unwrap();
                sent = true;
            }
            got.eof && got.exit.is_some()
        });
        assert!(text.contains("job:143"), "{text}");
        assert_eq!(pty.drain(0).exit, Some(Ok(4)));
        assert!(pty.signal(0).is_err());
        pty.close().unwrap();
        drop(pty);
        assert_eq!(budget.usage().0, 0);
    }

    #[test]
    fn saturated_output_and_blocked_input_cancel_without_waiting_for_child_io() {
        let budget = Budget::new(8 * 1024 * 1024);
        let (wake, wakes) = waker();
        let script = "trap '' HUP; while :; do printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'; done";
        let mut pty = Pty::spawn(&command(script), 24, 80, &budget, wake).unwrap();
        // The reader waits once it has queued a batch; nothing drains it.
        {
            let mut state = pty.output.state.lock();
            while state.chunks.is_empty() {
                pty.output.changed.wait(&mut state);
            }
        }
        wakes.recv().unwrap();
        assert!(pty.queued_bytes().1 <= OUTPUT);
        let _ = pty.write(&[b'a'; INPUT]);
        let _ = pty.write(&[b'b'; INPUT]);
        assert!(pty.queued_bytes().0 <= INPUT);
        pty.close().unwrap();
        drop(pty);
        assert_eq!(budget.usage().0, 0);
    }

    #[test]
    fn idle_reader_cancellation_leaves_no_threads_or_reservations() {
        let budget = Budget::new(8 * 1024 * 1024);
        for _ in 0..20 {
            let mut pty = Pty::spawn(&command("read x"), 8, 20, &budget, Arc::new(|| {})).unwrap();
            pty.close().unwrap();
            drop(pty);
            assert_eq!(budget.usage().0, 0);
        }
    }
}
