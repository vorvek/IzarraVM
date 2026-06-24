use izarravm_core::VideoCard;
use std::str::FromStr;

#[test]
fn distira_cards_are_parseable_as_vega_glide_variants() {
    assert_eq!(VideoCard::from_str("distira").unwrap(), VideoCard::Distira);
    assert_eq!(VideoCard::from_str("voodoo1").unwrap(), VideoCard::Distira);
    assert_eq!(VideoCard::Distira.to_string(), "distira");
}

#[test]
fn distira_motherboard_chip_names_are_not_selectable_video_cards() {
    assert!(VideoCard::from_str("distira1").is_err());
    assert!(VideoCard::from_str("distira2").is_err());
    assert!(VideoCard::from_str("bigdistira").is_err());
    assert!(VideoCard::from_str("smalldistira").is_err());
    assert!(VideoCard::from_str("obsidian_sb50_amethyst").is_err());
}
