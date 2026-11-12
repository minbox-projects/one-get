use oneget_lib::platform::filename::{sanitize_filename, FilenameTarget};

const RESERVED_WINDOWS: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

fn assert_common_output(output: &str, target: FilenameTarget, input_debug: &str) {
    assert!(
        !output.is_empty(),
        "output must be non-empty for target={target:?} input={input_debug}"
    );
    assert!(
        !output.contains('/') && !output.contains('\\'),
        "output must not contain path separators for target={target:?} input={input_debug}: {output:?}"
    );
    assert!(
        output != "." && output != "..",
        "output must not be dot segment for target={target:?} input={input_debug}: {output:?}"
    );
    assert!(
        !output.chars().any(|c| c == '\0' || c.is_control()),
        "output must not contain NUL/control chars for target={target:?} input={input_debug}: {output:?}"
    );
    assert!(
        output.len() <= 200,
        "output must be at most 200 bytes for target={target:?} input={input_debug}: len={} {output:?}",
        output.len()
    );
}

fn assert_windows_specific(output: &str, input_debug: &str) {
    assert!(
        !output.ends_with('.') && !output.ends_with(' '),
        "Windows output must not end with dot or space for input={input_debug}: {output:?}"
    );
    let upper = output.to_ascii_uppercase();
    let stem = upper.split('.').next().unwrap_or(&upper);
    assert!(
        !RESERVED_WINDOWS.contains(&stem),
        "Windows output must not be a reserved device name for input={input_debug}: {output:?}"
    );
}

#[test]
fn sanitize_filename_adversarial_table_satisfies_platform_constraints() {
    let long_unicode = "名".repeat(300);
    let inputs: Vec<(&str, String)> = vec![
        ("traversal", "../../.ssh/authorized_keys".to_string()),
        ("absolute_unix", "/etc/passwd".to_string()),
        (
            "absolute_windows",
            r"C:\Windows\System32\drivers\etc\hosts".to_string(),
        ),
        ("reserved_con", "CON".to_string()),
        ("reserved_aux_ext", "aux.txt".to_string()),
        ("trailing_dot", "name.".to_string()),
        ("trailing_space", "name ".to_string()),
        ("nul_and_control", "bad\0name\x01\x07\x1f".to_string()),
        ("empty", "".to_string()),
        ("long_unicode", long_unicode),
    ];

    for (label, input) in &inputs {
        let input_debug = format!("{label}:{input:?}");
        for target in [FilenameTarget::MacOs, FilenameTarget::Windows] {
            let first = sanitize_filename(input, target);
            let second = sanitize_filename(input, target);
            assert_eq!(
                first, second,
                "sanitize_filename must be deterministic for target={target:?} input={input_debug}"
            );
            assert_common_output(&first, target, &input_debug);
            if matches!(target, FilenameTarget::Windows) {
                assert_windows_specific(&first, &input_debug);
            }
        }
    }
}

#[test]
fn sanitize_filename_macos_keeps_ordinary_name_unchanged() {
    let input = "report.pdf";
    let output = sanitize_filename(input, FilenameTarget::MacOs);
    assert_eq!(output, input);
}
