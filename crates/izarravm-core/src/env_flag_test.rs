// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn flag_values_that_mean_off() {
    for value in [
        "", " ", "0", " 0 ", "off", "OFF", "Off", " off\t", "false", "FALSE", "False", "\n",
    ] {
        assert!(!flag_value_is_on(value), "{value:?} must be off");
    }
}

#[test]
fn flag_values_that_mean_on() {
    for value in [
        "1", "on", "yes", "true", "00", "0x0", "offf", "no", "anything", "-1",
    ] {
        assert!(flag_value_is_on(value), "{value:?} must be on");
    }
}

#[test]
fn an_unset_variable_is_off() {
    assert!(!env_flag("IZARRAVM_TEST_FLAG_THAT_IS_NEVER_SET"));
}
