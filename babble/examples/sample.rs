//! Print a few babbled programs — a human sanity check on the walk's shape.
//! `cargo run -p babble --example sample`

fn main() {
    for seed in 0..3u64 {
        println!("--- seed {seed} ---\n{}\n", babble::generate(seed));
    }
}
