// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Boolean environment flags.

/// True when a flag value turns the flag on. The empty string, `0`, `off` and
/// `false` turn it off. The test trims white space and ignores ASCII case. Any
/// other value turns the flag on.
pub fn flag_value_is_on(value: &str) -> bool {
    let value = value.trim();
    !(value.is_empty()
        || value == "0"
        || value.eq_ignore_ascii_case("off")
        || value.eq_ignore_ascii_case("false"))
}

/// True when the environment variable `name` is set to a value that turns a
/// flag on. An unset variable is off. A value that is not valid Unicode is
/// read lossily, so it counts as on.
pub fn env_flag(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| flag_value_is_on(&value.to_string_lossy()))
}

#[cfg(test)]
#[path = "env_flag_test.rs"]
mod tests;
