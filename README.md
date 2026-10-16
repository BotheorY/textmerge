# TextMerge 1.0

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

TextMerge is a portable Windows x64 application that merges two text files
into a UTF-8 target, retaining each distinct line exactly once. Run
`TextMerge.exe`; no installer, administrator privileges, Rust, or editor is
needed on the destination PC. Copy the executable to a USB drive to use it
elsewhere. Include the license and third-party notices when redistributing.

## Using the application

All controls are in one window. Browse for sources A and B, select a target
name and directory with **Save as...**, choose the settings, then click
**Merge files**. Paths can also be typed. Replacement of an existing target
requires confirmation. The target cannot be either source or a hard link to it.

Settings and the last paths are saved on normal close under
`HKEY_CURRENT_USER\Software\TextMerge` and restored on the next start. Defaults:

| Setting | Default | Alternatives |
| --- | --- | --- |
| Duplicate detection | Case sensitive | Case insensitive |
| Merge order | B lines below A lines | B lines above A lines, B before A, B after A |
| Target line endings | Windows (CRLF) | Unix (LF) |
| Temporary folder | `%TEMP%\TextMerge` | Any writable folder with sufficient space |

**Pause** stops the worker at its next bounded I/O checkpoint. **Resume**
continues it. **Cancel** stops the job and removes its private temporary files.
The window stays responsive. On close during a job, confirm cancellation;
TextMerge waits for safe cleanup before exiting. The short final publication
step cannot be paused or cancelled once it begins. Windows disk operations
already in progress must return before cancellation can be honored.

## Exact merge semantics

- The first occurrence in the selected traversal order wins; its spelling and
  whitespace are preserved. Duplicates within the same source are also removed.
- Above: `B1, A1, B2, A2, ...`; below: `A1, B1, A2, B2, ...`. Positions always
  refer to original source lines, before duplicate removal. Remaining lines
  from the longer source follow in their original order.
- Before: all B, then all A. After: all A, then all B.
- Case-insensitive matching uses Unicode 17 full, non-Turkic case folding.
  It does not trim whitespace, normalize accents, or change output spelling.
- Input: strict UTF-8 (with or without BOM), UTF-16 LE/BE with BOM. Malformed
  input is rejected, never silently replaced. Convert legacy ANSI or UTF-32
  files to UTF-8 first.
- LF and CRLF delimit lines; a lone CR is content. A final unterminated line
  is accepted. Every retained output line ends with the selected terminator.
  Empty input contributes no lines; repeated empty lines become one empty line.
- Output is always UTF-8 without BOM. NUL characters in otherwise valid text
  are preserved.

## Large files, disk use, and reliability

Two buffered streaming decoders feed a SQLite disk index. Index keys contain
BLAKE3 digests, canonical lengths, and 64-bit offsets. Every hash match is
verified against canonical bytes on disk, so hash collisions cannot discard
distinct lines. The SQLite cache is capped at 64 MiB, memory mapping is off,
and lines above 256 KiB spill to disk. Even a single line larger than RAM is
processed in bounded chunks. Memory depends on fixed buffers and the index
cache, not input size or unique-line count.

The temporary folder holds a private index, canonical unique text, and any
large current lines. Allow space for canonical unique text, index overhead
(typically around 100 bytes per unique line), and up to two copies of the
largest active line; folding may expand text. The target directory separately
needs enough free space for the complete new output, in addition to an existing
target. A fast local SSD is recommended for the temporary folder. Removable
drives must support the file sizes involved; FAT32 cannot store a file >=4 GiB.

The completed output is flushed and synced, then renamed over the target on
the same volume. Ordinary failures and cancellation preserve any prior target.
Sources are opened without write/delete sharing to avoid changes during a job.
Temporary SQLite journaling is disabled deliberately: the index is disposable
and never reused after an interrupted job. No recovery of partial jobs is
attempted. Power loss, forced termination, device removal, filesystem faults,
or network filesystems can leave private `textmerge-*` folders or
`.textmerge-*.partial` files; after ensuring no job is running, those generated
artifacts can be removed. Never disconnect a drive during a merge.

Errors are logged to `logs\TextMerge-errors.log` beside the executable, with
one rotated previous log. If that location is unwritable, the fallback is
`%LOCALAPPDATA%\TextMerge\logs`. Log entries use UTC Unix seconds. If both
locations fail, the GUI still displays the error. The executable has an
embedded multi-resolution icon, version information, a per-monitor DPI v2
manifest, and native accessible controls. The build is unsigned; no signing
certificate is included.

## Source map

`engine.rs`: streaming decoder, spill buffers, exact deduplication, safe publish.
`ui.rs`: native GUI and background-job lifecycle. `settings.rs`: registry.
`logging.rs`: bounded error logging. `tests/merging.rs`: reference, encoding,
large-line, cancellation, overwrite, hard-link and multi-gigabyte tests.
`tools/create-icon.ps1`: deterministic original icon source, using vector shapes.

## License

TextMerge is licensed under the [MIT License](LICENSE).
Copyright (c) 2026 TextMerge contributors.

The MIT license covers the original application source, documentation, build
scripts, and original application icon. The complete, unmodified MIT text is
in the repository-root `LICENSE` file so GitHub can identify the repository
license. `Cargo.toml` declares the SPDX license identifier `MIT`; original
source files also carry SPDX identifiers.

Third-party dependencies and Unicode case-folding data retain their original
licenses. Unicode data terms are in [assets/UNICODE-LICENSE.txt](assets/UNICODE-LICENSE.txt).
Redistribution notices and license texts are included in the portable package's
`THIRD-PARTY-NOTICES.md` and `ThirdPartyLicenses` directory. Keep those notices
and the MIT `LICENSE` file when distributing TextMerge.
