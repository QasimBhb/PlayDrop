//! The `uniffi-bindgen` command for this workspace. Library mode is the only supported way:
//!
//! ```text
//! cargo build -p playdrop-ffi --release
//! cargo run -p uniffi-bindgen -- generate --library target/release/libplaydrop_ffi.so \
//!     --language kotlin --out-dir apps/android/app/build/generated/uniffi
//! ```

fn main() {
    uniffi::uniffi_bindgen_main();
}
