// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn set_bits(set: &[bool; 256]) -> Vec<u8> {
    (0..=255).filter(|&v| set[usize::from(v)]).collect()
}

#[test]
fn parses_a_single_hex_vector() {
    assert_eq!(set_bits(&parse_vectors("67")), vec![0x67]);
}

#[test]
fn parses_comma_separated_hex_vectors() {
    assert_eq!(set_bits(&parse_vectors("67,21")), vec![0x21, 0x67]);
}

#[test]
fn trims_whitespace_around_tokens() {
    assert_eq!(set_bits(&parse_vectors(" 67 , 21 ")), vec![0x21, 0x67]);
}

#[test]
fn skips_empty_tokens_from_stray_or_trailing_commas() {
    assert_eq!(set_bits(&parse_vectors("67,,21,")), vec![0x21, 0x67]);
}

#[test]
fn skips_a_malformed_token_without_losing_the_rest() {
    // "zz" is not valid hex; it is reported to stderr and otherwise dropped,
    // and does not stop the well-formed vectors around it from taking.
    assert_eq!(set_bits(&parse_vectors("67,zz,21")), vec![0x21, 0x67]);
}

#[test]
fn empty_spec_traces_nothing() {
    assert_eq!(set_bits(&parse_vectors("")), Vec::<u8>::new());
}
