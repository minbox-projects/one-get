//! Large-file offset coverage for sparse part files and planner arithmetic.
//!
//! Covers REQ-017/AC-030: a 5 GiB logical part file keeps head, middle, and
//! tail writes at their absolute u64 offsets without 32-bit truncation, and
//! segment plus Content-Range arithmetic stays exact above 4 GiB. The part
//! file is sparse, so only a few small ranges are written; no test downloads
//! or hashes the full logical length.

use oneget_lib::engine::http::{format_content_range, parse_content_range_value};
use oneget_lib::engine::part_file::PartFile;
use oneget_lib::engine::planner::{plan_segments, SegmentPlan};
use oneget_lib::platform::files::{CapDir, SparsePreallocation};
use tempfile::tempdir;

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
const FIVE_GIB: u64 = 5 * GIB;
const TEN_GIB: u64 = 10 * GIB;
const FOUR_GIB_BOUNDARY: u64 = 4_294_967_296;
const FOUR_GIB_BOUNDARY_MINUS_ONE: u64 = 4_294_967_295;
const PART_NAME: &str = "large.bin.oneget.part";

#[test]
fn five_gib_part_file_keeps_head_middle_and_tail_at_absolute_offsets() {
    let root = tempdir().expect("tempdir");
    let cap = CapDir::open(root.path()).expect("open capdir");

    let part = PartFile::create(&cap, "large.bin", Some(FIVE_GIB)).expect("create part file");
    assert_eq!(
        part.len(),
        FIVE_GIB,
        "part file length must be exactly 5 GiB after preallocation"
    );

    let outcome = part.preallocation_outcome();
    #[cfg(target_os = "macos")]
    assert!(
        matches!(outcome, SparsePreallocation::Sparse),
        "macOS set_len must report sparse preallocation"
    );
    #[cfg(windows)]
    match outcome {
        SparsePreallocation::Sparse => {}
        SparsePreallocation::Fallback => {
            eprintln!(
                "compatibility result: sparse preallocation unavailable, explicit fallback recorded"
            );
        }
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    assert!(
        matches!(
            outcome,
            SparsePreallocation::Sparse | SparsePreallocation::Fallback
        ),
        "preallocation must report an explicit outcome"
    );

    let head = b"head-range-payload";
    let middle = b"middle-range-payload";
    let tail = b"tail-range-payload";
    let middle_offset = 4 * GIB + 256 * MIB;
    let tail_offset = FIVE_GIB - tail.len() as u64;

    assert!(
        middle_offset > u64::from(u32::MAX),
        "middle offset must exceed the 32-bit range"
    );
    assert!(
        middle_offset + middle.len() as u64 <= FIVE_GIB,
        "the middle write must stay inside the 5 GiB logical file"
    );
    assert!(
        tail_offset > u64::from(u32::MAX),
        "tail offset must exceed the 32-bit range"
    );

    part.write_at(0, head)
        .expect("write head range at offset 0");
    part.write_at(middle_offset, middle)
        .expect("write middle range at its absolute offset");
    part.write_at(tail_offset, tail)
        .expect("write tail range near the end of the logical file");
    part.sync_all().expect("sync part file after writes");

    assert_eq!(
        part.len(),
        FIVE_GIB,
        "writes must neither truncate nor grow the 5 GiB logical length"
    );

    let file = cap
        .open_read(PART_NAME)
        .expect("open part file for reading");

    let mut head_back = vec![0u8; head.len()];
    read_exact_at(&file, 0, &mut head_back);
    assert_eq!(
        head_back, head,
        "head bytes must match at absolute offset 0"
    );

    let mut middle_back = vec![0u8; middle.len()];
    read_exact_at(&file, middle_offset, &mut middle_back);
    assert_eq!(
        middle_back, middle,
        "middle bytes must match above 4 GiB without truncation"
    );

    let mut tail_back = vec![0u8; tail.len()];
    read_exact_at(&file, tail_offset, &mut tail_back);
    assert_eq!(
        tail_back, tail,
        "tail bytes must match near 5 GiB without truncation"
    );

    assert!(
        middle_offset > FOUR_GIB_BOUNDARY_MINUS_ONE,
        "the middle write must sit above 4 GiB"
    );
}

#[test]
fn planner_totals_above_four_gib_stay_gap_free_and_exact() {
    for total in [FIVE_GIB, TEN_GIB] {
        let SegmentPlan::Ranged(segments) =
            plan_segments(Some(total), true, 64).expect("plan a known large total")
        else {
            panic!("a known range-supported total must produce ranged segments");
        };

        assert_eq!(segments.len(), 64, "64 connections must be honored");
        assert_eq!(segments[0].start, 0, "the first segment starts at zero");
        assert_eq!(
            segments.last().expect("segments").end,
            total,
            "the final segment must end exactly at the total"
        );

        let mut expected_start = 0u64;
        let mut covered = 0u64;
        for segment in &segments {
            assert_eq!(
                segment.start, expected_start,
                "segments must be gap-free and half-open"
            );
            assert!(
                segment.end > segment.start,
                "every segment must cover at least one byte"
            );
            covered += segment.end - segment.start;
            expected_start = segment.end;
        }

        assert_eq!(covered, total, "segment lengths must sum to the total");
        assert!(
            segments.last().expect("segments").end > u64::from(u32::MAX),
            "the final ending offset must remain above the 32-bit range"
        );
    }
}

#[test]
fn segment_boundaries_at_four_gib_are_exact() {
    for total in [FOUR_GIB_BOUNDARY_MINUS_ONE, FOUR_GIB_BOUNDARY] {
        let SegmentPlan::Ranged(segments) =
            plan_segments(Some(total), true, 64).expect("plan a boundary total")
        else {
            panic!("a known range-supported total must produce ranged segments");
        };

        assert_eq!(
            segments.last().expect("segments").end,
            total,
            "a total at the 4 GiB boundary must not be rounded or truncated"
        );
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.end - segment.start)
                .sum::<u64>(),
            total,
            "half-open segment lengths must add up to the exact total"
        );
    }
}

#[test]
fn content_range_round_trips_above_four_gib() {
    // Content-Range byte bounds are inclusive; parsing the formatted value must
    // return the same u64 pair without narrowing.
    for (start, end) in [
        (
            FOUR_GIB_BOUNDARY_MINUS_ONE,
            FOUR_GIB_BOUNDARY_MINUS_ONE + 511,
        ),
        (FOUR_GIB_BOUNDARY, FOUR_GIB_BOUNDARY + 511),
        (4_500_000_000, 4_500_000_511),
    ] {
        let formatted = format_content_range(start, end);
        assert!(
            formatted.contains(&start.to_string()),
            "formatted Content-Range must contain the exact start {start}: {formatted}"
        );
        assert!(
            formatted.contains(&end.to_string()),
            "formatted Content-Range must contain the exact end {end}: {formatted}"
        );

        let parsed = parse_content_range_value(&formatted).expect("parse formatted Content-Range");
        assert_eq!(
            parsed,
            (start, end),
            "Content-Range round trip must be lossless: {formatted}"
        );
    }
}

#[cfg(unix)]
fn read_exact_at(file: &std::fs::File, offset: u64, buffer: &mut [u8]) {
    use std::os::unix::fs::FileExt;

    file.read_exact_at(buffer, offset).expect("read range");
}

#[cfg(windows)]
fn read_exact_at(file: &std::fs::File, offset: u64, buffer: &mut [u8]) {
    use std::os::windows::fs::FileExt;

    let mut read = 0usize;
    while read < buffer.len() {
        let position = offset + read as u64;
        let count = file
            .seek_read(&mut buffer[read..], position)
            .expect("read range");
        assert!(count > 0, "range read must make progress");
        read += count;
    }
}

#[cfg(not(any(unix, windows)))]
fn read_exact_at(file: &std::fs::File, offset: u64, buffer: &mut [u8]) {
    use std::io::{Read, Seek, SeekFrom};

    let mut handle = file.try_clone().expect("clone file handle");
    handle.seek(SeekFrom::Start(offset)).expect("seek range");
    handle.read_exact(buffer).expect("read range");
}
