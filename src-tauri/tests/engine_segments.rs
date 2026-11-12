use oneget_lib::engine::error::EngineError;
use oneget_lib::engine::planner::{
    plan_segments, PlannedSegment, SegmentPlan, MAX_CONNECTIONS, MIN_SEGMENT_BYTES,
};

fn assert_planned_segment_eq(seg: &PlannedSegment, index: u32, start: u64, end: u64) {
    assert_eq!(seg.index, index, "segment index");
    assert_eq!(seg.start, start, "segment {index} start");
    assert_eq!(seg.end, end, "segment {index} end");
    assert!(seg.end > seg.start, "segment {index} must be non-empty");
}

fn assert_ranged_plan_invariants(
    total: u64,
    connections: u32,
    plan: &SegmentPlan,
) -> Vec<PlannedSegment> {
    let segments = match plan {
        SegmentPlan::Ranged(segs) => segs.clone(),
        SegmentPlan::OpenEnded { .. } => panic!("expected Ranged plan for total {total}"),
    };

    if total == 0 {
        assert!(
            segments.is_empty(),
            "total 0 must produce an empty ranged plan"
        );
        return segments;
    }

    assert!(
        !segments.is_empty(),
        "non-zero known total must produce at least one segment"
    );
    assert!(
        segments.len() as u32 >= 1 && segments.len() as u32 <= MAX_CONNECTIONS,
        "segment count must be in 1..=64, got {}",
        segments.len()
    );

    // Half-open coverage [0,total), no gaps/overlaps, contiguous indices.
    let mut expected_start = 0u64;
    for (i, seg) in segments.iter().enumerate() {
        assert_eq!(seg.index as usize, i, "segment indices must be contiguous");
        assert_eq!(
            seg.start, expected_start,
            "gap/overlap or wrong start at segment {i}"
        );
        assert!(seg.end > seg.start, "segment {i} must have positive length");
        expected_start = seg.end;
    }
    assert_eq!(
        expected_start, total,
        "segments must cover exactly [0,total)"
    );

    // Size rule: each >= MIN except whole-small and final remainder.
    let whole_small = total < MIN_SEGMENT_BYTES;
    for (i, seg) in segments.iter().enumerate() {
        let len = seg.end - seg.start;
        let is_last = i + 1 == segments.len();
        if whole_small {
            // single segment covering the whole small file is allowed
            assert_eq!(segments.len(), 1, "whole-small file should be one segment");
            assert_eq!(len, total);
        } else if is_last {
            // final remainder may be smaller than MIN
            assert!(len >= 1, "final segment must be non-empty");
        } else {
            assert!(
                len >= MIN_SEGMENT_BYTES,
                "interior segment {i} length {len} must be >= MIN_SEGMENT_BYTES ({MIN_SEGMENT_BYTES})"
            );
        }
    }

    // Expected count formula: min(connections, max(1, total / MIN_SEGMENT_BYTES))
    let expected_count = std::cmp::min(
        connections as u64,
        std::cmp::max(1u64, total / MIN_SEGMENT_BYTES),
    ) as usize;
    assert_eq!(
        segments.len(),
        expected_count,
        "segment count must follow min(connections, max(1, total / MIN_SEGMENT_BYTES)) for total={total} connections={connections}"
    );

    segments
}

fn assert_code(err: &EngineError, code: &str) {
    assert_eq!(
        err.code(),
        code,
        "expected code {code}, got {}: {}",
        err.code(),
        err
    );
}

#[test]
fn planner_matrix_known_range_totals_cover_segments_and_expected_count_formula() {
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * 1024 * 1024;
    let totals: [u64; 7] = [
        0,
        1,
        MIN_SEGMENT_BYTES - 1,
        MIN_SEGMENT_BYTES,
        8 * MIB,
        64 * MIB,
        10 * GIB,
    ];
    let connections: [u32; 3] = [1, 8, 64];

    for total in totals {
        for c in connections {
            let plan = plan_segments(Some(total), true, c)
                .unwrap_or_else(|e| panic!("plan_segments(Some({total}), true, {c}) failed: {e}"));
            let segments = assert_ranged_plan_invariants(total, c, &plan);
            match &plan {
                SegmentPlan::Ranged(planned) => {
                    assert_eq!(
                        planned.len(),
                        segments.len(),
                        "returned segment list must match plan"
                    );
                    for seg in &segments {
                        assert_planned_segment_eq(seg, seg.index, seg.start, seg.end);
                    }
                }
                SegmentPlan::OpenEnded { .. } => panic!("known range-capable total must be Ranged"),
            }

            // Spot-check coverage sum equals total for non-empty plans.
            let sum: u64 = segments.iter().map(|s| s.end - s.start).sum();
            assert_eq!(sum, total, "sum of segment lengths must equal total");
        }
    }
}

#[test]
fn planner_rejects_invalid_connection_counts_with_invalid_connections() {
    for c in [0u32, 65u32] {
        let err = plan_segments(Some(8 * 1024 * 1024), true, c)
            .expect_err("invalid connection count must fail");
        assert_code(&err, "INVALID_CONNECTIONS");
    }
}

#[test]
fn planner_returns_single_open_ended_segment_for_unknown_length_or_no_range_support() {
    // Unknown length -> single OpenEnded segment.
    let plan_unknown = plan_segments(None, true, 8).expect("unknown length should plan");
    match plan_unknown {
        SegmentPlan::OpenEnded { start } => assert_eq!(start, 0, "open-ended segment starts at 0"),
        SegmentPlan::Ranged(_) => panic!("unknown length must produce OpenEnded plan"),
    }

    // No range support -> single OpenEnded segment even if total known.
    let plan_no_range =
        plan_segments(Some(64 * 1024 * 1024), false, 8).expect("no-range known total should plan");
    match plan_no_range {
        SegmentPlan::OpenEnded { start } => assert_eq!(start, 0, "open-ended segment starts at 0"),
        SegmentPlan::Ranged(_) => panic!("no range support must produce OpenEnded plan"),
    }

    // Unknown length and no range support.
    let plan_both = plan_segments(None, false, 1).expect("unknown/no-range should plan");
    match plan_both {
        SegmentPlan::OpenEnded { start } => assert_eq!(start, 0),
        SegmentPlan::Ranged(_) => panic!("must produce OpenEnded plan"),
    }

    // Keep compiler aware of PlannedSegment field access shape in this crate.
    let _ = |s: &PlannedSegment| (s.index, s.start, s.end);
}

#[test]
fn planner_zero_total_is_empty_ranged_plan() {
    let plan = plan_segments(Some(0), true, 8).expect("zero total should plan");
    match plan {
        SegmentPlan::Ranged(segments) => assert!(segments.is_empty(), "zero total must be empty"),
        SegmentPlan::OpenEnded { .. } => panic!("zero total must be Ranged(empty), not OpenEnded"),
    }
}

mod part_file_tests {
    use oneget_lib::engine::part_file::PartFile;
    use oneget_lib::platform::files::CapDir;
    use sha2::{Digest, Sha256};
    use tempfile::tempdir;

    fn sha256_hex(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let digest = hasher.finalize();
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[tokio::test]
    async fn part_file_preallocates_writes_at_absolute_offsets_syncs_reads_back_and_hashes() {
        let root = tempdir().expect("tempdir");
        let cap = CapDir::open(root.path()).expect("open capdir");
        let total = 1024 * 1024u64; // 1 MiB
        let filename = "payload.bin";

        let part = PartFile::create(&cap, filename, Some(total)).expect("create part file");
        assert_eq!(
            PartFile::part_name(filename),
            "payload.bin.oneget.part",
            "part_name must return <filename>.oneget.part"
        );
        assert_eq!(
            part.part_name(),
            "payload.bin.oneget.part",
            "instance part_name must match expected suffix"
        );
        assert_eq!(part.len(), total, "known total must be preallocated");

        let head: Vec<u8> = (0u8..64).collect();
        let middle_offset = 512 * 1024u64;
        let middle: Vec<u8> = (100u8..164).collect();

        part.write_at(0, &head).expect("write head at offset 0");
        part.write_at(middle_offset, &middle)
            .expect("write middle at absolute offset");
        part.sync_all().expect("sync part file");

        let stored = cap
            .read("payload.bin.oneget.part")
            .await
            .expect("read part file back");
        assert_eq!(
            stored.len() as u64,
            total,
            "stored length must remain preallocated"
        );

        let mut expected = vec![0u8; total as usize];
        expected[0..head.len()].copy_from_slice(&head);
        let mid_start = middle_offset as usize;
        expected[mid_start..mid_start + middle.len()].copy_from_slice(&middle);
        assert_eq!(
            stored, expected,
            "full preallocated-plus-written bytes must match"
        );
        assert_eq!(
            part.sha256(),
            sha256_hex(&expected),
            "part sha256 must equal SHA-256 of expected buffer"
        );
    }

    #[tokio::test]
    async fn zero_length_part_file_hashes_to_sha256_of_empty_input() {
        let root = tempdir().expect("tempdir");
        let cap = CapDir::open(root.path()).expect("open capdir");

        let part = PartFile::create(&cap, "empty.bin", Some(0)).expect("create zero-length part");
        assert_eq!(part.len(), 0, "zero-length part must have len 0");
        assert_eq!(
            part.sha256(),
            sha256_hex(&[]),
            "zero-length part must hash to SHA-256 of empty input"
        );

        // Ensure the part file exists with expected name and is empty on disk.
        let stored = cap
            .read("empty.bin.oneget.part")
            .await
            .expect("read empty part");
        assert!(stored.is_empty(), "stored zero-length part must be empty");
    }
}
