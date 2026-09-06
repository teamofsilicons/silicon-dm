//! Rebuild embedded migration metadata when the migration directory changes.

fn main() {
    // sqlx::migrate! embeds the directory. Cargo must rebuild when files are
    // added or removed as well as when an existing migration changes.
    println!("cargo:rerun-if-changed=migrations");
}
