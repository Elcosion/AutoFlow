//! Real process integration with an input-free fixture, never a real macro.
use autoflow_lib::runtime_process::SupervisedWorker;
use autoflow_lib::runtime_protocol::{
    read_frame, write_frame, ActionEnvelope, ExecutorAction, RunIdentity, MAX_FRAME_BYTES,
    PROTOCOL_VERSION,
};
use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Probe(Child);
impl Probe {
    fn start() -> Self {
        Self(
            Command::new(env!("CARGO_BIN_EXE_runtime_protocol_probe"))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("input-free protocol process"),
        )
    }
    fn exits(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if self.0.try_wait().expect("poll").is_some() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "probe did not exit after disconnect/fault"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn envelope() -> ActionEnvelope {
    ActionEnvelope {
        version: PROTOCOL_VERSION,
        secret: "f".repeat(64),
        identity: RunIdentity {
            session: "isolated-process-test".into(),
            run: 1,
            generation: 3,
        },
        sequence: 1,
        action: ExecutorAction::Move { x: -50, y: 20 },
    }
}

#[test]
fn private_pipe_accepts_one_action_and_duplicate_causes_exit() {
    let mut probe = Probe::start();
    write_frame(probe.0.stdin.as_mut().expect("stdin"), &envelope()).expect("send");
    // Read via a deadline-bounded thread, so a broken child cannot hang cargo.
    let mut stdout = probe.0.stdout.take().expect("stdout");
    let (sender, receiver) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        sender
            .send(read_frame::<bool>(&mut stdout))
            .expect("first result");
        sender
            .send(read_frame::<bool>(&mut stdout))
            .expect("duplicate result");
    });
    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(3))
            .expect("first ack"),
        Ok(true)
    );
    write_frame(probe.0.stdin.as_mut().expect("stdin"), &envelope()).expect("duplicate");
    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(3))
            .expect("rejection"),
        Ok(false)
    );
    probe.exits();
    reader.join().expect("reader exit");
}

#[test]
fn parent_disconnect_exits_without_automatic_restart() {
    let mut probe = Probe::start();
    drop(probe.0.stdin.take());
    probe.exits();
}

#[test]
fn oversized_header_exits_without_waiting_for_body() {
    let mut probe = Probe::start();
    let stdin = probe.0.stdin.as_mut().expect("stdin");
    stdin
        .write_all(&(MAX_FRAME_BYTES as u32 + 1).to_le_bytes())
        .expect("header");
    stdin.flush().expect("flush");
    probe.exits();
}

#[test]
fn supervised_disconnect_revokes_before_confirming_worker_exit() {
    let mut command = Command::new(env!("CARGO_BIN_EXE_runtime_protocol_probe"));
    let mut worker = SupervisedWorker::spawn(&mut command).expect("supervised worker");
    let revoked = std::sync::atomic::AtomicBool::new(false);
    let report = worker.stop(
        || revoked.store(true, std::sync::atomic::Ordering::Release),
        Duration::from_secs(1),
    );
    assert!(revoked.load(std::sync::atomic::Ordering::Acquire));
    assert!(!report.forced);
    assert!(report.status.is_some());
    assert!(report.error.is_none());
}

#[test]
fn unresponsive_worker_is_terminated_but_input_cleanup_is_not_assumed() {
    let mut command = Command::new(env!("CARGO_BIN_EXE_runtime_protocol_probe"));
    command.arg("--ignore-parent");
    let mut worker = SupervisedWorker::spawn(&mut command).expect("unresponsive worker");
    let tracked_inputs = std::sync::atomic::AtomicUsize::new(1);
    let revoked = std::sync::atomic::AtomicBool::new(false);
    let report = worker.stop(
        || revoked.store(true, std::sync::atomic::Ordering::Release),
        Duration::from_millis(20),
    );
    assert!(revoked.load(std::sync::atomic::Ordering::Acquire));
    assert!(report.forced);
    assert!(report.status.is_some());
    assert!(report.error.is_none());
    assert_eq!(tracked_inputs.load(std::sync::atomic::Ordering::Acquire), 1);
    assert!(worker.try_wait().expect("exit query").is_some());
}

#[cfg(windows)]
#[test]
fn owner_process_death_closes_job_and_terminates_dependent_worker() {
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };
    let mut owner = Probe(
        Command::new(env!("CARGO_BIN_EXE_runtime_protocol_probe"))
            .arg("--supervise-child")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("owner fixture"),
    );
    let mut output = owner.0.stdout.take().expect("output");
    let (sender, receiver) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let _ = sender.send(read_frame::<u32>(&mut output));
    });
    let dependent_pid = receiver
        .recv_timeout(Duration::from_secs(3))
        .expect("dependent PID")
        .expect("PID frame");
    reader.join().expect("PID reader");
    // Keep a process handle, not a reusable PID, across the parent's death.
    let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, dependent_pid) }
        .expect("dependent handle");
    owner
        .0
        .kill()
        .expect("terminate only owned input-free fixture");
    owner.exits();
    let result = unsafe { WaitForSingleObject(process, 3000) };
    let _ = unsafe { CloseHandle(process) };
    assert_eq!(
        result, WAIT_OBJECT_0,
        "dependent survived owning process death"
    );
}
