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
        if dir.join("hello.bin").is_file() {
            fs::write(dir.join("discovered"), []).unwrap();
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
    if args.ends_with(&["exec-out".into(), "screencap".into(), "-p".into()]) {
        std::io::stdout()
            .write_all(&fs::read(dir.join("screencap.png")).unwrap())
            .unwrap();
        return;
    }
    // A recorded Hello travels over a real socket once the device becomes visible.
    if dir.join("hello.bin").is_file() {
        if args.get(2).is_some_and(|a| a == "push") { return; }
        if line.contains("forward --no-rebind") {
            let port = args.iter().find_map(|s| s.strip_prefix("tcp:")).unwrap();
            fs::write(dir.join("port"), port).unwrap();
            return;
        }
        if line.contains("app_process") {
            let port = fs::read_to_string(dir.join("port")).unwrap();
            let listener = std::net::TcpListener::bind(format!("127.0.0.1:{port}")).unwrap();
            println!("ARB_COMPANION_READY");
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(&fs::read(dir.join("hello.bin")).unwrap()).unwrap();
            let _ = std::io::copy(&mut stream, &mut std::io::sink());
            return;
        }
    }
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
