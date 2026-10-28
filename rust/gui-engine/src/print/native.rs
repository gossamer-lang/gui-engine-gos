//! The system's printers. Unix systems (Linux and macOS) print through
//! CUPS: the document goes to the spooler as PDF with `lp`, and a queued job
//! is withdrawn with `cancel`. Windows draws each page into a GDI print job
//! at the printer's resolution.

use serde::{Deserialize, Serialize};

/// A printer the system offers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Destination {
    pub name: String,
    pub default: bool,
}

/// How a document prints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    /// The printer; empty names the system default.
    pub destination: String,
    /// The CUPS server to print through (a host, host:port or socket path);
    /// empty uses the system's. Windows prints through its own spooler.
    pub server: String,
    pub copies: i64,
    /// Page ranges such as `1-3,5`; empty prints every page.
    pub pages: String,
    pub title: String,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            destination: String::new(),
            server: String::new(),
            copies: 1,
            pages: String::new(),
            title: "Document".into(),
        }
    }
}

impl Options {
    pub(crate) fn check(&self) -> Result<(), String> {
        if !(1..=999).contains(&self.copies) {
            return Err("copies must be 1 to 999".into());
        }
        let printable = |s: &str| s.len() <= 256 && !s.chars().any(char::is_control);
        if !printable(&self.destination) || !printable(&self.title) || !printable(&self.server) {
            return Err("the printer or title is not printable text".into());
        }
        if !self.pages.is_empty() {
            ranges(&self.pages, u32::MAX)?;
        }
        Ok(())
    }
}

/// The 1-based page numbers `pages` names, within `count` pages.
pub(crate) fn ranges(pages: &str, count: u32) -> Result<Vec<u32>, String> {
    let mut out = Vec::new();
    for part in pages.split(',') {
        let part = part.trim();
        let (a, b) = part.split_once('-').unwrap_or((part, part));
        let a: u32 = a
            .trim()
            .parse()
            .map_err(|_| format!("bad page range {part}"))?;
        let b: u32 = b
            .trim()
            .parse()
            .map_err(|_| format!("bad page range {part}"))?;
        if a == 0 || b < a {
            return Err(format!("bad page range {part}"));
        }
        out.extend((a..=b.min(count)).take(100_000));
    }
    Ok(out)
}

#[cfg(unix)]
mod cups {
    use super::{Destination, Options};
    use std::io::Write;
    use std::process::{Command, Stdio};

    fn run(program: &str, server: &str, args: &[&str]) -> Result<String, String> {
        let mut command = Command::new(program);
        if !server.is_empty() {
            command.args(["-h", server]);
        }
        let output = command
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("{program}: {e}"))?;
        let take = |bytes: &[u8]| {
            String::from_utf8_lossy(&bytes[..bytes.len().min(64 * 1024)])
                .trim()
                .to_string()
        };
        if output.status.success() {
            return Ok(take(&output.stdout));
        }
        // CUPS tools name themselves at the start of their own messages.
        let why = take(&output.stderr);
        Err(if why.is_empty() {
            format!("{program} failed: {}", output.status)
        } else if why.starts_with(&format!("{program}:")) {
            why
        } else {
            format!("{program}: {why}")
        })
    }

    pub fn destinations(server: &str) -> Result<Vec<Destination>, String> {
        let names = run("lpstat", server, &["-e"])?;
        // `lpstat -d` fails when no default is set, which is not an error here.
        let default = run("lpstat", server, &["-d"]).unwrap_or_default();
        let default = default
            .rsplit_once(": ")
            .map(|(_, name)| name.trim().to_string())
            .unwrap_or_default();
        Ok(names
            .lines()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(|name| Destination {
                name: name.to_string(),
                default: name == default,
            })
            .collect())
    }

    /// Spools `pdf`, answering the spooler's job id.
    pub fn submit(pdf: &[u8], media: &str, options: &Options) -> Result<String, String> {
        let dir = std::env::temp_dir();
        let mut random = [0u8; 16];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut random))
            .map_err(|e| e.to_string())?;
        let file = dir.join(format!(
            "gui-print-{}.pdf",
            random
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        ));
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut out = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&file)
                .map_err(|e| e.to_string())?;
            out.write_all(pdf).map_err(|e| e.to_string())?;
        }
        let _remove = scopeguard::guard(file.clone(), |file| {
            let _ = std::fs::remove_file(file);
        });
        let copies = options.copies.to_string();
        let mut args: Vec<String> = Vec::new();
        if !options.destination.is_empty() {
            args.extend(["-d".into(), options.destination.clone()]);
        }
        args.extend(["-t".into(), options.title.clone(), "-n".into(), copies]);
        args.extend(["-o".into(), format!("media={media}")]);
        if !options.pages.is_empty() {
            args.extend(["-o".into(), format!("page-ranges={}", options.pages)]);
        }
        args.push("--".into());
        args.push(file.to_string_lossy().into_owned());
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let answer = run("lp", &options.server, &refs)?;
        // lp answers "request id is <job> (1 file(s))".
        answer
            .split_whitespace()
            .skip_while(|word| *word != "is")
            .nth(1)
            .map(str::to_string)
            .ok_or_else(|| format!("lp did not name its job: {answer}"))
    }

    pub fn cancel(server: &str, job: &str) -> Result<(), String> {
        if job.is_empty() || job.starts_with('-') || job.len() > 256 {
            return Err("not a print job".into());
        }
        run("cancel", server, &[job]).map(|_| ())
    }
}

#[cfg(windows)]
mod gdi {
    use super::{Destination, Options};
    use crate::print::Page;
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateDCW, DIB_RGB_COLORS, DeleteDC, GetDeviceCaps,
        HORZRES, LOGPIXELSX, LOGPIXELSY, SRCCOPY, StretchDIBits, VERTRES,
    };
    use windows_sys::Win32::Graphics::Printing::{
        ClosePrinter, EnumPrintersW, GetDefaultPrinterW, JOB_CONTROL_DELETE, OpenPrinterW,
        PRINTER_ENUM_CONNECTIONS, PRINTER_ENUM_LOCAL, PRINTER_HANDLE, PRINTER_INFO_4W, SetJobW,
    };
    use windows_sys::Win32::Storage::Xps::{
        AbortDoc, DOCINFOW, EndDoc, EndPage, StartDocW, StartPage,
    };

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn from_wide(ptr: *const u16) -> String {
        if ptr.is_null() {
            return String::new();
        }
        // SAFETY: the pointer is a NUL-terminated string the spooler wrote.
        let len = (0..).take_while(|&i| unsafe { *ptr.add(i) } != 0).count();
        // SAFETY: `len` units precede the terminator.
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(ptr, len) })
    }

    fn default_printer() -> String {
        let mut size = 0u32;
        // SAFETY: a null buffer asks for the needed size.
        unsafe { GetDefaultPrinterW(std::ptr::null_mut(), &mut size) };
        let mut buffer = vec![0u16; size as usize + 1];
        // SAFETY: the buffer holds `size` units.
        if unsafe { GetDefaultPrinterW(buffer.as_mut_ptr(), &mut size) } == 0 {
            return String::new();
        }
        from_wide(buffer.as_ptr())
    }

    pub fn destinations(_server: &str) -> Result<Vec<Destination>, String> {
        let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
        let (mut needed, mut count) = (0u32, 0u32);
        // SAFETY: a zero-sized query answers the needed byte count.
        unsafe {
            EnumPrintersW(
                flags,
                std::ptr::null(),
                4,
                std::ptr::null_mut(),
                0,
                &mut needed,
                &mut count,
            )
        };
        let mut buffer = vec![0u8; needed as usize];
        // SAFETY: `buffer` holds `needed` bytes for level-4 records.
        if unsafe {
            EnumPrintersW(
                flags,
                std::ptr::null(),
                4,
                buffer.as_mut_ptr(),
                needed,
                &mut needed,
                &mut count,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let default = default_printer();
        let records = buffer.as_ptr().cast::<PRINTER_INFO_4W>();
        Ok((0..count as usize)
            .map(|i| {
                // SAFETY: the spooler wrote `count` records at the buffer's start.
                let name = from_wide(unsafe { (*records.add(i)).pPrinterName });
                Destination {
                    default: name == default,
                    name,
                }
            })
            .collect())
    }

    /// Prints `pages` as one GDI job, answering the spooler's job id.
    pub fn submit(
        pages: &[Page],
        options: &Options,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<String, String> {
        let printer = if options.destination.is_empty() {
            default_printer()
        } else {
            options.destination.clone()
        };
        let device = wide(&printer);
        // SAFETY: `device` is a NUL-terminated printer name.
        let dc = unsafe {
            CreateDCW(
                std::ptr::null(),
                device.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        if dc.is_null() {
            return Err(format!("cannot open printer {printer}"));
        }
        let _dc = scopeguard::guard(dc, |dc| {
            // SAFETY: the device context was created above.
            unsafe { DeleteDC(dc) };
        });
        // SAFETY: `dc` is a printer device context.
        let (dpi_x, dpi_y, width, height) = unsafe {
            (
                GetDeviceCaps(dc, LOGPIXELSX as i32),
                GetDeviceCaps(dc, LOGPIXELSY as i32),
                GetDeviceCaps(dc, HORZRES as i32),
                GetDeviceCaps(dc, VERTRES as i32),
            )
        };
        let title = wide(&options.title);
        let info = DOCINFOW {
            cbSize: size_of::<DOCINFOW>() as i32,
            lpszDocName: title.as_ptr(),
            lpszOutput: std::ptr::null(),
            lpszDatatype: std::ptr::null(),
            fwType: 0,
        };
        // SAFETY: `info` and its strings outlive the call.
        let job = unsafe { StartDocW(dc, &info) };
        if job <= 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let selected = if options.pages.is_empty() {
            (1..=pages.len() as u32).collect()
        } else {
            super::ranges(&options.pages, pages.len() as u32)?
        };
        // Rasterize at most 300 pixels per inch, stretched to the device.
        let scale = (f64::from(dpi_x.min(300)) / 72.0).max(0.5);
        for _ in 0..options.copies {
            for number in &selected {
                if cancelled() {
                    // SAFETY: the job started on `dc` above.
                    unsafe { AbortDoc(dc) };
                    return Err("printing cancelled".into());
                }
                let page = &pages[*number as usize - 1];
                let image = crate::print::preview(page, scale.min(8.0))?;
                let mut bgra = image.rgba;
                for px in bgra.as_chunks_mut::<4>().0 {
                    px.swap(0, 2);
                }
                let mut header: BITMAPINFO = unsafe { std::mem::zeroed() };
                header.bmiHeader = BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: image.width as i32,
                    biHeight: -(image.height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..unsafe { std::mem::zeroed() }
                };
                let target_w = (page.width / 72.0 * f64::from(dpi_x)).round() as i32;
                let target_h = (page.height / 72.0 * f64::from(dpi_y)).round() as i32;
                // SAFETY: the page is open on `dc`, and `bgra` matches `header`.
                let drawn = unsafe {
                    StartPage(dc);
                    let drawn = StretchDIBits(
                        dc,
                        0,
                        0,
                        target_w.min(width.max(1) * 2),
                        target_h.min(height.max(1) * 2),
                        0,
                        0,
                        image.width as i32,
                        image.height as i32,
                        bgra.as_ptr().cast(),
                        &header,
                        DIB_RGB_COLORS,
                        SRCCOPY,
                    );
                    EndPage(dc);
                    drawn
                };
                if drawn == 0 {
                    // SAFETY: the job started on `dc` above.
                    unsafe { AbortDoc(dc) };
                    return Err("the printer refused a page".into());
                }
            }
        }
        // SAFETY: the job started on `dc` above.
        if unsafe { EndDoc(dc) } <= 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(format!("{printer}\u{1f}{job}"))
    }

    pub fn cancel(_server: &str, job: &str) -> Result<(), String> {
        let (printer, id) = job.split_once('\u{1f}').ok_or("not a print job")?;
        let id: u32 = id.parse().map_err(|_| "not a print job")?;
        let name = wide(printer);
        let mut handle = PRINTER_HANDLE {
            Value: std::ptr::null_mut(),
        };
        // SAFETY: `name` is NUL-terminated; `handle` receives the printer.
        if unsafe { OpenPrinterW(name.as_ptr(), &mut handle, std::ptr::null()) } == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        // SAFETY: `handle` is open; level 0 sends only the command.
        let done = unsafe { SetJobW(handle, id, 0, std::ptr::null(), JOB_CONTROL_DELETE) };
        // SAFETY: closes the handle opened above.
        unsafe { ClosePrinter(handle) };
        if done == 0 {
            Err(std::io::Error::last_os_error().to_string())
        } else {
            Ok(())
        }
    }
}

/// The printers `server` offers ("" for the system's).
pub fn destinations(server: &str) -> Result<Vec<Destination>, String> {
    #[cfg(unix)]
    return cups::destinations(server);
    #[cfg(windows)]
    return gdi::destinations(server);
    #[cfg(not(any(unix, windows)))]
    {
        let _ = server;
        Err("this platform offers no printers".into())
    }
}

/// Sends a document to a printer, answering the spooler's job id. Unix
/// spools `pdf`; Windows draws `pages`.
pub fn submit(
    pages: &[super::Page],
    pdf: impl FnOnce() -> Result<Vec<u8>, String>,
    media: &str,
    options: &Options,
    cancelled: &dyn Fn() -> bool,
) -> Result<String, String> {
    options.check()?;
    #[cfg(unix)]
    {
        let _ = (pages, cancelled);
        cups::submit(&pdf()?, media, options)
    }
    #[cfg(windows)]
    {
        let _ = (pdf, media);
        gdi::submit(pages, options, cancelled)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (pages, pdf, media, cancelled);
        Err("this platform offers no printers".into())
    }
}

/// Withdraws a job the spooler of `server` still holds.
pub fn cancel(server: &str, job: &str) -> Result<(), String> {
    #[cfg(unix)]
    return cups::cancel(server, job);
    #[cfg(windows)]
    return gdi::cancel(server, job);
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (server, job);
        Err("this platform offers no printers".into())
    }
}
