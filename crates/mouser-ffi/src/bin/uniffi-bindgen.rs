//! Thin wrapper exposing uniffi's bindgen CLI for this crate, so foreign-language
//! bindings can be produced with `cargo run --bin uniffi-bindgen -- generate ...`.

fn main() {
    uniffi::uniffi_bindgen_main()
}
