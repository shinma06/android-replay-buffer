#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    missing_docs,
    reason = "binary daemon entrypoint may write to stdio and omit library docstrings"
)]

fn daemon_banner() -> String {
    let (_, core_schema) = arb_core::crate_info();
    let (os_name, _) = arb_os::crate_info();
    let (storage_name, _) = arb_storage::crate_info();
    let (media_name, _) = arb_media::crate_info();
    let (adb_name, _) = arb_adb::crate_info();
    format!(
        "arbd v2 stub (schema={core_schema}, modules={os_name},{storage_name},{media_name},{adb_name})"
    )
}

fn main() {
    println!("{}", daemon_banner());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_daemon_banner_lists_all_core_crates() {
        let banner = daemon_banner();
        assert!(banner.contains("schema=2"));
        assert!(banner.contains("arb-os"));
        assert!(banner.contains("arb-storage"));
        assert!(banner.contains("arb-media"));
        assert!(banner.contains("arb-adb"));
    }
}
