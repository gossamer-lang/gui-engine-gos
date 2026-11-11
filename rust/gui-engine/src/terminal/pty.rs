//! Unix pseudo-terminal sessions. Blocking readiness waits are interrupted
//! through a socket pair, and fixed reservations cover the queues, IO buffers
//! and the IO threads' stacks.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use parking_lot_core::SpinWait;
use portable_pty::{Child, CommandBuilder, MasterPty, PtyPair};

use super::Command;
use super::transport::{
    BATCH, CHUNK, Drain, Ending, Handoff, INPUT, Notify, Output, Queue, STACK, Sink, Slot,
    check_command, pty_size,
};
use crate::memory::{Budget, Lease};

/// A program running on a pseudo-terminal, with its IO threads.
pub struct Pty {
    master: Option<Box<dyn MasterPty + Send>>,
    input: Arc<Queue>,
    ending: Arc<Ending>,
    cancel: UnixStream,
    handoff: Arc<Handoff>,
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

/// The working directory of process `pid`.
#[cfg(target_os = "freebsd")]
fn process_cwd(pid: i32) -> Option<String> {
    // SAFETY: the record is plain old data, which all zeros initializes.
    let mut file: libc::kinfo_file = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::kinfo_file>();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_CWD, pid];
    // SAFETY: `mib` names one process's working-directory record, and
    // `file` / `len` describe writable storage of that record's size.
    let read = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            4,
            (&raw mut file).cast(),
            &raw mut len,
            std::ptr::null(),
            0,
        )
    };
    if read != 0 || len == 0 {
        return None;
    }
    // SAFETY: the kernel writes a NUL-terminated path into the record.
    let path = unsafe { std::ffi::CStr::from_ptr(file.kf_path.as_ptr()) };
    path.to_str().ok().map(str::to_owned)
}

/// The name of process `pid`'s program.
#[cfg(target_os = "freebsd")]
fn process_name(pid: i32) -> Option<String> {
    // SAFETY: the record is plain old data, which all zeros initializes.
    let mut info: libc::kinfo_proc = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::kinfo_proc>();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, pid];
    // SAFETY: `mib` names one process's record, and `info` / `len` describe
    // writable storage of that record's size.
    let read = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            4,
            (&raw mut info).cast(),
            &raw mut len,
            std::ptr::null(),
            0,
        )
    };
    if read != 0 || len != std::mem::size_of::<libc::kinfo_proc>() {
        return None;
    }
    // SAFETY: the kernel writes a NUL-terminated command name into the record.
    let name = unsafe { std::ffi::CStr::from_ptr(info.ki_comm.as_ptr()) };
    let name = name.to_str().ok()?;
    (!name.is_empty()).then(|| name.to_string())
}

/// Other systems name no process's directory to an unprivileged reader.
#[cfg(not(any(target_os = "linux", target_os = "freebsd", target_os = "macos")))]
fn process_cwd(_pid: i32) -> Option<String> {
    None
}

/// The name of process `pid`'s program.
#[cfg(target_os = "linux")]
fn process_name(pid: i32) -> Option<String> {
    let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let name = name.trim_end();
    (!name.is_empty()).then(|| name.to_string())
}

/// The name of process `pid`'s program.
#[cfg(target_os = "macos")]
fn process_name(pid: i32) -> Option<String> {
    let mut name = [0u8; 256];
    // SAFETY: `name` is writable storage of the length given.
    let len = unsafe { libc::proc_name(pid, name.as_mut_ptr().cast(), name.len() as u32) };
    (len > 0).then(|| String::from_utf8_lossy(&name[..len as usize]).into_owned())
}

/// Other systems name no process's program to an unprivileged reader.
#[cfg(not(any(target_os = "linux", target_os = "freebsd", target_os = "macos")))]
fn process_name(_pid: i32) -> Option<String> {
    None
}

/// What a wait in `readiness` ended on.
#[derive(Debug, PartialEq, Eq)]
enum Wake {
    /// The descriptor is ready.
    Ready,
    /// The cancel descriptor is readable.
    Cancelled,
}

/// Waits until `fd` is ready for `events` or `cancel` is readable. If `owed`
/// is set, first returns this thread's freed memory to the runtime, and
/// clears `owed` once that is done.
fn readiness(fd: i32, cancel: i32, events: i16, owed: &mut bool) -> Result<Wake, String> {
    let pollfd = |fd, events| libc::pollfd {
        fd,
        events,
        revents: 0,
    };
    let mut fds = [pollfd(fd, events), pollfd(cancel, libc::POLLIN)];
    loop {
        let mut bound = -1;
        if *owed {
            match windowing::idle::thread_idle() {
                Some(due) => {
                    bound = i32::try_from(due.as_micros().div_ceil(1000)).unwrap_or(i32::MAX);
                }
                None => *owed = false,
            }
        }
        // SAFETY: `fds` holds two initialized pollfd records; the PTY
        // descriptor stays open until both IO threads have joined and the
        // cancel descriptor is owned by the calling thread.
        let result = unsafe { libc::poll(fds.as_mut_ptr(), 2, bound) };
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
            return Ok(Wake::Cancelled);
        }
        return Ok(Wake::Ready);
    }
}

fn would_block(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
    )
}

// The search the program's start makes (portable-pty's), so a program it
// would not find is refused when the terminal opens: a relative path that
// begins with `.` or `..` is taken from the working directory, any other
// relative path is looked for along PATH, and the file must be executable.
fn resolve(builder: &CommandBuilder, program: &str) -> Result<(), String> {
    use std::ffi::{CString, OsStr};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Component, Path, PathBuf};

    let runnable = |path: &Path| {
        !path.is_dir()
            && CString::new(path.as_os_str().as_bytes())
                // SAFETY: `path` is a NUL-terminated string that outlives the call.
                .is_ok_and(|path| unsafe { libc::access(path.as_ptr(), libc::X_OK) } == 0)
    };
    let exe = Path::new(program);
    if exe.is_absolute() {
        return if runnable(exe) {
            Ok(())
        } else {
            Err(format!("{program} is not an executable program"))
        };
    }
    let dir: PathBuf = builder
        .get_cwd()
        .map(PathBuf::from)
        .filter(|dir| dir.is_dir())
        .or_else(|| builder.get_env("HOME").map(PathBuf::from))
        .unwrap_or_default();
    if matches!(
        exe.components().next(),
        Some(Component::CurDir | Component::ParentDir)
    ) {
        return if runnable(&dir.join(exe)) {
            Ok(())
        } else {
            Err(format!("{program} is not an executable program"))
        };
    }
    let path = builder.get_env("PATH").unwrap_or(OsStr::new(""));
    if std::env::split_paths(path).any(|entry| runnable(&dir.join(entry).join(exe))) {
        Ok(())
    } else {
        Err(format!("{program} was not found on PATH"))
    }
}

/// A pseudo-terminal opened for a program that has not started yet, so the
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
        let ending = Arc::new(Ending::default());
        let stopping = Arc::new(AtomicBool::new(false));
        let handoff = Arc::new(Handoff::new());
        let mut pty = Pty {
            master: Some(pair.master),
            input: input.clone(),
            ending: ending.clone(),
            cancel,
            handoff: handoff.clone(),
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
        let end = ending.clone();
        let stop = stopping.clone();
        let wake = notify.clone();
        let mut output = Output::new(sink, notify.clone(), handoff)?;
        pty.reader = Some(
            thread::Builder::new()
                .name("gui-pty-read".into())
                .stack_size(STACK)
                .spawn(move || {
                    super::transport::interactive_thread();
                    let mut buffer = [0; CHUNK];
                    // A streaming program refills the terminal almost at
                    // once, and on macOS each read gets only a kilobyte, so
                    // waiting in poll after every read would cost as much as
                    // the read. The reader spins briefly first, and hands
                    // output to the parser only when it is about to wait.
                    let mut spin = SpinWait::new();
                    // The reader allocates only when it parses, so it has
                    // memory to return only after a parse. The parser
                    // returns everything else.
                    let mut owed = false;
                    let ended = loop {
                        if stop.load(Ordering::Acquire) {
                            break false;
                        }
                        match reader.read(&mut buffer) {
                            Ok(0) => break true,
                            Ok(n) => {
                                spin.reset();
                                if !output.push(&buffer[..n]) {
                                    break false;
                                }
                            }
                            Err(error) if would_block(&error) => {
                                if spin.spin() {
                                    continue;
                                }
                                spin.reset();
                                if output.idle() {
                                    owed = true;
                                    continue;
                                }
                                if readiness(fd, signal.as_raw_fd(), libc::POLLIN, &mut owed)
                                    != Ok(Wake::Ready)
                                {
                                    break false;
                                }
                                output.rested();
                            }
                            Err(_) => break true,
                        }
                    };
                    // The program's last output is parsed before its end is
                    // known; a cancelled reader drops what is left.
                    if ended {
                        output.finish();
                    } else {
                        drop(output);
                    }
                    end.finish(true, None);
                    wake.fire();
                })
                .map_err(|e| e.to_string())?,
        );
        let stop = stopping.clone();
        let wake = notify.clone();
        pty.writer = Some(
            thread::Builder::new()
                .name("gui-pty-write".into())
                .stack_size(STACK)
                .spawn(move || {
                    while let Some(bytes) = input.pop() {
                        let mut offset = 0;
                        while offset < bytes.len() && !stop.load(Ordering::Acquire) {
                            if readiness(fd, write_signal.as_raw_fd(), libc::POLLOUT, &mut true)
                                != Ok(Wake::Ready)
                            {
                                return;
                            }
                            match writer.write(&bytes[offset..]) {
                                Ok(0) => return,
                                Ok(n) => offset += n,
                                Err(error) if would_block(&error) => (),
                                Err(_) => return,
                            }
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
                                libc::id_t::from(pid),
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
                    ending.finish(false, Some(result));
                    notify.fire();
                })
                .map_err(|e| e.to_string())?,
        );
        Ok(pty)
    }
}

impl Pty {
    /// Opens a pseudo-terminal of `rows` x `cols` for `command`, refusing a
    /// program the start would not find; `wake` runs on an IO thread when
    /// output, end of stream or exit arrives once it starts.
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
        let dimensions = pty_size(rows, cols, cell)?;
        let lease = budget.reserve(3 * INPUT + 4 * STACK + 4 * CHUNK + 3 * BATCH + 64 * 1024)?;
        let mut builder = CommandBuilder::new(&command.program);
        builder.args(&command.args);
        super::transport::environment(&mut builder, command);
        if !command.cwd.is_empty() {
            builder.cwd(&command.cwd);
        }
        resolve(&builder, &command.program)?;
        let pair = portable_pty::native_pty_system()
            .openpty(dimensions)
            .map_err(|e| e.to_string())?;
        Ok(Opened {
            pair,
            builder,
            slot,
            lease,
            wake,
            sink,
        })
    }

    /// Starts `command` on a new pseudo-terminal of `rows` x `cols`, as
    /// `open` and `Opened::start` do together.
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
        // Input with none queued before it goes to the program from here,
        // sparing the writer thread's wake; what the kernel leaves, it
        // writes.
        let fd = self.master.as_ref().and_then(|m| m.as_raw_fd());
        self.input
            .write_or_push(bytes, |bytes| match fd {
                Some(fd) => {
                    // SAFETY: `fd` is the open, nonblocking master descriptor
                    // `master` owns, and `bytes` is valid for its length.
                    let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
                    usize::try_from(n).unwrap_or(0)
                }
                None => 0,
            })
            .map(|_| ())
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

    pub fn has_pending_output(&self) -> bool {
        self.notify.is_pending()
    }

    /// Asks the parser thread to return memory that other threads freed for
    /// it, such as rows dropped when history is compacted. The parser
    /// allocated those rows, so it is the thread that must return them.
    pub fn release_memory(&self) {
        self.handoff.release();
    }

    /// Input bytes waiting for the program.
    pub fn queued_input(&self) -> usize {
        self.input.bytes()
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

    /// The program name of the job in the terminal's foreground, when a job
    /// other than the program runs there.
    pub fn job(&self) -> Option<String> {
        let fd = self.master.as_ref().and_then(|m| m.as_raw_fd())?;
        // SAFETY: `fd` is the open master descriptor.
        let foreground = unsafe { libc::tcgetpgrp(fd) };
        (foreground > 0 && foreground != self.pid as i32)
            .then(|| process_name(foreground))
            .flatten()
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
        self.handoff.close();
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

    /// A sink that keeps what the reader hands it.
    fn sink() -> (Sink, Arc<parking_lot::Mutex<Vec<u8>>>) {
        let kept = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let into = kept.clone();
        (
            Box::new(move |bytes: &[u8]| into.lock().extend_from_slice(bytes)),
            kept,
        )
    }

    /// Waits until `done` holds for the output so far and the drain state.
    fn collect(
        pty: &Pty,
        wakes: &mpsc::Receiver<()>,
        kept: &parking_lot::Mutex<Vec<u8>>,
        mut done: impl FnMut(&str, &Drain) -> bool,
    ) -> String {
        loop {
            let got = pty.drain();
            let output = String::from_utf8_lossy(&kept.lock()).into_owned();
            if done(&output, &got) {
                return output;
            }
            wakes.recv().unwrap();
        }
    }

    #[test]
    fn real_pty_has_tty_unicode_input_resize_exit_and_all_threads_join() {
        let budget = Budget::new(8 * 1024 * 1024);
        let (wake, wakes) = waker();
        let (output, kept) = sink();
        let script = "test -t 0 || exit 8; printf 'ready\\r\\n'; read word; \
                      printf 'got:%s\\r\\n' \"$word\"; stty size; exit 7";
        let mut pty = Pty::spawn(&command(script), 12, 44, (0, 0), &budget, wake, output).unwrap();
        pty.resize(18, 66, (0, 0)).unwrap();
        let mut sent = false;
        let text = collect(&pty, &wakes, &kept, |text, got| {
            if !sent && text.contains("ready") {
                pty.write("héllo 界\n".as_bytes()).unwrap();
                sent = true;
            }
            got.eof && got.exit.is_some()
        });
        assert!(text.contains("got:héllo 界"), "{text}");
        assert!(text.contains("18 66"), "{text}");
        assert_eq!(pty.drain().exit, Some(Ok(7)));
        pty.close().unwrap();
        drop(pty);
        assert_eq!(budget.usage().0, 0);
    }

    #[test]
    fn a_stream_outrunning_its_parse_goes_through_the_parser_whole_and_before_its_end() {
        let budget = Budget::new(8 * 1024 * 1024);
        let (wake, wakes) = waker();
        let kept = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let parsers = Arc::new(parking_lot::Mutex::new(std::collections::BTreeSet::new()));
        let (into, seen) = (kept.clone(), parsers.clone());
        // Each parse takes long enough for more of the stream to arrive.
        let output: Sink = Box::new(move |bytes: &[u8]| {
            thread::sleep(std::time::Duration::from_millis(2));
            into.lock().extend_from_slice(bytes);
            seen.lock()
                .insert(thread::current().name().map(str::to_owned));
        });
        let mut pty = Pty::spawn(
            // Output passes untranslated, so the bytes kept are the ones
            // the program wrote.
            &command("stty -onlcr; seq 1 40000"),
            24,
            80,
            (0, 0),
            &budget,
            wake,
            output,
        )
        .unwrap();
        let text = collect(&pty, &wakes, &kept, |_, got| got.eof);
        let expected: String = (1..=40000).map(|n| format!("{n}\n")).collect();
        assert!(
            text == expected,
            "{} bytes of {}",
            text.len(),
            expected.len()
        );
        assert!(parsers.lock().contains(&Some("gui-term-parse".to_owned())));
        pty.close().unwrap();
        drop(pty);
        assert_eq!(budget.usage().0, 0);
    }

    #[test]
    fn control_c_reaches_the_real_foreground_process_group() {
        let budget = Budget::new(8 * 1024 * 1024);
        let (wake, wakes) = waker();
        let (output, kept) = sink();
        let mut pty = Pty::spawn(
            &command("printf ready; exec sleep 20"),
            24,
            80,
            (0, 0),
            &budget,
            wake,
            output,
        )
        .unwrap();
        let mut sent = false;
        collect(&pty, &wakes, &kept, |text, got| {
            if !sent && text.contains("ready") {
                pty.write(b"\x03").unwrap();
                sent = true;
            }
            got.exit.is_some()
        });
        assert_eq!(pty.drain().exit, Some(Ok(130)));
        pty.close().unwrap();
        drop(pty);
        assert_eq!(budget.usage().0, 0);
    }

    #[test]
    fn a_sent_signal_reaches_the_foreground_job_and_not_the_shell() {
        let budget = Budget::new(8 * 1024 * 1024);
        let (wake, wakes) = waker();
        let (output, kept) = sink();
        // An interactive shell runs `sleep` as its foreground job; the shell
        // itself survives the job's SIGTERM and reports its status.
        // The job is in the foreground before it prints.
        let script = "set -m; sh -c 'printf ready; exec sleep 20'; echo \"job:$?\"; exit 4";
        let mut pty = Pty::spawn(&command(script), 24, 80, (0, 0), &budget, wake, output).unwrap();
        let mut sent = false;
        let text = collect(&pty, &wakes, &kept, |text, got| {
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
        assert_eq!(pty.drain().exit, Some(Ok(4)));
        assert!(pty.signal(0).is_err());
        pty.close().unwrap();
        drop(pty);
        assert_eq!(budget.usage().0, 0);
    }

    #[test]
    fn saturated_output_and_blocked_input_cancel_without_waiting_for_child_io() {
        let budget = Budget::new(8 * 1024 * 1024);
        let (wake, wakes) = waker();
        let (output, kept) = sink();
        let script = "trap '' HUP; while :; do printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'; done";
        let mut pty = Pty::spawn(&command(script), 24, 80, (0, 0), &budget, wake, output).unwrap();
        // The reader hands the sink one batch after another; nothing else
        // takes the output.
        while kept.lock().is_empty() {
            wakes.recv().unwrap();
        }
        let _ = pty.write(&[b'a'; INPUT]);
        let _ = pty.write(&[b'b'; INPUT]);
        assert!(pty.queued_input() <= INPUT);
        pty.close().unwrap();
        drop(pty);
        assert_eq!(budget.usage().0, 0);
    }

    #[test]
    fn idle_reader_cancellation_leaves_no_threads_or_reservations() {
        let budget = Budget::new(8 * 1024 * 1024);
        for _ in 0..20 {
            let mut pty = Pty::spawn(
                &command("read x"),
                8,
                20,
                (0, 0),
                &budget,
                Arc::new(|| {}),
                Box::new(|_: &[u8]| {}),
            )
            .unwrap();
            pty.close().unwrap();
            drop(pty);
            assert_eq!(budget.usage().0, 0);
        }
    }
}
