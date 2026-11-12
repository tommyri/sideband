//! `sqlx::migrate!` embeds `migrations/` at compile time, so rebuild the
//! crate when a migration is added or changed (T-114).

fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
