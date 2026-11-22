#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    missing_docs,
    reason = "tray binary may write diagnostics to stdio and omits library docstrings"
)]

fn tray_banner() -> String {
    let (_, schema) = arb_core::crate_info();
    format!("arb-tray v2 stub (schema={schema})")
}

fn main() {
    println!("{}", tray_banner());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_tray_banner_reports_schema_v2() {
        assert_eq!(tray_banner(), "arb-tray v2 stub (schema=2)");
    }
}
