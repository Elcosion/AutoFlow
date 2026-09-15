use autoflow_lib::automation::{run_vision_benchmark, run_vision_diagnostic};
use std::time::Duration;

fn duration_seconds() -> u64 {
    let mut args = std::env::args().skip(1);
    while let Some(argument) = args.next() {
        if argument == "--duration-seconds" {
            if let Some(value) = args.next() {
                if let Ok(seconds) = value.parse::<u64>() {
                    return seconds.min(600);
                }
            }
        }
    }
    600
}

fn main() {
    println!("AutoFlow pure-memory vision benchmark");
    println!("{}", run_vision_benchmark());
    if std::env::args().any(|argument| argument == "--capture-diagnostic") {
        let seconds = duration_seconds();
        println!("AutoFlow capture diagnostic; trend duration={}s", seconds);
        println!("{}", run_vision_diagnostic(Duration::from_secs(seconds)));
    }
}
