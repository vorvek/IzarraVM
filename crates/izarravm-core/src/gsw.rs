// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::clock::ClockRate;
use super::{ConfigError, normalize};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum GswMode {
    Gsw386Slow,
    #[default]
    Gsw386,
    Gsw486,
    Gsw586,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CpuPersona {
    I386,
    I486,
    I586,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum L1Cache {
    None,
    Unified { kib: u16 },
    Split { instruction_kib: u16, data_kib: u16 },
}

impl L1Cache {
    pub const fn total_kib(self) -> u16 {
        match self {
            Self::None => 0,
            Self::Unified { kib } => kib,
            Self::Split {
                instruction_kib,
                data_kib,
            } => instruction_kib + data_kib,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheGeometry {
    pub l1: L1Cache,
    /// External cache in KiB. This is the motherboard cache on a 386 and the
    /// L2 cache on the 486 and 586 profiles.
    pub external_kib: u16,
}

impl CacheGeometry {
    pub const fn compatibility_kib(self) -> (u16, u16) {
        (self.l1.total_kib(), self.external_kib)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GswModeSpec {
    pub mode: GswMode,
    pub canonical_name: &'static str,
    pub rank: u8,
    pub register_code: u8,
    pub clock: ClockRate,
    pub persona: CpuPersona,
    pub cache: CacheGeometry,
}

pub const GSW_MODE_SPECS: [GswModeSpec; 4] = [
    GswModeSpec {
        mode: GswMode::Gsw386Slow,
        canonical_name: "386-slow",
        rank: 0,
        register_code: 3,
        clock: ClockRate::new(22_000_000, 3),
        persona: CpuPersona::I386,
        cache: CacheGeometry {
            l1: L1Cache::None,
            external_kib: 64,
        },
    },
    GswModeSpec {
        mode: GswMode::Gsw386,
        canonical_name: "386",
        rank: 1,
        register_code: 0,
        clock: ClockRate::from_hz(22_000_000),
        persona: CpuPersona::I386,
        cache: CacheGeometry {
            l1: L1Cache::None,
            external_kib: 64,
        },
    },
    GswModeSpec {
        mode: GswMode::Gsw486,
        canonical_name: "486",
        rank: 2,
        register_code: 1,
        clock: ClockRate::from_hz(66_000_000),
        persona: CpuPersona::I486,
        cache: CacheGeometry {
            l1: L1Cache::Unified { kib: 8 },
            external_kib: 256,
        },
    },
    GswModeSpec {
        mode: GswMode::Gsw586,
        canonical_name: "586",
        rank: 3,
        register_code: 2,
        clock: ClockRate::from_hz(200_000_000),
        persona: CpuPersona::I586,
        cache: CacheGeometry {
            l1: L1Cache::Split {
                instruction_kib: 16,
                data_kib: 16,
            },
            external_kib: 512,
        },
    },
];

impl GswMode {
    pub const fn spec(self) -> &'static GswModeSpec {
        match self {
            Self::Gsw386Slow => &GSW_MODE_SPECS[0],
            Self::Gsw386 => &GSW_MODE_SPECS[1],
            Self::Gsw486 => &GSW_MODE_SPECS[2],
            Self::Gsw586 => &GSW_MODE_SPECS[3],
        }
    }

    pub const fn from_rank(rank: u8) -> Option<Self> {
        let mut index = 0;
        while index < GSW_MODE_SPECS.len() {
            let spec = &GSW_MODE_SPECS[index];
            if spec.rank == rank {
                return Some(spec.mode);
            }
            index += 1;
        }
        None
    }

    pub const fn from_register_code(code: u8) -> Option<Self> {
        let mut index = 0;
        while index < GSW_MODE_SPECS.len() {
            let spec = &GSW_MODE_SPECS[index];
            if spec.register_code == code {
                return Some(spec.mode);
            }
            index += 1;
        }
        None
    }

    pub const fn canonical_name(self) -> &'static str {
        self.spec().canonical_name
    }

    pub const fn rank(self) -> u8 {
        self.spec().rank
    }

    pub const fn register_code(self) -> u8 {
        self.spec().register_code
    }

    pub const fn clock_rate(self) -> ClockRate {
        self.spec().clock
    }

    pub const fn persona(self) -> CpuPersona {
        self.spec().persona
    }

    pub const fn cache_geometry(self) -> CacheGeometry {
        self.spec().cache
    }

    /// Compatibility shim for downstream code that has not migrated to
    /// [`ClockRate`]. Remove it in the next timing migration.
    pub const fn clock_hz(self) -> u64 {
        self.clock_rate().floor_hz()
    }

    /// Compatibility shim for downstream code that still consumes aggregate
    /// `(L1 KiB, external/L2 KiB)` values. Remove it in the next cache migration.
    pub const fn cache_kb(self) -> (u16, u16) {
        self.cache_geometry().compatibility_kib()
    }
}

impl GswMode {
    /// Whether the current batching policy uses the higher-throughput timing
    /// path retained for the 486 and 586 personas.
    pub const fn uses_approximate_timing(self) -> bool {
        matches!(self.persona(), CpuPersona::I486 | CpuPersona::I586)
    }
}

impl fmt::Display for GswMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.canonical_name())
    }
}

impl FromStr for GswMode {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match normalize(value).as_str() {
            "286" | "80286" | "i286" | "gsw286" => Err(ConfigError::RemovedCpu286),
            "386" | "gsw386" | "386dx25" | "i386dx25" | "i386dx_25" | "386_25" => Ok(Self::Gsw386),
            "386slow" | "slow" | "gsw386slow" => Ok(Self::Gsw386Slow),
            "486" | "gsw486" | "486dx266" | "i486dx266" | "i486dx2_66" | "486dx2_66" => {
                Ok(Self::Gsw486)
            }
            "586" | "gsw586" => Ok(Self::Gsw586),
            _ => Err(ConfigError::UnknownPreset {
                kind: "CPU",
                value: value.to_owned(),
            }),
        }
    }
}

impl Serialize for GswMode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.canonical_name())
    }
}

impl<'de> Deserialize<'de> for GswMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

#[cfg(test)]
#[path = "gsw_test.rs"]
mod tests;
