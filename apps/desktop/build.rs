//! Compiles the Slint UI in `ui/` into Rust at build time.

fn main() {
    slint_build::compile("ui/app.slint").expect("compile Slint UI");
}
