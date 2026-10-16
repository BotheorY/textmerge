// SPDX-License-Identifier: MIT
//! The index contains only (digest, length, disk offset). Hash matches are always
//! confirmed byte-for-byte. Neither a whole input nor an unbounded line is held
//! in memory. The final file is published only after a successful, synced write.
use anyhow::{Context, Result, bail, ensure};
use encoding_rs::{Decoder, DecoderResult, UTF_8, UTF_16BE, UTF_16LE};
use rusqlite::{Connection, params};
use std::os::windows::ffi::OsStrExt;
use std::{
    fs::{File, OpenOptions},
    io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};
use tempfile::{NamedTempFile, TempDir};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx, MOVEFILE_REPLACE_EXISTING,
    MOVEFILE_WRITE_THROUGH, MoveFileExW,
};

const CHUNK_SIZE: usize = 64 * 1024;
const MEMORY_LINE_LIMIT: usize = 256 * 1024;

#[derive(Clone, Debug)]
pub struct MergeOptions {
    pub source_a: PathBuf,
    pub source_b: PathBuf,
    pub target: PathBuf,
    pub temporary_directory: PathBuf,
    pub case_sensitive: bool,
    /// 0: B[i], A[i]; 1: A[i], B[i]; 2: all B, all A; 3: all A, all B.
    pub order: usize,
    pub windows_eol: bool,
    pub overwrite: bool,
}

#[derive(Default)]
struct ControlState {
    paused: bool,
    cancelled: bool,
    committing: bool,
}

/// Shared cancellation and pause gate. Waiting never blocks the GUI thread.
#[derive(Default)]
pub struct Control {
    state: Mutex<ControlState>,
    changed: Condvar,
}

#[derive(Debug)]
pub struct Cancelled;
impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Merge cancelled")
    }
}
impl std::error::Error for Cancelled {}

impl Control {
    pub fn set_paused(&self, paused: bool) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.committing || state.cancelled {
            return false;
        }
        state.paused = paused;
        self.changed.notify_all();
        true
    }
    pub fn cancel(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.committing {
            return false;
        }
        state.cancelled = true;
        self.changed.notify_all();
        true
    }
    pub fn checkpoint(&self) -> Result<()> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        while state.paused && !state.cancelled {
            state = self.changed.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        if state.cancelled {
            return Err(Cancelled.into());
        }
        Ok(())
    }
    fn begin_commit(&self) -> Result<()> {
        self.checkpoint()?;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.cancelled {
            return Err(Cancelled.into());
        }
        state.committing = true;
        Ok(())
    }
}

#[derive(Default)]
pub struct Progress {
    pub input_bytes: AtomicU64,
    pub total_bytes: AtomicU64,
    pub lines: AtomicU64,
    pub unique_lines: AtomicU64,
    pub output_bytes: AtomicU64,
    pub stage: Mutex<String>,
}
impl Progress {
    pub fn set_stage(&self, text: &str) {
        *self.stage.lock().unwrap_or_else(|e| e.into_inner()) = text.into();
    }
}

#[derive(Debug)]
pub struct MergeReport {
    pub lines: u64,
    pub unique_lines: u64,
    pub output_bytes: u64,
    pub seconds: f64,
}

/// A small line stays in RAM; unusually long lines spill into a private job folder.
struct LineData {
    memory: Vec<u8>,
    disk: Option<NamedTempFile>,
    length: u64,
}
impl LineData {
    fn new() -> Self {
        Self {
            memory: Vec::new(),
            disk: None,
            length: 0,
        }
    }
    fn append(&mut self, bytes: &[u8], directory: &Path) -> Result<()> {
        if self.disk.is_none() && self.memory.len().saturating_add(bytes.len()) > MEMORY_LINE_LIMIT
        {
            let mut disk = NamedTempFile::new_in(directory)?;
            disk.write_all(&self.memory)?;
            self.memory.clear();
            self.disk = Some(disk);
        }
        if let Some(disk) = &mut self.disk {
            disk.write_all(bytes)?;
        } else {
            self.memory.extend_from_slice(bytes);
        }
        self.length += bytes.len() as u64;
        Ok(())
    }
    fn reader(&self) -> Result<Box<dyn Read + '_>> {
        if let Some(disk) = &self.disk {
            let mut file = disk.reopen()?;
            file.seek(SeekFrom::Start(0))?;
            Ok(Box::new(
                BufReader::with_capacity(CHUNK_SIZE, file).take(self.length),
            ))
        } else {
            Ok(Box::new(io::Cursor::new(&self.memory)))
        }
    }
    fn remove_last_cr(&mut self) -> Result<()> {
        if self.length == 0 {
            return Ok(());
        }
        if let Some(disk) = &mut self.disk {
            let file = disk.as_file_mut();
            file.seek(SeekFrom::Start(self.length - 1))?;
            let mut last = [0];
            file.read_exact(&mut last)?;
            if last[0] == b'\r' {
                self.length -= 1;
                file.set_len(self.length)?;
            }
        } else if self.memory.last() == Some(&b'\r') {
            self.memory.pop();
            self.length -= 1;
        }
        Ok(())
    }
}

/// Strict incremental decoder. Invalid text is rejected instead of silently
/// replacing characters. No BOM means UTF-8; BOMs select UTF-8 or UTF-16.
struct TextReader {
    input: BufReader<File>,
    decoder: Decoder,
    output: Vec<u8>,
    position: usize,
    finished: bool,
    progress: Arc<Progress>,
    control: Arc<Control>,
}
impl TextReader {
    fn new(file: File, progress: Arc<Progress>, control: Arc<Control>) -> Result<Self> {
        let mut input = BufReader::with_capacity(CHUNK_SIZE, file);
        let header = input.fill_buf()?;
        ensure!(
            !header.starts_with(&[0xff, 0xfe, 0, 0]) && !header.starts_with(&[0, 0, 0xfe, 0xff]),
            "UTF-32 is not supported. Convert the source to UTF-8 first."
        );
        let (encoding, skip) = if header.starts_with(&[0xff, 0xfe]) {
            (UTF_16LE, 2)
        } else if header.starts_with(&[0xfe, 0xff]) {
            (UTF_16BE, 2)
        } else if header.starts_with(&[0xef, 0xbb, 0xbf]) {
            (UTF_8, 3)
        } else {
            (UTF_8, 0)
        };
        input.consume(skip);
        progress
            .input_bytes
            .fetch_add(skip as u64, Ordering::Relaxed);
        Ok(Self {
            input,
            decoder: encoding.new_decoder_without_bom_handling(),
            output: Vec::new(),
            position: 0,
            finished: false,
            progress,
            control,
        })
    }
    fn refill(&mut self) -> Result<bool> {
        if self.position < self.output.len() {
            return Ok(true);
        }
        if self.finished {
            return Ok(false);
        }
        self.control.checkpoint()?;
        self.output.resize(CHUNK_SIZE * 3 + 4, 0);
        let input = self.input.fill_buf()?;
        let last = input.is_empty();
        let (result, read, written) =
            self.decoder
                .decode_to_utf8_without_replacement(input, &mut self.output, last);
        self.input.consume(read);
        self.progress
            .input_bytes
            .fetch_add(read as u64, Ordering::Relaxed);
        self.position = 0;
        self.output.truncate(written);
        if matches!(result, DecoderResult::Malformed(_, _)) {
            bail!(
                "Invalid input encoding near byte {}. Use UTF-8, or UTF-16 with a BOM; legacy ANSI files must be converted first.",
                self.progress.input_bytes.load(Ordering::Relaxed)
            );
        }
        self.finished = last && matches!(result, DecoderResult::InputEmpty);
        if written == 0 {
            if self.finished {
                Ok(false)
            } else {
                self.refill()
            }
        } else {
            Ok(true)
        }
    }
    fn next_line(&mut self, directory: &Path) -> Result<Option<LineData>> {
        let mut line = LineData::new();
        loop {
            if !self.refill()? {
                return Ok((line.length != 0).then_some(line));
            }
            self.control.checkpoint()?;
            let bytes = &self.output[self.position..];
            if let Some(end) = bytes.iter().position(|&b| b == b'\n') {
                line.append(&bytes[..end], directory)?;
                self.position += end + 1;
                line.remove_last_cr()?;
                return Ok(Some(line));
            }
            line.append(bytes, directory)?;
            self.position = self.output.len();
        }
    }
}

/// Feed complete UTF-8 fragments to the normalizer, including when a read ends
/// inside a multi-byte character. At most three carry bytes are retained.
fn canonicalize(
    line: &LineData,
    sensitive: bool,
    directory: &Path,
    control: &Control,
) -> Result<(LineData, blake3::Hash)> {
    // Most lines are short: avoid allocating or clearing a 64 KiB streaming
    // buffer for every row in multi-million-line inputs.
    if line.disk.is_none() {
        control.checkpoint()?;
        let text = std::str::from_utf8(&line.memory)?;
        let bytes = if sensitive {
            line.memory.clone()
        } else {
            crate::casefold::fold(text).into_bytes()
        };
        let digest = blake3::hash(&bytes);
        let mut canonical = LineData::new();
        canonical.append(&bytes, directory)?;
        return Ok((canonical, digest));
    }
    let mut canonical = LineData::new();
    let mut hasher = blake3::Hasher::new();
    let mut reader = line.reader()?;
    let mut buffer = vec![0; CHUNK_SIZE + 4];
    let mut carry = 0;
    loop {
        control.checkpoint()?;
        let count = reader.read(&mut buffer[carry..CHUNK_SIZE])?;
        let available = carry + count;
        if available == 0 {
            break;
        }
        let valid = match std::str::from_utf8(&buffer[..available]) {
            Ok(_) => available,
            Err(error) if error.error_len().is_none() && count != 0 => error.valid_up_to(),
            Err(error) => return Err(error.into()),
        };
        let text = std::str::from_utf8(&buffer[..valid])?;
        if sensitive {
            hasher.update(text.as_bytes());
            canonical.append(text.as_bytes(), directory)?;
        } else {
            let folded = crate::casefold::fold(text);
            hasher.update(folded.as_bytes());
            canonical.append(folded.as_bytes(), directory)?;
        }
        carry = available - valid;
        buffer.copy_within(valid..available, 0);
    }
    Ok((canonical, hasher.finalize()))
}

fn copy_checked(mut reader: impl Read, writer: &mut impl Write, control: &Control) -> Result<u64> {
    let mut buffer = [0; CHUNK_SIZE];
    let mut total = 0;
    loop {
        control.checkpoint()?;
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(total);
        }
        writer.write_all(&buffer[..count])?;
        total += count as u64;
    }
}

fn copy_line(line: &LineData, writer: &mut impl Write, control: &Control) -> Result<u64> {
    control.checkpoint()?;
    if line.disk.is_none() {
        writer.write_all(&line.memory)?;
        Ok(line.length)
    } else {
        copy_checked(line.reader()?, writer, control)
    }
}

fn exact_match(
    candidate: &LineData,
    store: &mut File,
    offset: u64,
    control: &Control,
) -> Result<bool> {
    store.seek(SeekFrom::Start(offset))?;
    let mut reader = candidate.reader()?;
    let mut first = [0; CHUNK_SIZE];
    let mut second = [0; CHUNK_SIZE];
    let mut remaining = candidate.length;
    while remaining != 0 {
        control.checkpoint()?;
        let count = remaining.min(CHUNK_SIZE as u64) as usize;
        reader.read_exact(&mut first[..count])?;
        store.read_exact(&mut second[..count])?;
        if first[..count] != second[..count] {
            return Ok(false);
        }
        remaining -= count as u64;
    }
    Ok(true)
}

struct Merger<'a> {
    connection: Connection,
    store: BufWriter<File>,
    store_reader: File,
    store_offset: u64,
    output: BufWriter<File>,
    directory: &'a Path,
    options: &'a MergeOptions,
    control: &'a Control,
    progress: &'a Progress,
}
impl Merger<'_> {
    fn line(&mut self, line: LineData) -> Result<()> {
        self.control.checkpoint()?;
        let (canonical, digest) = canonicalize(
            &line,
            self.options.case_sensitive,
            self.directory,
            self.control,
        )?;
        let mut query = self
            .connection
            .prepare_cached("SELECT position FROM seen WHERE digest=?1 AND length=?2")?;
        let mut rows = query.query(params![digest.as_bytes().as_slice(), canonical.length])?;
        let mut duplicate = false;
        while let Some(row) = rows.next()? {
            self.store.flush()?;
            if exact_match(
                &canonical,
                &mut self.store_reader,
                row.get(0)?,
                self.control,
            )? {
                duplicate = true;
                break;
            }
        }
        drop(rows);
        drop(query);
        self.progress.lines.fetch_add(1, Ordering::Relaxed);
        if duplicate {
            return Ok(());
        }
        copy_line(&canonical, &mut self.store, self.control)?;
        self.connection
            .prepare_cached("INSERT INTO seen(digest,length,position) VALUES(?1,?2,?3)")?
            .execute(params![
                digest.as_bytes().as_slice(),
                canonical.length,
                self.store_offset
            ])?;
        self.store_offset += canonical.length;
        let written = copy_line(&line, &mut self.output, self.control)?;
        let ending: &[u8] = if self.options.windows_eol {
            b"\r\n"
        } else {
            b"\n"
        };
        self.output.write_all(ending)?;
        self.progress.unique_lines.fetch_add(1, Ordering::Relaxed);
        self.progress
            .output_bytes
            .fetch_add(written + ending.len() as u64, Ordering::Relaxed);
        Ok(())
    }
}

fn input_file(path: &Path) -> Result<File> {
    // Read sharing prevents another process from changing either source mid-merge.
    let file = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(path)
        .with_context(|| format!("Cannot open source: {}", path.display()))?;
    ensure!(
        file.metadata()?.is_file(),
        "Source must be a regular file: {}",
        path.display()
    );
    Ok(file)
}

fn same_file(first: &File, second: &File) -> Result<bool> {
    fn identity(file: &File) -> Result<FILE_ID_INFO> {
        let mut identity: FILE_ID_INFO = unsafe { std::mem::zeroed() };
        // SAFETY: The live file handle and writable structure have the size
        // required by FileIdInfo. Failure is propagated, never guessed away.
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileIdInfo,
                (&mut identity as *mut FILE_ID_INFO).cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error()).context("Cannot verify target file identity");
        }
        Ok(identity)
    }
    if let (Ok(a), Ok(b)) = (identity(first), identity(second)) {
        return Ok(a.VolumeSerialNumber == b.VolumeSerialNumber
            && a.FileId.Identifier == b.FileId.Identifier);
    }
    // FAT/exFAT and some remote filesystems do not expose the modern 128-bit
    // identity class. The classic API supplies a volume ID and 64-bit file ID.
    fn classic_identity(file: &File) -> Result<(u32, u32, u32)> {
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let mut information: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0 {
            return Err(io::Error::last_os_error()).context("Cannot verify target file identity");
        }
        Ok((
            information.dwVolumeSerialNumber,
            information.nFileIndexHigh,
            information.nFileIndexLow,
        ))
    }
    Ok(classic_identity(first)? == classic_identity(second)?)
}

pub fn merge(
    options: &MergeOptions,
    control: Arc<Control>,
    progress: Arc<Progress>,
) -> Result<MergeReport> {
    let started = Instant::now();
    ensure!(options.order < 4, "Invalid merge order");
    let first = input_file(&options.source_a)?;
    let second = input_file(&options.source_b)?;
    progress.total_bytes.store(
        first
            .metadata()?
            .len()
            .checked_add(second.metadata()?.len())
            .context("Input size overflow")?,
        Ordering::Relaxed,
    );
    if options.target.exists() {
        let target = File::open(&options.target)?;
        ensure!(
            !same_file(&first, &target)? && !same_file(&second, &target)?,
            "The target must not be a source file, hard link, or alias of a source."
        );
        ensure!(
            options.overwrite,
            "The target already exists. Confirm replacement before merging."
        );
    }
    let parent = options
        .target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    ensure!(parent.is_dir(), "The target directory does not exist");
    std::fs::create_dir_all(&options.temporary_directory)
        .context("Cannot create temporary directory")?;
    let job: TempDir = tempfile::Builder::new()
        .prefix("textmerge-")
        .tempdir_in(&options.temporary_directory)?;
    // The staging file is beside the destination, so publication is a same-volume
    // atomic rename. Cancellation and failures leave any existing target intact.
    let staging = tempfile::Builder::new()
        .prefix(".textmerge-")
        .suffix(".partial")
        .tempfile_in(parent)?;
    let staging_path = staging.into_temp_path();
    let output = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&staging_path)?;
    let store_path = job.path().join("keys.dat");
    let store = File::create(&store_path)?;
    let connection = Connection::open(job.path().join("index.sqlite"))?;
    // A covering WITHOUT ROWID primary key avoids storing every digest twice
    // and avoids an extra table lookup on each candidate collision.
    connection.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA cache_size=-65536; PRAGMA mmap_size=0; PRAGMA temp_store=FILE;
        CREATE TABLE seen(digest BLOB NOT NULL,length INTEGER NOT NULL,position INTEGER NOT NULL,
        PRIMARY KEY(digest,length,position)) WITHOUT ROWID; BEGIN IMMEDIATE;")?;
    let mut merger = Merger {
        connection,
        store: BufWriter::with_capacity(1024 * 1024, store),
        store_reader: File::open(&store_path)?,
        store_offset: 0,
        output: BufWriter::with_capacity(1024 * 1024, output),
        directory: job.path(),
        options,
        control: &control,
        progress: &progress,
    };
    let mut a = TextReader::new(first, progress.clone(), control.clone())
        .context("Cannot decode source A")?;
    let mut b = TextReader::new(second, progress.clone(), control.clone())
        .context("Cannot decode source B")?;
    progress.set_stage("Merging and verifying unique lines");
    if options.order < 2 {
        let mut a_done = false;
        let mut b_done = false;
        while !a_done || !b_done {
            for source in if options.order == 0 { [1, 0] } else { [0, 1] } {
                let (reader, done, label) = if source == 0 {
                    (&mut a, &mut a_done, "A")
                } else {
                    (&mut b, &mut b_done, "B")
                };
                if !*done {
                    match reader
                        .next_line(job.path())
                        .with_context(|| format!("Reading source {label}"))?
                    {
                        Some(line) => merger.line(line)?,
                        None => *done = true,
                    }
                }
            }
        }
    } else {
        let readers = if options.order == 2 {
            [&mut b, &mut a]
        } else {
            [&mut a, &mut b]
        };
        for reader in readers {
            while let Some(line) = reader.next_line(job.path())? {
                merger.line(line)?;
            }
        }
    }
    control.checkpoint()?;
    progress.set_stage("Flushing and syncing output");
    merger.output.flush()?;
    merger.output.get_ref().sync_all()?;
    // Close handles before renaming on Windows; the temporary index is disposable.
    drop(merger);
    drop(a);
    drop(b);
    control.begin_commit()?;
    progress.set_stage("Publishing completed target");
    let from: Vec<u16> = staging_path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let to: Vec<u16> = options
        .target
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let flags = MOVEFILE_WRITE_THROUGH
        | if options.overwrite {
            MOVEFILE_REPLACE_EXISTING
        } else {
            0
        };
    // SAFETY: Both UTF-16 path buffers are NUL-terminated and live through the call.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), flags) } == 0 {
        return Err(io::Error::last_os_error())
            .context("Cannot publish target; the existing target was preserved");
    }
    let _ = staging_path.close();
    if let Err(error) = job.close() {
        crate::logging::error(&format!("Temporary folder cleanup: {error}"));
    }
    progress.set_stage("Complete");
    Ok(MergeReport {
        lines: progress.lines.load(Ordering::Relaxed),
        unique_lines: progress.unique_lines.load(Ordering::Relaxed),
        output_bytes: progress.output_bytes.load(Ordering::Relaxed),
        seconds: started.elapsed().as_secs_f64(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_digest_collision_never_discards_distinct_text() {
        let directory = tempfile::tempdir().unwrap();
        let options = MergeOptions {
            source_a: PathBuf::new(),
            source_b: PathBuf::new(),
            target: PathBuf::new(),
            temporary_directory: directory.path().into(),
            case_sensitive: true,
            order: 1,
            windows_eol: false,
            overwrite: false,
        };
        let store_path = directory.path().join("keys");
        std::fs::write(&store_path, b"other").unwrap();
        let mut store = OpenOptions::new().append(true).open(&store_path).unwrap();
        store.flush().unwrap();
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE seen(digest BLOB,length INTEGER,position INTEGER)")
            .unwrap();
        // Deliberately plant a different line under the digest for 'hello'.
        let digest = blake3::hash(b"hello");
        connection
            .execute(
                "INSERT INTO seen VALUES(?1,5,0)",
                [digest.as_bytes().as_slice()],
            )
            .unwrap();
        let control = Control::default();
        let progress = Progress::default();
        let output_path = directory.path().join("output");
        let mut merger = Merger {
            connection,
            store: BufWriter::new(store),
            store_reader: File::open(&store_path).unwrap(),
            store_offset: 5,
            output: BufWriter::new(File::create(&output_path).unwrap()),
            directory: directory.path(),
            options: &options,
            control: &control,
            progress: &progress,
        };
        for _ in 0..2 {
            let mut line = LineData::new();
            line.append(b"hello", directory.path()).unwrap();
            merger.line(line).unwrap();
        }
        merger.output.flush().unwrap();
        assert_eq!(std::fs::read(output_path).unwrap(), b"hello\n");
        assert_eq!(progress.unique_lines.load(Ordering::Relaxed), 1);
        assert_eq!(
            merger
                .connection
                .query_row("SELECT count(*) FROM seen", [], |r| r.get::<_, u64>(0))
                .unwrap(),
            2
        );
    }
}
