// The modules live in the library crate (src/lib.rs). Declaring them again here
// with `mod` would compile a second, incompatible copy of every type into this
// binary, so this file only ever `use`s them.

#[tokio::main]
async fn main() {
    println!("E2EE Chat App");
}
