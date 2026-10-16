// SPDX-License-Identifier: MIT
use std::{
    fs,
    io::{Read, Write},
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use textmerge::engine::{Cancelled, Control, MergeOptions, Progress, merge};

fn options(directory: &std::path::Path) -> MergeOptions {
    MergeOptions {
        source_a: directory.join("a.txt"),
        source_b: directory.join("b.txt"),
        target: directory.join("result.txt"),
        temporary_directory: directory.join("scratch"),
        case_sensitive: true,
        order: 1,
        windows_eol: false,
        overwrite: false,
    }
}
fn execute(a: &[u8], b: &[u8], order: usize, sensitive: bool, windows: bool) -> Vec<u8> {
    let directory = tempfile::tempdir().unwrap();
    let mut opt = options(directory.path());
    fs::write(&opt.source_a, a).unwrap();
    fs::write(&opt.source_b, b).unwrap();
    opt.order = order;
    opt.case_sensitive = sensitive;
    opt.windows_eol = windows;
    merge(&opt, Arc::default(), Arc::default()).unwrap();
    fs::read(&opt.target).unwrap()
}

#[test]
fn all_orders_and_unequal_lengths() {
    let a = b"A1\nshared\nA3\nA4";
    let b = b"B1\nshared\nB3";
    for (order, expected) in [
        "B1\nA1\nshared\nB3\nA3\nA4\n",
        "A1\nB1\nshared\nA3\nB3\nA4\n",
        "B1\nshared\nB3\nA1\nA3\nA4\n",
        "A1\nshared\nA3\nA4\nB1\nB3\n",
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(execute(a, b, order, true, false), expected.as_bytes());
    }
}
#[test]
fn unicode_full_casefold_and_original_spelling() {
    assert_eq!(
        execute("Ა\n".as_bytes(), "ა\n".as_bytes(), 3, false, false),
        "Ა\n".as_bytes()
    );
    assert_eq!(
        execute(
            "Straße\nΣ\nİ\n A \n".as_bytes(),
            "STRASSE\nς\ni\nA\n".as_bytes(),
            3,
            false,
            false
        ),
        "Straße\nΣ\nİ\n A \ni\nA\n".as_bytes()
    );
    assert_eq!(
        execute(b"Hello\n", b"hello\n", 3, true, false),
        b"Hello\nhello\n"
    );
}
#[test]
fn empty_files_blank_lines_and_endings() {
    assert_eq!(execute(b"", b"", 1, true, false), b"");
    assert_eq!(execute(b"\n\n", b"\r\n", 1, true, true), b"\r\n");
    assert_eq!(execute(b"a\r\nb", b"b\na\n", 1, true, true), b"a\r\nb\r\n");
    assert_eq!(
        execute(b"last\r", b"last", 3, true, false),
        b"last\r\nlast\n"
    );
}
#[test]
fn bom_and_utf16_both_byte_orders() {
    let text = "Hello\n世界\n🙂";
    let le: Vec<u8> = [0xff, 0xfe]
        .into_iter()
        .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
        .collect();
    let be: Vec<u8> = [0xfe, 0xff]
        .into_iter()
        .chain(text.encode_utf16().flat_map(u16::to_be_bytes))
        .collect();
    assert_eq!(
        execute(&le, &be, 1, true, false),
        format!("{text}\n").as_bytes()
    );
    assert_eq!(
        execute(b"\xef\xbb\xbfHello", b"Hello", 1, true, false),
        b"Hello\n"
    );
}
#[test]
fn long_lines_spill_and_unicode_crosses_chunks() {
    let mut text = "x".repeat(65535);
    text.push('🙂');
    text.push_str(&"Straße".repeat(80000));
    let result = execute(
        text.as_bytes(),
        text.to_uppercase().as_bytes(),
        1,
        false,
        false,
    );
    assert_eq!(result, format!("{text}\n").as_bytes());
}
#[test]
fn invalid_input_preserves_target_and_cleans_up() {
    for input in [
        vec![0xff, b'\n'],
        vec![0xff, 0xfe, 0, 0xd8],
        vec![0xef, 0xbb, 0xbf, 0xc2],
        vec![0xff, 0xfe, 0, 0],
    ] {
        let directory = tempfile::tempdir().unwrap();
        let mut opt = options(directory.path());
        opt.overwrite = true;
        fs::write(&opt.source_a, &input).unwrap();
        fs::write(&opt.source_b, b"valid").unwrap();
        fs::write(&opt.target, b"original").unwrap();
        assert!(merge(&opt, Arc::default(), Arc::default()).is_err());
        assert_eq!(fs::read(&opt.target).unwrap(), b"original");
        assert_eq!(fs::read_dir(&opt.temporary_directory).unwrap().count(), 0);
        assert!(!fs::read_dir(directory.path()).unwrap().any(|p| {
            p.unwrap()
                .path()
                .extension()
                .is_some_and(|e| e == "partial")
        }));
    }
}
#[test]
fn sources_and_hard_links_cannot_be_targets() {
    let directory = tempfile::tempdir().unwrap();
    let mut opt = options(directory.path());
    fs::write(&opt.source_a, b"original").unwrap();
    fs::write(&opt.source_b, b"new").unwrap();
    fs::hard_link(&opt.source_a, &opt.target).unwrap();
    opt.overwrite = true;
    assert!(merge(&opt, Arc::default(), Arc::default()).is_err());
    assert_eq!(fs::read(&opt.source_a).unwrap(), b"original");
}
#[test]
fn overwrite_requires_explicit_authorization() {
    let directory = tempfile::tempdir().unwrap();
    let mut opt = options(directory.path());
    fs::write(&opt.source_a, b"a").unwrap();
    fs::write(&opt.source_b, b"b").unwrap();
    fs::write(&opt.target, b"original").unwrap();
    assert!(merge(&opt, Arc::default(), Arc::default()).is_err());
    assert_eq!(fs::read(&opt.target).unwrap(), b"original");
    opt.overwrite = true;
    merge(&opt, Arc::default(), Arc::default()).unwrap();
    assert_eq!(fs::read(&opt.target).unwrap(), b"a\nb\n");
}
#[test]
fn pause_resume_and_cancel_preserve_target() {
    let directory = tempfile::tempdir().unwrap();
    let mut opt = options(directory.path());
    opt.overwrite = true;
    fs::write(&opt.source_a, b"a\nb\nc").unwrap();
    fs::write(&opt.source_b, b"d").unwrap();
    fs::write(&opt.target, b"original").unwrap();
    let control = Arc::new(Control::default());
    control.set_paused(true);
    let progress = Arc::new(Progress::default());
    let c = control.clone();
    let p = progress.clone();
    let worker = std::thread::spawn(move || merge(&opt, c, p));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(progress.lines.load(Ordering::Relaxed), 0);
    control.cancel();
    let error = worker.join().unwrap().unwrap_err();
    assert!(error.is::<Cancelled>());
    assert_eq!(
        fs::read(directory.path().join("result.txt")).unwrap(),
        b"original"
    );
    let control = Arc::new(Control::default());
    control.set_paused(true);
    let c = control.clone();
    let opt = options(directory.path());
    let worker = std::thread::spawn(move || {
        let mut opt = opt;
        opt.overwrite = true;
        merge(&opt, c, Arc::default())
    });
    std::thread::sleep(Duration::from_millis(50));
    control.set_paused(false);
    assert!(worker.join().unwrap().is_ok());
}
#[test]
fn deterministic_randomized_reference() {
    use std::collections::HashSet;
    let mut seed = 17u64;
    for order in 0..4 {
        let mut a = Vec::new();
        let mut b = Vec::new();
        for source in [&mut a, &mut b] {
            for _ in 0..300 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                source.push(format!("line-{}", seed % 97));
            }
        }
        let mut ordered = Vec::new();
        if order < 2 {
            for i in 0..a.len() {
                if order == 0 {
                    ordered.extend([&b[i], &a[i]])
                } else {
                    ordered.extend([&a[i], &b[i]])
                }
            }
        } else if order == 2 {
            ordered.extend(b.iter());
            ordered.extend(a.iter());
        } else {
            ordered.extend(a.iter());
            ordered.extend(b.iter());
        }
        let mut seen = HashSet::new();
        let expected: String = ordered
            .into_iter()
            .filter(|line| seen.insert((*line).clone()))
            .map(|line| format!("{line}\n"))
            .collect();
        assert_eq!(
            execute(
                a.join("\n").as_bytes(),
                b.join("\n").as_bytes(),
                order,
                true,
                false
            ),
            expected.as_bytes()
        );
    }
}

/// Generates two >2 GiB single-line sources, verifies exact output by streaming,
/// and prints a machine-readable report. Run explicitly; ordinary tests stay fast.
#[test]
#[ignore = "writes and processes more than 4 GiB of input"]
fn multi_gigabyte_single_line_stress() {
    let directory = tempfile::tempdir().unwrap();
    let opt = options(directory.path());
    let block = vec![b'x'; 1024 * 1024];
    let blocks = 2049u64;
    for path in [&opt.source_a, &opt.source_b] {
        let mut file = std::io::BufWriter::new(fs::File::create(path).unwrap());
        for _ in 0..blocks {
            file.write_all(&block).unwrap();
        }
        file.write_all(b"\n").unwrap();
        file.flush().unwrap();
    }
    let report = merge(&opt, Arc::default(), Arc::default()).unwrap();
    assert_eq!(report.unique_lines, 1);
    assert_eq!(report.lines, 2);
    let mut file = fs::File::open(&opt.target).unwrap();
    let mut buffer = vec![0; block.len()];
    for _ in 0..blocks {
        file.read_exact(&mut buffer).unwrap();
        assert_eq!(buffer, block);
    }
    let mut ending = Vec::new();
    file.read_to_end(&mut ending).unwrap();
    assert_eq!(ending, b"\n");
    println!("STRESS_REPORT {report:?}");
}

/// Exercise a large on-disk index with mixed overlap and many short lines.
#[test]
#[ignore = "benchmarks two million input lines"]
fn million_line_index_stress() {
    let directory = tempfile::tempdir().unwrap();
    let opt = options(directory.path());
    for (path, start) in [(&opt.source_a, 0), (&opt.source_b, 500_000)] {
        let mut file = std::io::BufWriter::new(fs::File::create(path).unwrap());
        for number in start..start + 1_000_000 {
            writeln!(file, "line-{number:09}").unwrap();
        }
        file.flush().unwrap();
    }
    let report = merge(&opt, Arc::default(), Arc::default()).unwrap();
    assert_eq!(report.lines, 2_000_000);
    assert_eq!(report.unique_lines, 1_500_000);
    let output = std::io::BufReader::new(fs::File::open(&opt.target).unwrap());
    use std::io::BufRead;
    let mut lines = output.lines();
    for number in 0..500_000 {
        assert_eq!(lines.next().unwrap().unwrap(), format!("line-{number:09}"));
        assert_eq!(
            lines.next().unwrap().unwrap(),
            format!("line-{:09}", number + 500_000)
        );
    }
    for number in 1_000_000..1_500_000 {
        assert_eq!(lines.next().unwrap().unwrap(), format!("line-{number:09}"));
    }
    assert!(lines.next().is_none());
    println!("INDEX_REPORT {report:?}");
}
