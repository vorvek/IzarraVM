// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use izarravm_core::{AppConfig, VideoCard};
use std::str::FromStr;

#[test]
fn canonical_name_and_migration_aliases_all_select_vega() {
    for name in [
        "vega",
        "et4000ax",
        "et4000_ax",
        "s3virgedx",
        "s3_virge_dx",
        "distira",
        "voodoo1",
        "voodoo2",
    ] {
        assert_eq!(VideoCard::from_str(name).unwrap(), VideoCard::Vega);

        let config: AppConfig =
            toml::from_str(&format!("[machine]\nvideo = \"{name}\"\n")).unwrap();
        assert_eq!(config.machine.video, VideoCard::Vega);
        assert!(
            toml::to_string(&config)
                .unwrap()
                .contains("video = \"vega\"")
        );
    }

    assert_eq!(VideoCard::Vega.to_string(), "vega");
}

#[test]
fn compatibility_constants_are_values_not_fake_choices() {
    for card in [
        VideoCard::Et4000Ax,
        VideoCard::S3VirgeDx,
        VideoCard::Distira,
        VideoCard::Voodoo2,
    ] {
        assert_eq!(card, VideoCard::Vega);
        assert_eq!(card.to_string(), "vega");
        assert_eq!(toml::Value::try_from(card).unwrap().as_str(), Some("vega"));
    }
}

#[test]
fn internal_adapter_names_are_not_selectable_video_cards() {
    assert!(VideoCard::from_str("distira1").is_err());
    assert!(VideoCard::from_str("distira2").is_err());
    assert!(VideoCard::from_str("bigdistira").is_err());
    assert!(VideoCard::from_str("smalldistira").is_err());
    assert!(VideoCard::from_str("obsidian_sb50_amethyst").is_err());
}
