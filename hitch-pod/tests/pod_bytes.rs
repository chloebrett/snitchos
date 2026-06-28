//! `pod_bytes` is the leaf's one `unsafe` block, so it gets its own tests here
//! (the richer struct/round-trip coverage lives in `hitch`, but those run against
//! a different crate and wouldn't catch mutations to this one).

use hitch_pod::pod_bytes;

#[test]
fn pod_bytes_is_the_little_endian_image() {
    assert_eq!(pod_bytes(&[1u32, 0x0203_0405]), &[1, 0, 0, 0, 5, 4, 3, 2]);
}

#[test]
fn pod_bytes_length_is_count_times_size() {
    assert_eq!(pod_bytes(&[0u64; 3]).len(), 24);
    assert_eq!(pod_bytes::<u8>(&[]).len(), 0);
}
