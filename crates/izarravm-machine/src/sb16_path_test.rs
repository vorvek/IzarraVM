// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn disabled_mix_bypasses_opl_and_discards_voice_and_cd() {
    let config = SoundBlasterConfig {
        enabled: false,
        ..SoundBlasterConfig::default()
    };
    let mut path = Sb16Path::new(&config);

    assert_eq!(
        path.mix_snapshot()
            .mix_opl_voice((16_777_217, -16_777_217), (123, 456)),
        (16_777_217, -16_777_217)
    );
    assert_eq!(path.mix_snapshot().mix_cd((32_767, -32_768)), (0, 0));
    assert_eq!(path.cd_levels(), (0, 0));
    path.set_linked_cd_level(31);
    assert_eq!(path.cd_levels(), (0, 0));
}
