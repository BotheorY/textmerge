// SPDX-License-Identifier: MIT
//! All HWNDs are owned and accessed by the UI thread. Worker communication uses
//! a channel and atomics; no worker ever sends synchronous messages to the GUI.
use anyhow::{Context, Result, ensure};
use std::{
    collections::HashMap,
    ffi::c_void,
    mem::{size_of, zeroed},
    path::PathBuf,
    ptr::{null, null_mut},
    sync::{
        Arc,
        atomic::Ordering,
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
};
use textmerge::{
    engine::{self, Control, MergeOptions, MergeReport, Progress},
    logging,
    settings::Settings,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{Com::*, LibraryLoader::*, SystemServices::SS_LEFT},
    UI::{
        Controls::{Dialogs::*, *},
        HiDpi::*,
        Input::KeyboardAndMouse::EnableWindow,
        Shell::*,
        WindowsAndMessaging::*,
    },
};

const SOURCE_A: i32 = 101;
const SOURCE_B: i32 = 102;
const TARGET: i32 = 103;
const TEMP: i32 = 104;
const CASE: i32 = 201;
const ORDER: i32 = 202;
const EOL: i32 = 203;
const START: i32 = 301;
const PAUSE: i32 = 302;
const CANCEL: i32 = 303;
const STATUS: i32 = 401;
const DETAILS: i32 = 402;
const BAR: i32 = 403;
const HELP: i32 = 404;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

struct Job {
    receiver: Receiver<Result<MergeReport>>,
    thread: JoinHandle<()>,
    control: Arc<Control>,
    progress: Arc<Progress>,
    paused: bool,
}
struct App {
    window: HWND,
    controls: HashMap<i32, HWND>,
    fonts: Vec<HFONT>,
    settings: Settings,
    job: Option<Job>,
    closing: bool,
    scale: f64,
}

impl App {
    fn text(&self, id: i32) -> String {
        let window = self.controls[&id];
        // SAFETY: Controls are live HWNDs owned by this UI thread; buffers include a NUL.
        unsafe {
            let length = GetWindowTextLengthW(window);
            let mut buffer = vec![0; length as usize + 1];
            let count = GetWindowTextW(window, buffer.as_mut_ptr(), buffer.len() as i32);
            String::from_utf16_lossy(&buffer[..count as usize])
        }
    }
    fn set_text(&self, id: i32, value: &str) {
        unsafe {
            SetWindowTextW(self.controls[&id], wide(value).as_ptr());
        }
    }
    fn selected(&self, id: i32) -> usize {
        unsafe { SendMessageW(self.controls[&id], CB_GETCURSEL, 0, 0).max(0) as usize }
    }
    fn collect_settings(&self) -> Settings {
        Settings {
            source_a: self.text(SOURCE_A),
            source_b: self.text(SOURCE_B),
            target: self.text(TARGET),
            temporary_directory: PathBuf::from(self.text(TEMP)),
            case_sensitive: self.selected(CASE) == 0,
            order: self.selected(ORDER),
            windows_eol: self.selected(EOL) == 0,
        }
    }
    fn save(&mut self) {
        self.settings = self.collect_settings();
        if let Err(error) = self.settings.save() {
            logging::error(&format!("Saving registry settings: {error:#}"));
            self.message(
                "Settings could not be saved",
                &format!("{error:#}\n\nCheck write access to HKCU\\Software\\TextMerge."),
                MB_ICONWARNING,
            );
        }
    }
    fn message(&self, title: &str, text: &str, flags: u32) -> i32 {
        unsafe {
            MessageBoxW(
                self.window,
                wide(text).as_ptr(),
                wide(title).as_ptr(),
                flags | MB_OK,
            )
        }
    }
    fn add(&mut self, id: i32, class: &str, text: &str, style: u32) -> Result<()> {
        let instance = unsafe { GetModuleHandleW(null()) };
        let group_box = class == "BUTTON" && style & BS_TYPEMASK as u32 == BS_GROUPBOX as u32;
        let window = unsafe {
            CreateWindowExW(
                if class == "EDIT" {
                    WS_EX_CLIENTEDGE
                } else if group_box {
                    WS_EX_TRANSPARENT
                } else {
                    0
                },
                wide(class).as_ptr(),
                wide(text).as_ptr(),
                WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS | style,
                0,
                0,
                0,
                0,
                self.window,
                id as isize as HMENU,
                instance,
                null(),
            )
        };
        ensure!(
            !window.is_null(),
            "Cannot create {class} control: {}",
            std::io::Error::last_os_error()
        );
        unsafe {
            let font = self.fonts[if id == 1 {
                1
            } else if id == STATUS {
                2
            } else {
                0
            }];
            SendMessageW(window, WM_SETFONT, font as usize, 0);
        }
        self.controls.insert(id, window);
        Ok(())
    }
    fn create_controls(&mut self) -> Result<()> {
        self.rebuild_fonts();
        self.add(1, "STATIC", "TextMerge", SS_LEFT)?;
        self.add(
            2,
            "STATIC",
            "Combine two text files. Keep each unique line once.",
            SS_LEFT,
        )?;
        self.add(3, "BUTTON", "Files", BS_GROUPBOX as u32)?;
        for (label, id, text) in [
            (11, SOURCE_A, "Source A"),
            (12, SOURCE_B, "Source B"),
            (13, TARGET, "Target (UTF-8)"),
        ] {
            self.add(label, "STATIC", text, SS_LEFT)?;
            self.add(id, "EDIT", "", WS_TABSTOP | ES_AUTOHSCROLL as u32)?;
            self.add(
                id + 1000,
                "BUTTON",
                if id == TARGET {
                    "Save as..."
                } else {
                    "Browse..."
                },
                WS_TABSTOP | BS_PUSHBUTTON as u32,
            )?;
        }
        self.add(4, "BUTTON", "Merge settings", BS_GROUPBOX as u32)?;
        for (label, text) in [
            (21, "Duplicate detection"),
            (22, "Merge order"),
            (23, "Target line endings"),
            (24, "Temporary folder"),
        ] {
            self.add(label, "STATIC", text, SS_LEFT)?;
        }
        for (id, values) in [
            (CASE, vec!["Case sensitive", "Case insensitive"]),
            (
                ORDER,
                vec![
                    "B lines above A lines (B1, A1, B2, A2)",
                    "B lines below A lines (A1, B1, A2, B2)",
                    "B before A (all B, then all A)",
                    "B after A (all A, then all B)",
                ],
            ),
            (EOL, vec!["Windows (CRLF)", "Unix (LF)"]),
        ] {
            self.add(
                id,
                "COMBOBOX",
                "",
                WS_TABSTOP | WS_VSCROLL | CBS_DROPDOWNLIST as u32,
            )?;
            for value in values {
                unsafe {
                    SendMessageW(
                        self.controls[&id],
                        CB_ADDSTRING,
                        0,
                        wide(value).as_ptr() as isize,
                    );
                }
            }
        }
        self.add(TEMP, "EDIT", "", WS_TABSTOP | ES_AUTOHSCROLL as u32)?;
        self.add(
            TEMP + 1000,
            "BUTTON",
            "Browse...",
            WS_TABSTOP | BS_PUSHBUTTON as u32,
        )?;
        self.add(HELP,"STATIC","UTF-8 or UTF-16 with BOM. First occurrence wins, including duplicates within one source.\nTemporary disk space is required; the existing target is preserved until completion.",SS_LEFT)?;
        self.add(STATUS, "STATIC", "Ready to merge", SS_LEFT)?;
        self.add(BAR, "msctls_progress32", "", 0)?;
        self.add(
            DETAILS,
            "STATIC",
            "Choose both sources and a target file to begin.",
            SS_LEFT,
        )?;
        self.add(
            START,
            "BUTTON",
            "Merge files",
            WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
        )?;
        self.add(PAUSE, "BUTTON", "Pause", WS_TABSTOP | BS_PUSHBUTTON as u32)?;
        self.add(
            CANCEL,
            "BUTTON",
            "Cancel",
            WS_TABSTOP | BS_PUSHBUTTON as u32,
        )?;
        unsafe {
            SendMessageW(self.controls[&BAR], PBM_SETRANGE32, 0, 1000);
        }
        self.set_text(SOURCE_A, &self.settings.source_a);
        self.set_text(SOURCE_B, &self.settings.source_b);
        self.set_text(TARGET, &self.settings.target);
        self.set_text(TEMP, &self.settings.temporary_directory.to_string_lossy());
        unsafe {
            SendMessageW(
                self.controls[&CASE],
                CB_SETCURSEL,
                usize::from(!self.settings.case_sensitive),
                0,
            );
            SendMessageW(self.controls[&ORDER], CB_SETCURSEL, self.settings.order, 0);
            SendMessageW(
                self.controls[&EOL],
                CB_SETCURSEL,
                usize::from(!self.settings.windows_eol),
                0,
            );
        }
        self.enabled(false);
        self.layout();
        Ok(())
    }
    fn rebuild_fonts(&mut self) {
        // Controls receive the new font before old GDI objects are destroyed.
        let mut fonts = Vec::new();
        for (height, weight) in [(16, 400), (30, 600), (16, 600)] {
            fonts.push(unsafe {
                CreateFontW(
                    -(height as f64 * self.scale) as i32,
                    0,
                    0,
                    0,
                    weight,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET as u32,
                    OUT_DEFAULT_PRECIS as u32,
                    CLIP_DEFAULT_PRECIS as u32,
                    CLEARTYPE_QUALITY as u32,
                    DEFAULT_PITCH as u32,
                    wide("Segoe UI").as_ptr(),
                )
            });
        }
        for (&id, &window) in &self.controls {
            unsafe {
                SendMessageW(
                    window,
                    WM_SETFONT,
                    fonts[if id == 1 {
                        1
                    } else if id == STATUS {
                        2
                    } else {
                        0
                    }] as usize,
                    0,
                );
            }
        }
        for font in self.fonts.drain(..) {
            unsafe {
                DeleteObject(font);
            }
        }
        self.fonts = fonts;
    }
    fn layout(&self) {
        let mut rect = RECT::default();
        unsafe {
            if GetClientRect(self.window, &mut rect) == 0 || rect.right <= 0 || rect.bottom <= 0 {
                return;
            }
        }
        let width = rect.right as f64 / self.scale;
        let panel = width as i32 - 48;
        let mut placements = Vec::with_capacity(self.controls.len());
        let mut place = |id: i32, x: i32, y: i32, w: i32, h: i32| {
            if let Some(&window) = self.controls.get(&id) {
                placements.push((
                    id,
                    window,
                    (x as f64 * self.scale).round() as i32,
                    (y as f64 * self.scale).round() as i32,
                    (w as f64 * self.scale).round().max(1.0) as i32,
                    (h as f64 * self.scale).round().max(1.0) as i32,
                ));
            }
        };
        place(1, 28, 20, panel, 42);
        place(2, 28, 66, panel, 24);
        place(3, 24, 106, panel, 166);
        for (row, id, label) in [(0, SOURCE_A, 11), (1, SOURCE_B, 12), (2, TARGET, 13)] {
            let y = 138 + row * 42;
            place(label, 40, y + 5, 126, 22);
            place(id, 168, y, panel - 264, 28);
            place(id + 1000, 24 + panel - 104, y, 88, 28);
        }
        place(4, 24, 290, panel, 212);
        for (row, id, label) in [(0, CASE, 21), (1, ORDER, 22), (2, EOL, 23)] {
            let y = 322 + row * 40;
            place(label, 40, y + 5, 160, 24);
            place(id, 212, y, panel - 204, 220);
        }
        place(24, 40, 447, 160, 24);
        place(TEMP, 212, 442, panel - 316, 28);
        place(TEMP + 1000, 24 + panel - 104, 442, 88, 28);
        place(HELP, 28, 515, panel, 46);
        place(STATUS, 28, 578, panel, 24);
        place(BAR, 28, 610, panel - 8, 18);
        place(DETAILS, 28, 638, panel, 40);
        place(START, 28, 696, 160, 36);
        place(PAUSE, 200, 696, 100, 36);
        place(CANCEL, 312, 696, 100, 36);
        // Group boxes overlap their contents as sibling windows. Keep their
        // frames behind every interactive control, and clip sibling painting.
        // Redrawing controls during individual MoveWindow calls lets a group
        // background erase fields already painted at their new dimensions.
        let flags = |id| {
            SWP_NOACTIVATE
                | SWP_NOREDRAW
                | SWP_NOCOPYBITS
                | if id == 3 || id == 4 { 0 } else { SWP_NOZORDER }
        };
        let after = |id| {
            if id == 3 || id == 4 {
                HWND_BOTTOM
            } else {
                null_mut()
            }
        };
        unsafe {
            let mut batch = BeginDeferWindowPos(placements.len() as i32);
            for &(id, window, x, y, width, height) in &placements {
                if batch.is_null() {
                    break;
                }
                batch = DeferWindowPos(batch, window, after(id), x, y, width, height, flags(id));
            }
            // A failed DeferWindowPos invalidates the batch handle. Reapply the
            // entire layout individually, still suppressing intermediate paint.
            let applied = !batch.is_null() && EndDeferWindowPos(batch) != 0;
            if !applied {
                for &(id, window, x, y, width, height) in &placements {
                    if SetWindowPos(window, after(id), x, y, width, height, flags(id)) == 0 {
                        logging::error(&format!(
                            "Cannot resize control {id}: {}",
                            std::io::Error::last_os_error()
                        ));
                    }
                }
            }
            // Invalidate the background, child clients, and edit-control borders
            // together. In particular, WS_CLIPCHILDREN alone does not invalidate
            // children when the parent is restored from its maximized state.
            RedrawWindow(
                self.window,
                null(),
                null_mut(),
                RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN,
            );
        }
    }
    fn enabled(&self, running: bool) {
        for id in [
            SOURCE_A,
            SOURCE_B,
            TARGET,
            TEMP,
            SOURCE_A + 1000,
            SOURCE_B + 1000,
            TARGET + 1000,
            TEMP + 1000,
            CASE,
            ORDER,
            EOL,
            START,
        ] {
            unsafe {
                EnableWindow(self.controls[&id], i32::from(!running));
            }
        }
        unsafe {
            EnableWindow(self.controls[&PAUSE], i32::from(running));
            EnableWindow(self.controls[&CANCEL], i32::from(running));
        }
    }
    fn browse(&self, id: i32) {
        if id == TEMP {
            unsafe {
                let mut info: BROWSEINFOW = zeroed();
                info.hwndOwner = self.window;
                let title = wide("Choose a temporary folder with enough free disk space");
                info.lpszTitle = title.as_ptr();
                info.ulFlags = BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE | BIF_EDITBOX;
                let item = SHBrowseForFolderW(&info);
                if !item.is_null() {
                    let mut path = vec![0u16; 32768];
                    if SHGetPathFromIDListEx(item, path.as_mut_ptr(), path.len() as u32, 0) != 0 {
                        let end = path.iter().position(|&v| v == 0).unwrap_or(path.len());
                        self.set_text(TEMP, &String::from_utf16_lossy(&path[..end]));
                    }
                    CoTaskMemFree(item as *const c_void);
                }
            }
            return;
        }
        let mut buffer = vec![0u16; 32768];
        let initial = wide(&self.text(id));
        let length = (initial.len() - 1).min(buffer.len() - 1);
        buffer[..length].copy_from_slice(&initial[..length]);
        let filter =
            wide("Text files (*.txt;*.log;*.csv)\0*.txt;*.log;*.csv\0All files (*.*)\0*.*\0");
        let title = wide(if id == TARGET {
            "Save merged UTF-8 file"
        } else if id == SOURCE_A {
            "Open source A"
        } else {
            "Open source B"
        });
        let extension = wide("txt");
        unsafe {
            let mut dialog: OPENFILENAMEW = zeroed();
            dialog.lStructSize = size_of::<OPENFILENAMEW>() as u32;
            dialog.hwndOwner = self.window;
            dialog.lpstrFilter = filter.as_ptr();
            dialog.lpstrFile = buffer.as_mut_ptr();
            dialog.nMaxFile = buffer.len() as u32;
            dialog.lpstrTitle = title.as_ptr();
            dialog.lpstrDefExt = extension.as_ptr();
            dialog.Flags = OFN_EXPLORER
                | OFN_NOCHANGEDIR
                | OFN_PATHMUSTEXIST
                | if id == TARGET { 0 } else { OFN_FILEMUSTEXIST };
            let ok = if id == TARGET {
                GetSaveFileNameW(&mut dialog)
            } else {
                GetOpenFileNameW(&mut dialog)
            };
            if ok != 0 {
                let end = buffer.iter().position(|&v| v == 0).unwrap_or(buffer.len());
                self.set_text(id, &String::from_utf16_lossy(&buffer[..end]));
            } else {
                let code = CommDlgExtendedError();
                if code != 0 {
                    logging::error(&format!("File dialog failed: {code}"));
                    self.message("File dialog error",&format!("Windows file dialog failed (code {code}). You can enter the full path manually."),MB_ICONERROR);
                }
            }
        }
    }
    fn start(&mut self) -> Result<()> {
        let settings = self.collect_settings();
        ensure!(
            !settings.source_a.trim().is_empty()
                && !settings.source_b.trim().is_empty()
                && !settings.target.trim().is_empty(),
            "Choose source A, source B, and the target file."
        );
        ensure!(
            !settings.temporary_directory.as_os_str().is_empty(),
            "Choose a temporary folder."
        );
        let exists = PathBuf::from(&settings.target).exists();
        if exists
            && unsafe {
                MessageBoxW(
                    self.window,
                    wide(&format!(
                        "Replace this file after the merge completes?\n\n{}",
                        settings.target
                    ))
                    .as_ptr(),
                    wide("Confirm target replacement").as_ptr(),
                    MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
                )
            } != IDYES
        {
            return Ok(());
        }
        let options = MergeOptions {
            source_a: settings.source_a.into(),
            source_b: settings.source_b.into(),
            target: settings.target.into(),
            temporary_directory: settings.temporary_directory,
            case_sensitive: settings.case_sensitive,
            order: settings.order,
            windows_eol: settings.windows_eol,
            overwrite: exists,
        };
        let control = Arc::new(Control::default());
        let progress = Arc::new(Progress::default());
        progress.set_stage("Preparing merge");
        let (sender, receiver) = mpsc::channel();
        let worker_control = control.clone();
        let worker_progress = progress.clone();
        let thread = thread::Builder::new()
            .name("textmerge-worker".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    engine::merge(&options, worker_control, worker_progress)
                }))
                .unwrap_or_else(|_| {
                    Err(anyhow::anyhow!(
                        "The merge worker encountered an internal error. See the error log."
                    ))
                });
                let _ = sender.send(result);
            })
            .context("Cannot start background worker")?;
        self.job = Some(Job {
            receiver,
            thread,
            control,
            progress,
            paused: false,
        });
        self.enabled(true);
        self.set_text(PAUSE, "Pause");
        self.set_text(STATUS, "Preparing merge");
        self.set_text(DETAILS, "");
        Ok(())
    }
    fn tick(&mut self) {
        let Some(job) = &self.job else {
            return;
        };
        let progress = &job.progress;
        let read = progress.input_bytes.load(Ordering::Relaxed);
        let total = progress.total_bytes.load(Ordering::Relaxed);
        let unique = progress.unique_lines.load(Ordering::Relaxed);
        let lines = progress.lines.load(Ordering::Relaxed);
        let stage = progress
            .stage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let fraction = if total == 0 {
            0
        } else {
            ((read as f64 / total as f64) * 990.0).min(990.0) as usize
        };
        unsafe {
            SendMessageW(self.controls[&BAR], PBM_SETPOS, fraction, 0);
        }
        self.set_text(
            STATUS,
            if job.paused {
                "Paused — click Resume to continue"
            } else {
                &stage
            },
        );
        self.set_text(
            DETAILS,
            &format!(
                "{:.1} / {:.1} MiB read  |  {} lines  |  {} unique  |  {} duplicates",
                read as f64 / 1048576.0,
                total as f64 / 1048576.0,
                lines,
                unique,
                lines.saturating_sub(unique)
            ),
        );
        let result = match job.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Disconnected) => Some(Err(anyhow::anyhow!(
                "The background worker stopped unexpectedly"
            ))),
            Err(mpsc::TryRecvError::Empty) => None,
        };
        if let Some(result) = result {
            if let Some(job) = self.job.take() {
                let _ = job.thread.join();
            }
            self.enabled(false);
            self.set_text(PAUSE, "Pause");
            match result {
                Ok(report) => {
                    self.set_text(STATUS, "Merge complete");
                    self.set_text(DETAILS,&format!("Saved {} unique lines; removed {} duplicates. {:.1} MiB written in {:.2} seconds.",report.unique_lines,report.lines-report.unique_lines,report.output_bytes as f64/1048576.0,report.seconds));
                    unsafe {
                        SendMessageW(self.controls[&BAR], PBM_SETPOS, 1000, 0);
                    }
                }
                Err(error) if error.is::<engine::Cancelled>() => {
                    self.set_text(STATUS, "Cancelled — target preserved");
                    self.set_text(DETAILS, "Temporary job files have been removed.");
                }
                Err(error) => {
                    let log = logging::error(&format!("Merge failed: {error:#}"));
                    self.set_text(STATUS, "Merge failed — target preserved");
                    let detail=format!("{error:#}\n\n{}",log.map(|p|format!("Error log: {}",p.display())).unwrap_or_else(||"The error log could not be written; check disk access and free space.".into()));
                    self.set_text(DETAILS, &format!("{error:#}"));
                    self.message("Merge failed", &detail, MB_ICONERROR);
                }
            }
            if self.closing {
                self.save();
                unsafe {
                    DestroyWindow(self.window);
                }
            }
        }
    }
    fn command(&mut self, id: i32) {
        if (1101..=1104).contains(&id) {
            self.browse(id - 1000);
            return;
        }
        match id {
            START => {
                if let Err(error) = self.start() {
                    self.message("Cannot start merge", &format!("{error:#}"), MB_ICONWARNING);
                }
            }
            PAUSE => {
                if let Some(job) = &mut self.job {
                    let paused = !job.paused;
                    if job.control.set_paused(paused) {
                        job.paused = paused;
                        self.set_text(PAUSE, if paused { "Resume" } else { "Pause" });
                    }
                }
            }
            CANCEL => {
                if let Some(job) = &self.job
                    && job.control.cancel()
                {
                    self.set_text(STATUS, "Cancelling — cleaning up temporary files");
                    unsafe {
                        EnableWindow(self.controls[&CANCEL], 0);
                        EnableWindow(self.controls[&PAUSE], 0);
                    }
                }
            }
            _ => {}
        }
    }
    fn close(&mut self) {
        if let Some(job) = &self.job {
            let answer = unsafe {
                MessageBoxW(
                    self.window,
                    wide("Cancel the active merge and close TextMerge?").as_ptr(),
                    wide("Merge in progress").as_ptr(),
                    MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
                )
            };
            if answer == IDYES {
                self.closing = true;
                job.control.cancel();
                self.set_text(STATUS, "Waiting for the worker to finish safely...");
            }
        } else {
            self.save();
            unsafe {
                DestroyWindow(self.window);
            }
        }
    }
}

/// This callback borrows App only for the duration of one dispatched message.
/// Native modal dialogs may re-enter the window procedure, so timer and command
/// messages are ignored while a handler owns the mutable borrow.
// Rust 1.99 GNU emits this lint from the thread_local macro expansion even
// with the explicit const initializer below.
#[allow(clippy::missing_const_for_thread_local)]
unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    thread_local! {
        static HANDLING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if message == WM_DESTROY {
        unsafe {
            PostQuitMessage(0);
        }
        return 0;
    }
    let pointer = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut App;
    if !pointer.is_null() {
        let handled = HANDLING.with(|busy| {
            if busy.replace(true) {
                return if matches!(
                    message,
                    WM_COMMAND | WM_TIMER | WM_CLOSE | WM_SIZE | WM_DPICHANGED
                ) {
                    Some(0)
                } else {
                    None
                };
            }
            struct Reset<'a>(&'a std::cell::Cell<bool>);
            impl Drop for Reset<'_> {
                fn drop(&mut self) {
                    self.0.set(false);
                }
            }
            let _reset = Reset(busy);
            let app = unsafe { &mut *pointer };
            match message {
                WM_COMMAND => {
                    if wparam >> 16 == BN_CLICKED as usize {
                        app.command((wparam & 0xffff) as i32);
                    }
                    Some(0)
                }
                WM_TIMER => {
                    app.tick();
                    Some(0)
                }
                WM_CLOSE => {
                    app.close();
                    Some(0)
                }
                WM_SIZE => {
                    if wparam != SIZE_MINIMIZED as usize && !app.controls.is_empty() {
                        app.layout();
                    }
                    Some(0)
                }
                WM_GETMINMAXINFO => {
                    let info = unsafe { &mut *(lparam as *mut MINMAXINFO) };
                    info.ptMinTrackSize.x = (820.0 * app.scale) as i32;
                    info.ptMinTrackSize.y = (800.0 * app.scale) as i32;
                    Some(0)
                }
                WM_DPICHANGED => {
                    app.scale = (wparam & 0xffff) as f64 / 96.0;
                    app.rebuild_fonts();
                    let rect = unsafe { &*(lparam as *const RECT) };
                    unsafe {
                        SetWindowPos(
                            window,
                            null_mut(),
                            rect.left,
                            rect.top,
                            rect.right - rect.left,
                            rect.bottom - rect.top,
                            SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                    }
                    app.layout();
                    Some(0)
                }
                _ => None,
            }
        });
        if let Some(result) = handled {
            return result;
        }
    }
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

pub fn show_startup_error(text: &str) {
    unsafe {
        MessageBoxW(
            null_mut(),
            wide(text).as_ptr(),
            wide("TextMerge").as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

pub fn run() -> Result<()> {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let com = CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32);
        ensure!(com >= 0, "Cannot initialize Windows COM: {com}");
        let common = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_PROGRESS_CLASS | ICC_STANDARD_CLASSES,
        };
        InitCommonControlsEx(&common);
        let instance = GetModuleHandleW(null());
        let name = wide("TextMerge.MainWindow");
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            // MAKEINTRESOURCE uses the numeric resource ID as a pointer value;
            // it is an identifier and is never dereferenced by Rust.
            hIcon: LoadIconW(instance, std::ptr::without_provenance(1)),
            hIconSm: LoadIconW(instance, std::ptr::without_provenance(1)),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            lpszClassName: name.as_ptr(),
            ..zeroed()
        };
        ensure!(RegisterClassExW(&class) != 0, "Cannot register main window");
        let settings = Settings::load().unwrap_or_else(|e| {
            logging::error(&format!("Reading registry settings: {e:#}"));
            Settings::default()
        });
        let dpi = GetDpiForSystem() as f64 / 96.0;
        let window = CreateWindowExW(
            0,
            name.as_ptr(),
            wide("TextMerge").as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            (900.0 * dpi) as i32,
            (820.0 * dpi) as i32,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        ensure!(!window.is_null(), "Cannot create main window");
        let mut app = Box::new(App {
            window,
            controls: HashMap::new(),
            fonts: Vec::new(),
            settings,
            job: None,
            closing: false,
            scale: GetDpiForWindow(window) as f64 / 96.0,
        });
        app.create_controls()?;
        SetWindowLongPtrW(window, GWLP_USERDATA, (&mut *app as *mut App) as isize);
        SetTimer(window, 1, 150, None);
        ShowWindow(window, SW_SHOW);
        let mut message: MSG = zeroed();
        loop {
            let result = GetMessageW(&mut message, null_mut(), 0, 0);
            if result == 0 {
                break;
            }
            ensure!(result != -1, "Windows message loop failed");
            if IsDialogMessageW(window, &message) == 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        if let Some(job) = app.job.take() {
            job.control.cancel();
            let _ = job.thread.join();
        }
        for font in app.fonts.drain(..) {
            DeleteObject(font);
        }
        CoUninitialize();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exercise the application's own native controls in an off-screen test
    /// window. No existing application or user window is automated.
    #[test]
    fn resize_restore_preserves_control_layers_and_invalidates_children() {
        unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let common = INITCOMMONCONTROLSEX {
                dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_PROGRESS_CLASS | ICC_STANDARD_CLASSES,
            };
            assert_ne!(InitCommonControlsEx(&common), 0);
            let instance = GetModuleHandleW(null());
            let name = wide("TextMerge.LayoutRegressionTest");
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                hbrBackground: GetSysColorBrush(COLOR_WINDOW),
                lpszClassName: name.as_ptr(),
                ..zeroed()
            };
            assert_ne!(RegisterClassExW(&class), 0);
            let window = CreateWindowExW(
                0,
                name.as_ptr(),
                name.as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                -32000,
                -32000,
                900,
                820,
                null_mut(),
                null_mut(),
                instance,
                null(),
            );
            assert!(!window.is_null());
            let mut app = Box::new(App {
                window,
                controls: HashMap::new(),
                fonts: Vec::new(),
                settings: Settings::default(),
                job: None,
                closing: false,
                scale: GetDpiForWindow(window) as f64 / 96.0,
            });
            app.create_controls().unwrap();
            app.set_text(SOURCE_A, "C:\\Example\\source A.txt");
            app.set_text(SOURCE_B, "C:\\Example\\source B.txt");
            app.set_text(TARGET, "C:\\Example\\merged.txt");
            SendMessageW(app.controls[&CASE], CB_SETCURSEL, 1, 0);
            SendMessageW(app.controls[&ORDER], CB_SETCURSEL, 2, 0);
            SendMessageW(app.controls[&EOL], CB_SETCURSEL, 1, 0);
            SendMessageW(app.controls[&BAR], PBM_SETPOS, 623, 0);
            SetWindowLongPtrW(window, GWLP_USERDATA, (&mut *app as *mut App) as isize);
            // Child invalidation follows the WS_VISIBLE ancestor chain. Keep
            // that chain real while leaving the test outside the desktop area.
            ShowWindow(window, SW_SHOWNOACTIVATE);

            struct Cleanup(Box<App>);
            impl Drop for Cleanup {
                fn drop(&mut self) {
                    unsafe {
                        SetWindowLongPtrW(self.0.window, GWLP_USERDATA, 0);
                        DestroyWindow(self.0.window);
                        for font in self.0.fonts.drain(..) {
                            DeleteObject(font);
                        }
                    }
                }
            }
            let app = Cleanup(app);
            let settings_before = app.0.collect_settings();
            let mut restored_bounds = HashMap::new();
            for (&id, &control) in &app.0.controls {
                let mut bounds = RECT::default();
                assert_ne!(GetWindowRect(control, &mut bounds), 0);
                MapWindowPoints(null_mut(), window, (&mut bounds as *mut RECT).cast(), 2);
                restored_bounds.insert(id, bounds);
            }

            // Repeat enlarged/restored client sizes, including 125%/150%/200%
            // style widths. SetWindowPos delivers real WM_SIZE notifications.
            for (width, height) in [
                (1920, 1080),
                (900, 820),
                (2400, 1350),
                (900, 820),
                (2880, 1620),
                (900, 820),
            ] {
                for &control in app.0.controls.values() {
                    ValidateRect(control, null());
                }
                assert_ne!(
                    SetWindowPos(
                        window,
                        null_mut(),
                        0,
                        0,
                        width,
                        height,
                        SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE
                    ),
                    0
                );

                let mut order = Vec::new();
                let mut child = GetWindow(window, GW_CHILD);
                while !child.is_null() {
                    order.push(child);
                    child = GetWindow(child, GW_HWNDNEXT);
                }
                for group in [3, 4] {
                    let group_order = order
                        .iter()
                        .position(|&w| w == app.0.controls[&group])
                        .unwrap();
                    for (&id, &control) in &app.0.controls {
                        if id == 3 || id == 4 {
                            continue;
                        }
                        let control_order = order.iter().position(|&w| w == control).unwrap();
                        assert!(
                            control_order < group_order,
                            "Control {id} is covered by group box {group}"
                        );
                    }
                }
                for (&id, &control) in &app.0.controls {
                    assert_ne!(
                        GetUpdateRect(control, null_mut(), 0),
                        0,
                        "Control {id} was not invalidated after resize"
                    );
                    let mut bounds = RECT::default();
                    assert_ne!(GetWindowRect(control, &mut bounds), 0);
                    MapWindowPoints(null_mut(), window, (&mut bounds as *mut RECT).cast(), 2);
                    assert!(
                        bounds.right > bounds.left && bounds.bottom > bounds.top,
                        "Invalid bounds for {id}"
                    );
                    if width == 900 {
                        let expected = &restored_bounds[&id];
                        assert_eq!(
                            (bounds.left, bounds.top, bounds.right, bounds.bottom),
                            (expected.left, expected.top, expected.right, expected.bottom),
                            "Restored bounds changed for {id}"
                        );
                    }
                }
                let settings_after = app.0.collect_settings();
                assert_eq!(settings_after.source_a, settings_before.source_a);
                assert_eq!(settings_after.source_b, settings_before.source_b);
                assert_eq!(settings_after.target, settings_before.target);
                assert_eq!(
                    settings_after.temporary_directory,
                    settings_before.temporary_directory
                );
                assert_eq!(settings_after.order, settings_before.order);
                assert_eq!(
                    settings_after.case_sensitive,
                    settings_before.case_sensitive
                );
                assert_eq!(settings_after.windows_eol, settings_before.windows_eol);
                assert_eq!(app.0.text(START), "Merge files");
                assert_eq!(SendMessageW(app.0.controls[&BAR], PBM_GETPOS, 0, 0), 623);
            }
            // A DPI transition resizes the parent inside its message handler.
            // That nested WM_SIZE is intentionally guarded; the explicit final
            // layout must still resize every child and rebuild the fonts.
            let initial_dpi = (app.0.scale * 96.0).round() as u32;
            for dpi in [120, 144, 192, initial_dpi] {
                let factor = dpi as f64 / 96.0;
                let suggested = RECT {
                    left: -32000,
                    top: -32000,
                    right: -32000 + (900.0 * factor) as i32,
                    bottom: -32000 + (820.0 * factor) as i32,
                };
                SendMessageW(
                    window,
                    WM_DPICHANGED,
                    (dpi | dpi << 16) as usize,
                    (&suggested as *const RECT) as isize,
                );
                assert_eq!(app.0.scale, factor);
                let mut field = RECT::default();
                GetWindowRect(app.0.controls[&SOURCE_A], &mut field);
                MapWindowPoints(null_mut(), window, (&mut field as *mut RECT).cast(), 2);
                assert_eq!(field.left, (168.0 * factor).round() as i32);
                assert_eq!(field.top, (138.0 * factor).round() as i32);
                assert_eq!(app.0.text(SOURCE_A), settings_before.source_a);
                assert_eq!(SendMessageW(app.0.controls[&BAR], PBM_GETPOS, 0, 0), 623);
            }
        }
    }
}
