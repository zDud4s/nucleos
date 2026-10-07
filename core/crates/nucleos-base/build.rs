//! Makes a change under `core/migrations/` rebuild the crate.
//!
//! `storage::MIGRATOR` is `sqlx::migrate!("../../migrations")`, which reads the directory at compile
//! time and embeds every file, but on stable Rust it cannot tell cargo it did so
//! (`proc_macro::tracked::path` sits behind `sqlx_macros_unstable`). Without this line a commit that
//! only adds, edits or renames a `.sql` leaves cargo seeing nothing to rebuild, and the daemon and the
//! test suite both run the old list — silently, with no error until something queries a table that
//! was never created.
fn main() {
    println!("cargo:rerun-if-changed=../../migrations");
}
