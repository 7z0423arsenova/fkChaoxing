use std::process::Command;

#[test]
fn ask_reports_missing_provider_instead_of_claiming_an_answer() {
    let dir = std::env::temp_dir().join(format!(
        "fkchaoxing-rust-cli-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("config.ini");
    std::fs::write(&config, "[tiku]\nprovider=\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_fkChaoxing"))
        .current_dir(&dir)
        .args(["-c", config.to_str().unwrap(), "ask", "中国的首都是哪里？"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("题库"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("ok"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn replay_dry_run_lists_audio_requests_in_capture_order() {
    let dir = std::env::temp_dir().join(format!(
        "fkchaoxing-replay-cli-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let capture = dir.join("capture.har");
    std::fs::write(
        &capture,
        r#"{"log":{"entries":[
            {"startedDateTime":"2026-11-05T10:00:30.000Z","request":{"method":"GET","url":"https://mooc1.chaoxing.com/mooc-ans/multimedia/log/a/1/t?playingTime=30&duration=600&dtype=Audio&enc=x&_t=1","headers":[]}},
            {"startedDateTime":"2026-11-05T10:00:00.000Z","request":{"method":"GET","url":"https://mooc1.chaoxing.com/mooc-ans/multimedia/log/a/1/t?playingTime=0&duration=600&dtype=Audio&enc=x&_t=1","headers":[]}}
        ]}}"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_fkChaoxing"))
        .current_dir(&dir)
        .args([
            "-c",
            "missing.ini",
            "replay",
            capture.to_str().unwrap(),
            "--dry-run",
            "--media",
            "audio",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("匹配媒体进度报文 2 条"));
    assert!(stdout.contains("dry-run"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    let first = stderr.find("00:00 / 10:00").unwrap();
    let second = stderr.find("00:30 / 10:00").unwrap();
    assert!(first < second);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn study_subcommand_is_required() {
    let output = Command::new(env!("CARGO_BIN_EXE_fkChaoxing"))
        .args(["--config", "config.ini"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Usage:"));
}
