//! The `agmen` command line, built in this package so its end-to-end tests
//! can run it (see Cargo.toml).

fn main() -> std::process::ExitCode {
    agmen_cli::run()
}
