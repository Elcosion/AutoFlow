//! Input-free process fixture for exercising real pipe framing and teardown.
//! This process never installs hooks, captures a desktop, or injects input.
use autoflow_lib::runtime_protocol::{read_frame, write_frame, ActionEnvelope, ActionReceiver};

fn main() {
    #[cfg(windows)]
    for (flag, confirmed) in [
        ("--runtime-safety-mock-startup-failure", true),
        ("--runtime-safety-mock-startup-quarantine", false),
    ] {
        if std::env::args().any(|argument| argument == flag) {
            autoflow_lib::runtime_service::worker_main_mock_startup_failure(confirmed);
            return;
        }
    }
    #[cfg(windows)]
    if std::env::args().any(|argument| argument == "--runtime-safety-mock-unsafe-cleanup") {
        autoflow_lib::runtime_service::worker_main_mock_unsafe_cleanup();
        return;
    }
    #[cfg(windows)]
    if std::env::args().any(|argument| argument == "--own-safety-service") {
        let executable = std::env::current_exe().expect("fixture path");
        let client = autoflow_lib::runtime_service::SafetyClient::spawn(
            &executable,
            autoflow_lib::AppConfig::default(),
            std::env::temp_dir(),
            true,
        )
        .expect("input-free service");
        write_frame(&mut std::io::stdout().lock(), &client.pid).expect("fixture PID");
        loop {
            std::thread::park();
        }
    }
    #[cfg(windows)]
    if std::env::args().any(|argument| argument == "--runtime-safety-mock") {
        autoflow_lib::runtime_service::worker_main_mock_with_dispatch_hook(block_mock_control);
        return;
    }
    if std::env::args().any(|argument| argument == "--runtime-executor") {
        autoflow_lib::runtime_executor::worker_main();
        return;
    }
    if std::env::args().any(|argument| argument == "--supervise-child") {
        let executable = match std::env::current_exe() {
            Ok(path) => path,
            Err(_) => std::process::exit(2),
        };
        let mut command = std::process::Command::new(executable);
        command.arg("--ignore-parent");
        let worker = match autoflow_lib::runtime_process::SupervisedWorker::spawn(&mut command) {
            Ok(worker) => worker,
            Err(_) => std::process::exit(2),
        };
        if write_frame(&mut std::io::stdout().lock(), &worker.id()).is_err() {
            std::process::exit(2);
        }
        // The test kills this owning process, so its Rust destructors cannot
        // run. Only OS job-handle closure can contain the dependent worker.
        loop {
            std::thread::park();
        }
    }
    if std::env::args().any(|argument| argument == "--ignore-parent") {
        // Input-free deliberately unresponsive worker, bounded by its owner.
        loop {
            std::thread::park();
        }
    }
    let mut reader = std::io::stdin().lock();
    let mut writer = std::io::stdout().lock();
    let first: ActionEnvelope = match read_frame(&mut reader) {
        Ok(first) => first,
        Err(_) => std::process::exit(2),
    };
    // The probe has no production authority. Bootstrap identity is supplied
    // by its private parent pipe; production bootstrap will use a separate
    // trusted message before accepting any executor-supplied actions.
    let mut receiver = match ActionReceiver::new(first.identity.clone(), first.secret.clone()) {
        Ok(receiver) => receiver,
        Err(_) => std::process::exit(2),
    };
    let mut next = Some(first);
    loop {
        let envelope = match next.take() {
            Some(first) => first,
            None => match read_frame::<ActionEnvelope>(&mut reader) {
                Ok(envelope) => envelope,
                Err(_) => {
                    receiver.revoke();
                    break;
                }
            },
        };
        let accepted = receiver.accept(envelope).is_ok();
        if write_frame(&mut writer, &accepted).is_err() {
            receiver.revoke();
            break;
        }
        if !accepted {
            receiver.revoke();
            break;
        }
    }
}

#[cfg(windows)]
fn block_mock_control(command: &autoflow_lib::runtime_service::SafetyCommand) {
    use autoflow_lib::runtime_service::SafetyCommand;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenEventW, SetEvent, EVENT_MODIFY_STATE};
    let SafetyCommand::ConfigureVision { root } = command else {
        return;
    };
    let Some(name) = root
        .to_str()
        .filter(|name| name.starts_with("Local\\AutoFlow.MockBlockedRPC."))
    else {
        return;
    };
    let event = unsafe {
        OpenEventW(
            EVENT_MODIFY_STATE,
            false,
            &windows::core::HSTRING::from(name),
        )
    }
    .expect("private fixture entered event");
    unsafe { SetEvent(event) }.expect("signal blocked dispatch");
    unsafe { CloseHandle(event) }.expect("close fixture event");
    // Only this input-free fixture can dispatch the hook. No reply, no input,
    // and no cooperation with the ordinary RPC loop after this point.
    loop {
        std::thread::park();
    }
}
