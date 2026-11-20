//! `matter_clusters::gen` stays a working alias of `matter_clusters::clusters`
//! for edition 2015–2021 code (the module was renamed in 0.6.0 because `gen`
//! is a reserved keyword from edition 2024). This crate is edition 2021.

#![allow(clippy::unwrap_used)]

#[test]
fn the_gen_alias_names_the_same_items_as_clusters() {
    use matter_clusters::{clusters, gen};
    assert_eq!(gen::on_off::CLUSTER_ID, clusters::on_off::CLUSTER_ID);
    // The same function, not a copy: identical function pointers.
    let via_gen: fn() -> Vec<u8> = gen::on_off::encode_toggle;
    let via_clusters: fn() -> Vec<u8> = clusters::on_off::encode_toggle;
    assert_eq!(via_gen(), via_clusters());
    assert_eq!(
        gen::on_off::decode_on_time(&clusters::on_off::encode_on_time(30)).unwrap(),
        30
    );
}
