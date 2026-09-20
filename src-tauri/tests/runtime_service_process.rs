//! Real control-service pipes and lease watchdog; mock authority has no OS input.
#![cfg(windows)]
use autoflow_lib::runtime_service::{SafetyClient, SafetyCommand};
use autoflow_lib::{AppConfig, MacroRule};
use serde_json::Value;
use std::path::Path;
use std::time::{Duration, Instant};

#[test]
fn actual_ui_process_death_is_observed_without_rust_destructors() {
    use autoflow_lib::runtime_protocol::read_frame;
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };
    struct Owner(std::process::Child);
    impl Drop for Owner {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut owner = Owner(
        std::process::Command::new(env!("CARGO_BIN_EXE_runtime_protocol_probe"))
            .arg("--own-safety-service")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("owned input-free UI fixture"),
    );
    let mut output = owner.0.stdout.take().expect("PID pipe");
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let _ = tx.send(read_frame::<u32>(&mut output));
    });
    let pid = rx
        .recv_timeout(Duration::from_secs(4))
        .expect("bounded PID reply")
        .expect("PID frame");
    reader.join().expect("reader");
    let process =
        unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }.expect("own service handle");
    owner.0.kill().expect("kill only input-free owner fixture");
    let result = unsafe { WaitForSingleObject(process, 4000) };
    unsafe { CloseHandle(process) }.expect("close handle");
    assert_eq!(result, WAIT_OBJECT_0, "service survived parent death");
}

fn client() -> SafetyClient {
    SafetyClient::spawn(
        Path::new(env!("CARGO_BIN_EXE_runtime_protocol_probe")),
        AppConfig::default(),
        std::env::temp_dir(),
        true,
    )
    .expect("input-free safety service")
}

#[test]
fn startup_failure_reply_requires_confirmed_exit_or_retained_authority_quarantine() {
    use autoflow_lib::service_transport::{read_document, write_document};
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use std::os::windows::process::CommandExt;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::{CreateEventW, CreateMutexW};

    struct Fixture(std::process::Child);
    impl Drop for Fixture {
        fn drop(&mut self) {
            // Retained, freshly created input-free child only; no PID search.
            let _ = self.0.kill();
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                if self.0.try_wait().ok().flatten().is_some() {
                    return;
                }
                std::thread::yield_now();
            }
        }
    }
    fn authority_exists(session: &str) -> bool {
        let name =
            windows::core::HSTRING::from(format!("Local\\AutoFlow.MockInputAuthority.{session}"));
        let handle = unsafe { CreateMutexW(None, false, &name) }.expect("private authority probe");
        let exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        let _handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
        exists
    }
    fn spawn_fixture(flag: &str, bootstrap: &Value) -> (Fixture, Value) {
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_runtime_protocol_probe"))
            .arg(flag)
            .creation_flags(0x08000000)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("own input-free child");
        let mut stdin = child.stdin.take().expect("bootstrap pipe");
        let mut stdout = child.stdout.take().expect("reply pipe");
        let fixture = Fixture(child);
        write_document(&mut stdin, bootstrap).expect("private bootstrap");
        drop(stdin);
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let reader = std::thread::spawn(move || {
            let _ = tx.send(read_document::<Value>(&mut stdout));
        });
        let response = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("bounded startup response")
            .expect("startup document");
        reader.join().expect("fixture reader exit");
        (fixture, response)
    }
    for confirmed in [true, false] {
        let session = format!(
            "{:032x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        );
        let secret = session.repeat(2);
        let lease_name = format!("Local\\AutoFlow.ControlLease.{secret}");
        let stop_name = format!("Local\\AutoFlow.StopSignal.{secret}");
        let lease = unsafe {
            CreateEventW(
                None,
                false,
                true,
                &windows::core::HSTRING::from(&lease_name),
            )
        }
        .expect("private lease event");
        let _lease = unsafe { OwnedHandle::from_raw_handle(lease.0) };
        let stop = unsafe {
            CreateEventW(
                None,
                false,
                false,
                &windows::core::HSTRING::from(&stop_name),
            )
        }
        .expect("private stop event");
        let _stop = unsafe { OwnedHandle::from_raw_handle(stop.0) };
        let bootstrap = serde_json::json!({
            "version":2, "secret":secret, "session":session, "parent_pid":std::process::id(),
            "lease_name":lease_name, "stop_name":stop_name,
            "config":AppConfig::default(), "image_root":std::env::temp_dir(),
        });
        let flag = if confirmed {
            "--runtime-safety-mock-startup-failure"
        } else {
            "--runtime-safety-mock-startup-quarantine"
        };
        let (mut fixture, response) = spawn_fixture(flag, &bootstrap);
        assert_eq!(response["version"], 2);
        assert_eq!(response["secret"], secret);
        assert_eq!(response["session"], session);
        assert_eq!(response["sequence"], 0);
        assert_eq!(response["generation"], 0);
        if confirmed {
            assert_eq!(response["result"]["Err"]["code"], "mock_startup_failure");
            let deadline = Instant::now() + Duration::from_secs(2);
            while fixture
                .0
                .try_wait()
                .expect("retained child status")
                .is_none()
            {
                assert!(Instant::now() < deadline, "confirmed rollback did not exit");
                std::thread::yield_now();
            }
            assert!(
                !authority_exists(&session),
                "confirmed exit releases fixture authority"
            );
        } else {
            assert_eq!(
                response["result"]["Err"]["code"],
                "input_service_start_rollback_unconfirmed"
            );
            assert!(
                fixture.0.try_wait().expect("own child status").is_none(),
                "startup error is not safe exit"
            );
            assert!(
                authority_exists(&session),
                "quarantine must retain authority"
            );
            let (mut duplicate, rejection) = spawn_fixture(flag, &bootstrap);
            assert_eq!(
                rejection["result"]["Err"]["code"],
                "safety_service_unavailable"
            );
            let deadline = Instant::now() + Duration::from_secs(2);
            while duplicate.0.try_wait().expect("duplicate status").is_none() {
                assert!(
                    Instant::now() < deadline,
                    "duplicate authority should exit without initialization"
                );
                std::thread::yield_now();
            }
            assert!(
                fixture.0.try_wait().expect("original status").is_none(),
                "rejected start cannot dispose quarantined authority"
            );
        }
    }
}
fn play(client: &SafetyClient) -> Result<(), autoflow_lib::AppError> {
    let rule: MacroRule = serde_json::from_value(serde_json::json!({
        "id": "mock", "name": "input-free", "program": {"kind":"macro","steps":[]}
    }))
    .expect("mock rule");
    client.call(SafetyCommand::Play {
        rule: Box::new(rule),
    })
}
fn status(client: &SafetyClient) -> Value {
    client.call(SafetyCommand::PlaybackStatus).expect("status")
}

#[test]
fn process_dispatch_rejects_stale_identity_but_keeps_emergency_commands_live() {
    use autoflow_lib::service_transport::{read_document, write_document};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::os::windows::process::CommandExt;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Threading::{CreateEventW, SetEvent};

    struct Fixture {
        child: std::process::Child,
        stdin: std::process::ChildStdin,
        stdout: std::process::ChildStdout,
        lease: OwnedHandle,
        secret: String,
        session: String,
        sequence: u64,
    }
    impl Fixture {
        fn spawn() -> Self {
            let session = format!(
                "{:032x}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock")
                    .as_nanos()
            );
            let secret = session.repeat(2);
            let lease_name = format!("Local\\AutoFlow.ControlLease.{secret}");
            let stop_name = format!("Local\\AutoFlow.StopSignal.{secret}");
            let lease = unsafe {
                CreateEventW(
                    None,
                    false,
                    true,
                    &windows::core::HSTRING::from(&lease_name),
                )
            }
            .expect("private lease event");
            let lease = unsafe { OwnedHandle::from_raw_handle(lease.0) };
            let stop = unsafe {
                CreateEventW(
                    None,
                    false,
                    false,
                    &windows::core::HSTRING::from(&stop_name),
                )
            }
            .expect("private stop event");
            let _stop = unsafe { OwnedHandle::from_raw_handle(stop.0) };
            let mut child =
                std::process::Command::new(env!("CARGO_BIN_EXE_runtime_protocol_probe"))
                    .arg("--runtime-safety-mock")
                    .creation_flags(0x08000000)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .expect("own input-free child");
            let mut stdin = child.stdin.take().expect("request pipe");
            let mut stdout = child.stdout.take().expect("response pipe");
            write_document(
                &mut stdin,
                &serde_json::json!({
                    "version": 2,
                    "secret": secret,
                    "session": session,
                    "parent_pid": std::process::id(),
                    "lease_name": lease_name,
                    "stop_name": stop_name,
                    "config": AppConfig::default(),
                    "image_root": std::env::temp_dir(),
                }),
            )
            .expect("bootstrap request");
            let ready: Value = read_document(&mut stdout).expect("bootstrap response");
            assert_eq!(ready["result"]["Ok"], "ready");
            Self {
                child,
                stdin,
                stdout,
                lease,
                secret,
                session,
                sequence: 0,
            }
        }

        fn call(&mut self, command: SafetyCommand, generation: u64, revision: u64) -> Value {
            unsafe { SetEvent(HANDLE(self.lease.as_raw_handle())) }.expect("pulse private lease");
            self.sequence += 1;
            write_document(
                &mut self.stdin,
                &serde_json::json!({
                    "version": 2,
                    "secret": self.secret,
                    "session": self.session,
                    "sequence": self.sequence,
                    "generation": generation,
                    "admission_revision": revision,
                    "command": command,
                }),
            )
            .expect("service request");
            read_document(&mut self.stdout).expect("service response")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    let mut fixture = Fixture::spawn();
    let rule: MacroRule = serde_json::from_value(serde_json::json!({
        "id": "process-identity-test",
        "name": "input-free",
        "program": {"kind": "macro", "steps": []}
    }))
    .expect("mock rule");
    let started = fixture.call(
        SafetyCommand::Play {
            rule: Box::new(rule.clone()),
        },
        0,
        0,
    );
    assert!(started["result"].get("Ok").is_some());
    assert_eq!(started["admission_revision"], 1);

    let stale_revision = fixture.call(SafetyCommand::PlaybackStatus, 0, 0);
    assert_eq!(
        stale_revision["result"]["Err"]["code"],
        "safety_service_stale_request"
    );
    let active = fixture.call(SafetyCommand::PlaybackStatus, 0, 1);
    assert_eq!(active["result"]["Ok"]["running"], true);

    let stopped = fixture.call(SafetyCommand::Stop, u64::MAX, u64::MAX);
    assert!(stopped["result"].get("Ok").is_some());
    assert_eq!(stopped["generation"], 1);
    assert_eq!(stopped["admission_revision"], 2);
    let stale_generation = fixture.call(SafetyCommand::PlaybackStatus, 0, 1);
    assert_eq!(
        stale_generation["result"]["Err"]["code"],
        "safety_service_stale_request"
    );
    let stale_play = fixture.call(
        SafetyCommand::Play {
            rule: Box::new(rule.clone()),
        },
        0,
        1,
    );
    assert_eq!(
        stale_play["result"]["Err"]["code"],
        "safety_service_stale_request"
    );
    let idle = fixture.call(SafetyCommand::PlaybackStatus, 1, 2);
    assert_eq!(idle["result"]["Ok"]["running"], false);

    let restarted = fixture.call(
        SafetyCommand::Play {
            rule: Box::new(rule),
        },
        1,
        2,
    );
    assert_eq!(restarted["result"]["Ok"], Value::Null);
    let shutdown = fixture.call(SafetyCommand::Shutdown, 0, 0);
    assert_eq!(shutdown["result"]["Ok"], true);
}

#[test]
fn rejected_start_cannot_destroy_active_service_run() {
    let client = client();
    play(&client).expect("first admission");
    assert!(play(&client).is_err());
    assert_eq!(status(&client)["running"], true);
    client.call::<()>(SafetyCommand::Stop).expect("stop");
    assert_eq!(status(&client)["running"], false);
    play(&client).expect("explicit new admission");
    assert!(client
        .call::<bool>(SafetyCommand::Shutdown)
        .expect("shutdown"));
}
#[test]
fn independent_stop_signal_revokes_without_a_stop_rpc() {
    let client = client();
    play(&client).expect("admission");
    client.stop_signal().expect("priority event");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        assert!(Instant::now() < deadline, "priority stop not observed");
        match client.call::<Value>(SafetyCommand::PlaybackStatus) {
            Ok(status) if status["running"] == false => break,
            Ok(_) => {}
            Err(error) if error.code == "safety_service_stale_request" => {}
            Err(error) => panic!("unexpected status rejection: {error:?}"),
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(client
        .call::<bool>(SafetyCommand::Shutdown)
        .expect("shutdown"));
}
#[test]
fn dropping_ui_lease_causes_safety_process_exit() {
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };
    let client = client();
    play(&client).expect("admission");
    let process =
        unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, client.pid) }.expect("own child handle");
    drop(client);
    let result = unsafe { WaitForSingleObject(process, 4000) };
    unsafe { CloseHandle(process) }.expect("close own handle");
    assert_eq!(
        result, WAIT_OBJECT_0,
        "lease loss must exit, never auto-restart"
    );
}

#[test]
fn shutdown_requires_actual_process_exit_not_only_cleanup_reply() {
    use autoflow_lib::runtime_service::ServiceExitStatus;
    let client = client();
    assert_eq!(client.exit_status(), ServiceExitStatus::Alive);
    assert_eq!(
        client
            .confirm_exit(Duration::ZERO)
            .expect_err("live process must not appear exited")
            .code,
        "safety_service_exit_unconfirmed"
    );
    play(&client).expect("mock active run");
    client
        .shutdown_confirmed()
        .expect("cleanup reply and retained-handle exit confirmation");
    assert_eq!(client.exit_status(), ServiceExitStatus::Exited);
    assert!(client.call::<Value>(SafetyCommand::PlaybackStatus).is_err());
    assert!(client.confirm_exit(Duration::ZERO).is_ok());
}

#[test]
fn blocked_rpc_timeout_revokes_lease_and_guardian_confirms_actual_service_exit() {
    use autoflow_lib::runtime_service::ServiceExitStatus;
    use std::sync::Arc;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
    struct Event(HANDLE);
    impl Drop for Event {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
    let name = format!(
        "Local\\AutoFlow.MockBlockedRPC.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );
    let entered = Event(
        unsafe { CreateEventW(None, true, false, &windows::core::HSTRING::from(&name)) }
            .expect("own fixture event"),
    );
    let client = Arc::new(client());
    play(&client).expect("mock active run");
    let blocked_client = client.clone();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let request = std::thread::spawn(move || {
        let result =
            blocked_client.call::<Value>(SafetyCommand::ConfigureVision { root: name.into() });
        let _ = tx.send(result);
    });
    assert_eq!(
        unsafe { WaitForSingleObject(entered.0, 4000) },
        WAIT_OBJECT_0,
        "fault must actually enter the child RPC dispatcher"
    );
    assert_eq!(client.exit_status(), ServiceExitStatus::Alive);
    client
        .stop_signal()
        .expect("independent stop while RPC blocked");
    assert!(rx
        .recv_timeout(Duration::from_secs(7))
        .expect("bounded request timeout")
        .is_err());
    request.join().expect("caller exit");
    client
        .confirm_exit(Duration::from_secs(3))
        .expect("guardian must exit despite blocked RPC");
    assert_eq!(client.exit_status(), ServiceExitStatus::Exited);
    assert!(
        play(&client).is_err(),
        "no reconnect or replay after lease loss"
    );
}

#[test]
fn unsafe_cleanup_retains_process_and_cannot_report_safe_shutdown_or_resume() {
    use autoflow_lib::runtime_service::ServiceExitStatus;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };
    // This handle belongs only to the newly created input-free child. Always
    // dispose this intentionally quarantined fixture; never search user PIDs.
    struct Fixture(HANDLE);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = unsafe { TerminateProcess(self.0, 0) };
            let _ = unsafe { WaitForSingleObject(self.0, 3000) };
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
    let client = SafetyClient::spawn_mock_unsafe_cleanup(Path::new(env!(
        "CARGO_BIN_EXE_runtime_protocol_probe"
    )))
    .expect("own input-free unsafe-cleanup fixture");
    let process = Fixture(
        unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, client.pid) }
            .expect("retain own child identity before fault"),
    );
    play(&client).expect("mock active run");
    client.call::<()>(SafetyCommand::Stop).expect("stop");
    assert_eq!(status(&client)["phase"], "FaultLocked");
    assert!(
        play(&client).is_err(),
        "unsafe cleanup prohibits new playback"
    );
    assert!(!client
        .call::<bool>(SafetyCommand::Recover)
        .expect("recovery result"));
    assert!(!client
        .call::<bool>(SafetyCommand::Shutdown)
        .expect("unsafe shutdown reply"));
    assert_eq!(client.exit_status(), ServiceExitStatus::Alive);
    assert_eq!(
        client
            .confirm_exit(Duration::from_millis(100))
            .expect_err("quarantine is not safe exit")
            .code,
        "safety_service_exit_unconfirmed"
    );
    assert!(play(&client).is_err(), "closed client must not replay");
    unsafe { TerminateProcess(process.0, 0) }.expect("terminate only no-input fixture");
    assert_eq!(
        unsafe { WaitForSingleObject(process.0, 3000) },
        WAIT_OBJECT_0
    );
    client
        .confirm_exit(Duration::ZERO)
        .expect("actual fixture exit");
}
