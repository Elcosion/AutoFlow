#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    if std::env::args().any(|argument| argument == "--runtime-safety") {
        autoflow_lib::runtime_service::worker_main(false);
        return;
    }
    if std::env::args().any(|argument| argument == "--runtime-executor") {
        autoflow_lib::runtime_executor::worker_main();
        return;
    }
    autoflow_lib::run();
}
