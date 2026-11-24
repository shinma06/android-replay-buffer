//! Standalone ADB fixture for the subprocess supervisor test.
use std::{env, fs, io::Write, thread, time::Duration};

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let dir = std::path::PathBuf::from(env::var_os("FAKE_DEVICE_STATE").unwrap());
    if args.first().is_some_and(|arg| arg == "devices") {
        if dir.join("block").exists() {
            fs::write(dir.join("discovery-entered"), []).unwrap();
            while dir.join("block").exists() {
                thread::sleep(Duration::from_millis(10));
            }
        }
        println!("List of devices attached");
        print!(
            "{}",
            fs::read_to_string(dir.join("devices")).unwrap_or_default()
        );
        return;
    }
    if args == ["forward", "--list"] {
        println!(
            "{}",
            fs::read_to_string(dir.join("forwards")).unwrap_or_default()
        );
        return;
    }
    let line = args.join(" ");
    if line.contains("forward --remove") || line.contains("shell rm -f") {
        writeln!(
            fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(dir.join("reaped"))
                .unwrap(),
            "{line}"
        )
        .unwrap();
        return;
    }
    std::process::exit(1);
}
