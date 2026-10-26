//! Rebuild when a migration is added: `sqlx::migrate!` embeds the files that
//! exist at compile time, and on stable Rust it cannot tell Cargo to watch the
//! directory for new ones.

fn main() {
    println!("cargo::rerun-if-changed=migrations");
}
