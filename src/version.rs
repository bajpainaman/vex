//===- version.rs - Vx Package Tool ----------------------------*- Rust -*-===//
//
// Part of the Vx Project, under the Apache License v2.0 with LLVM Exceptions.
// See LICENSE for license information.
// SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
//
//===----------------------------------------------------------------------===//
//
// Package versions, written `MAJOR.MINOR.PATCH`, and which versions can stand in for each other.
//
//===----------------------------------------------------------------------===//
use std::fmt;
use std::str::FromStr;

use anyhow::{bail, Context, Error, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A version is written `MAJOR.MINOR.PATCH`.
const VERSION_PART_COUNT: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// Read a version written as `MAJOR.MINOR.PATCH`.
    ///
    /// # Errors
    ///
    /// If `text` is not three numbers with dots between them.
    pub fn parse(text: &str) -> Result<Self> {
        let parts: Vec<&str> = text.split('.').collect();
        if parts.len() != VERSION_PART_COUNT {
            bail!("version `{text}` must be three numbers, like `1.2.3`");
        }
        let number = |part: &str| -> Result<u64> {
            // `u64::from_str` accepts a leading `+`, which is not part of a version.
            if part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()) {
                bail!("version `{text}` must be three numbers, like `1.2.3`");
            }
            part.parse::<u64>()
                .with_context(|| format!("version `{text}` has a number that is too large"))
        };
        Ok(Self {
            major: number(parts[0])?,
            minor: number(parts[1])?,
            patch: number(parts[2])?,
        })
    }

    /// The part of the version that must match for one version to replace another: the major
    /// number, or the minor number too while the major number is 0, as in semantic versioning.
    /// Two versions with different lines are different APIs, and a build holds only one.
    pub fn compatibility_line(self) -> (u64, u64) {
        if self.major == 0 {
            (0, self.minor)
        } else {
            (self.major, 0)
        }
    }

    /// The line written for a person: `1.x` or `0.4.x`.
    pub fn compatibility_line_name(self) -> String {
        if self.major == 0 {
            format!("0.{}.x", self.minor)
        } else {
            format!("{}.x", self.major)
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl FromStr for Version {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self> {
        Self::parse(text)
    }
}

impl Serialize for Version {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).expect("a valid version")
    }

    #[test]
    fn a_version_must_be_three_numbers() {
        for text in ["1.2", "1.2.3.4", "1.x.3", "1..3", "+1.2.3", ""] {
            let error = format!("{:#}", Version::parse(text).expect_err(text));
            assert!(error.contains("must be three numbers"), "{text}: {error}");
        }
    }

    #[test]
    fn versions_compare_number_by_number() {
        assert!(version("1.10.0") > version("1.9.9"));
        assert!(version("2.0.0") > version("1.99.99"));
    }

    #[test]
    fn versions_with_the_same_major_number_can_replace_each_other() {
        assert_eq!(
            version("1.2.3").compatibility_line(),
            version("1.9.0").compatibility_line()
        );
        assert_ne!(
            version("1.2.3").compatibility_line(),
            version("2.0.0").compatibility_line()
        );
    }

    #[test]
    fn below_1_0_0_the_minor_number_must_match_too() {
        assert_eq!(
            version("0.4.1").compatibility_line(),
            version("0.4.7").compatibility_line()
        );
        assert_ne!(
            version("0.4.1").compatibility_line(),
            version("0.5.0").compatibility_line()
        );
        assert_eq!(version("0.4.1").compatibility_line_name(), "0.4.x");
        assert_eq!(version("3.4.1").compatibility_line_name(), "3.x");
    }
}
