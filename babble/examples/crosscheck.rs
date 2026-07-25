//! Reproduce, on the host, the completion the on-target kvetch server served —
//! the cross-engine determinism claim, checked by hand before it becomes the
//! `kvetch-babble-serves` scenario's assertion.
//! `cargo run -p babble --example crosscheck`

fn main() {
    const PREFIX: &str = "greet(name) {";
    const CAP: usize = 256;
    let seed = kvetch_proto::request_seed(0, 0);

    let mut buf = vec![0u8; CAP];
    buf[..PREFIX.len()].copy_from_slice(PREFIX.as_bytes());
    let reply = babble::serve::handle_request(&mut buf, PREFIX.len(), 8, seed);

    let completion = &buf[PREFIX.len()..PREFIX.len() + reply.written as usize];
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in completion {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }

    println!("seed      {seed} (as i64: {})", seed as i64);
    println!("written   {}", reply.written);
    println!("checksum  {}", (hash >> 1) as i64);
    println!("text      {:?}", core::str::from_utf8(completion).unwrap());
}
