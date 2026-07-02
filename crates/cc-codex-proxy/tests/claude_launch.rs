use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    thread,
};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_cc-codex-proxy")
}

fn fake_claude(dir: &Path) -> PathBuf {
    let path = dir.join("fake-claude");
    fs::write(
        &path,
        "#!/usr/bin/env bash\nprintf 'base=%s\\nmodel=%s\\nsmall=%s\\ncompact=%s\\n' \"$ANTHROPIC_BASE_URL\" \"$ANTHROPIC_MODEL\" \"$ANTHROPIC_SMALL_FAST_MODEL\" \"$CLAUDE_CODE_AUTO_COMPACT_WINDOW\"\n",
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn launch_runs_normal_claude_when_app_pid_is_dead() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_claude(dir.path());

    let output = Command::new(bin())
        .args(["claude", "launch", "--app-pid", "999999", "--real-claude"])
        .arg(&fake)
        .arg("--")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("base=\n"));
    assert!(stdout.contains("model=\n"));
}

#[test]
#[ignore = "requires local TCP listener support"]
fn launch_sets_proxy_env_when_app_pid_is_alive_and_health_is_ok() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_claude(dir.path());
    let port = healthy_once_server();

    let output = Command::new(bin())
        .args([
            "claude",
            "launch",
            "--app-pid",
            &std::process::id().to_string(),
            "--real-claude",
        ])
        .arg(&fake)
        .args(["--port", &port.to_string(), "--"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains(&format!("base=http://127.0.0.1:{port}\n")));
    assert!(stdout.contains("model=claude-opus-4-8\n"));
    assert!(stdout.contains("small=claude-haiku-4-5\n"));
    assert!(stdout.contains("compact=272000\n"));
}

#[test]
fn launch_fails_when_app_pid_is_alive_and_proxy_is_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_claude(dir.path());
    let port = 9;

    let output = Command::new(bin())
        .args([
            "claude",
            "launch",
            "--app-pid",
            &std::process::id().to_string(),
            "--real-claude",
        ])
        .arg(&fake)
        .args(["--port", &port.to_string(), "--"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert_eq!(output.stdout, b"");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("proxy server is stopped"));
}

fn healthy_once_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0; 1024];
            let _ = stream.read(&mut buf);
            let response =
                b"HTTP/1.1 200 OK\r\ncontent-length: 11\r\nconnection: close\r\n\r\n{\"ok\":true}";
            let _ = stream.write_all(response);
        }
    });
    port
}
