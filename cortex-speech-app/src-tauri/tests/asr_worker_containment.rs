use std::process::Command;

#[test]
fn native_worker_abort_is_contained_and_supervisor_restarts_it() {
    let executable = assert_cmd::cargo::cargo_bin("cortex-speech-app");
    let output = Command::new(executable)
        .arg("--cortex-asr-supervisor-self-test")
        .output()
        .expect("run ASR supervisor fault-injection test");

    assert!(
        output.status.success(),
        "the host must survive a hard worker abort and restart it; status={:?}, stdout={}, stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
