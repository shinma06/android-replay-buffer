#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    missing_docs,
    reason = "CLI binary prints user-facing output and omits library docstrings"
)]

fn cli_banner() -> String {
    let (_, schema) = arb_core::crate_info();
    format!("arb-cli v2 stub (schema={schema})")
}

fn main() {
    println!("{}", cli_banner());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_cli_banner_reports_schema_v2() {
        assert_eq!(cli_banner(), "arb-cli v2 stub (schema=2)");
    }
}
