mod common;

use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

use elephant_ladder_dictionary_pack::DictionaryPack;

#[test]
fn build_reads_decompressed_jsonl_from_stdin() {
    let source = b"{\"lang_code\":\"fr\",\"word\":\"streamed\"}\n";
    let temporary = tempfile::tempdir().unwrap();
    let snapshot_path = temporary.path().join("snapshot.json");
    fs::write(
        &snapshot_path,
        serde_json::to_vec(&common::fr_snapshot_json(source, 10)).unwrap(),
    )
    .unwrap();
    let output_path = temporary.path().join("pack");

    let mut child = Command::new(env!("CARGO_BIN_EXE_elephant-dictionary-pack"))
        .args(["build", "--edition"])
        .arg(common::fr_edition_path())
        .arg("--snapshot")
        .arg(&snapshot_path)
        .args(["--builder-revision", "cli-test", "--input", "-", "--output"])
        .arg(&output_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(source).unwrap();
    let result = child.wait_with_output().unwrap();

    assert!(
        result.status.success(),
        "builder failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8(result.stdout).unwrap();
    assert!(stdout.contains("pack_revision="), "{stdout}");
    assert!(stdout.contains("source_records=1\n"), "{stdout}");
    assert!(stdout.contains("selected_records=1\n"), "{stdout}");
    DictionaryPack::open(output_path).unwrap();
}
