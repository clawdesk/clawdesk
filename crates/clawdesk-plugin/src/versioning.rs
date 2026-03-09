//! Plugin versioning — semantic version constraints and compatibility resolution.
//!
//! Extends the ABI layer with semantic versioning for plugin packages,
//! dependency version constraints, and upgrade compatibility validation.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fmt;

/// Semantic version (major.minor.patch[-pre]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemVer {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    /// Pre-release label (e.g., "alpha.1", "rc.2").
    #[serde(default)]
    pub pre: Option<String>,
}

impl SemVer {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
            pre: None,
        }
    }

    pub fn with_pre(mut self, pre: impl Into<String>) -> Self {
        self.pre = Some(pre.into());
        self
    }

    /// Parse from "major.minor.patch[-pre]" format.
    pub fn parse(s: &str) -> Option<Self> {
        let (version_part, pre) = if let Some((v, p)) = s.split_once('-') {
            (v, Some(p.to_string()))
        } else {
            (s, None)
        };

        let parts: Vec<&str> = version_part.split('.').collect();
        if parts.len() != 3 {
            return None;
        }

        Some(Self {
            major: parts[0].parse().ok()?,
            minor: parts[1].parse().ok()?,
            patch: parts[2].parse().ok()?,
            pre,
        })
    }

    /// Whether this is a pre-release version.
    pub fn is_pre_release(&self) -> bool {
        self.pre.is_some()
    }
}

impl Ord for SemVer {
    fn cmp(&self, other: &Self) -> Ordering {
        self.major
            .cmp(&other.major)
            .then(self.minor.cmp(&other.minor))
            .then(self.patch.cmp(&other.patch))
            .then_with(|| match (&self.pre, &other.pre) {
                (None, None) => Ordering::Equal,
                (Some(_), None) => Ordering::Less, // pre-release < release
                (None, Some(_)) => Ordering::Greater,
                (Some(a), Some(b)) => a.cmp(b),
            })
    }
}

impl PartialOrd for SemVer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for SemVer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(pre) = &self.pre {
            write!(f, "-{}", pre)?;
        }
        Ok(())
    }
}

/// Version constraint for dependency resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VersionConstraint {
    /// Exact version match.
    Exact { version: SemVer },
    /// Minimum version (inclusive): >= version.
    AtLeast { version: SemVer },
    /// Compatible range: same major, >= specified minor.patch.
    /// Equivalent to ^version in Cargo.
    Compatible { version: SemVer },
    /// Range: [min, max).
    Range { min: SemVer, max: SemVer },
}

impl VersionConstraint {
    /// Check if a version satisfies this constraint.
    pub fn satisfied_by(&self, v: &SemVer) -> bool {
        match self {
            Self::Exact { version } => v == version,
            Self::AtLeast { version } => v >= version,
            Self::Compatible { version } => {
                v.major == version.major && (v.minor, v.patch) >= (version.minor, version.patch)
            }
            Self::Range { min, max } => v >= min && v < max,
        }
    }
}

impl fmt::Display for VersionConstraint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exact { version } => write!(f, "={}", version),
            Self::AtLeast { version } => write!(f, ">={}", version),
            Self::Compatible { version } => write!(f, "^{}", version),
            Self::Range { min, max } => write!(f, ">={}, <{}", min, max),
        }
    }
}

/// A versioned plugin dependency declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginDependency {
    /// Plugin ID this depends on.
    pub plugin_id: String,
    /// Version constraint.
    pub constraint: VersionConstraint,
    /// Whether this dependency is optional.
    #[serde(default)]
    pub optional: bool,
}

/// Result of checking upgrade compatibility between two versions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpgradeCompatibility {
    /// Safe upgrade — same major, higher minor/patch.
    Compatible,
    /// Breaking upgrade — different major version.
    Breaking {
        from_major: u32,
        to_major: u32,
    },
    /// Downgrade — target version is lower.
    Downgrade,
    /// Same version — no change needed.
    Same,
}

/// Check upgrade compatibility from one version to another.
pub fn check_upgrade(from: &SemVer, to: &SemVer) -> UpgradeCompatibility {
    match from.cmp(to) {
        Ordering::Equal => UpgradeCompatibility::Same,
        Ordering::Greater => UpgradeCompatibility::Downgrade,
        Ordering::Less => {
            if from.major != to.major {
                UpgradeCompatibility::Breaking {
                    from_major: from.major,
                    to_major: to.major,
                }
            } else {
                UpgradeCompatibility::Compatible
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_parse() {
        let v = SemVer::parse("1.2.3").unwrap();
        assert_eq!(v, SemVer::new(1, 2, 3));

        let v = SemVer::parse("0.5.0-alpha.1").unwrap();
        assert_eq!(v.major, 0);
        assert_eq!(v.pre.as_deref(), Some("alpha.1"));
    }

    #[test]
    fn semver_ordering() {
        let v1 = SemVer::new(1, 0, 0);
        let v2 = SemVer::new(1, 1, 0);
        let v3 = SemVer::new(2, 0, 0);
        assert!(v1 < v2);
        assert!(v2 < v3);

        // Pre-release < release.
        let pre = SemVer::new(1, 0, 0).with_pre("alpha");
        assert!(pre < v1);
    }

    #[test]
    fn compatible_constraint() {
        let c = VersionConstraint::Compatible {
            version: SemVer::new(1, 2, 0),
        };
        assert!(c.satisfied_by(&SemVer::new(1, 2, 0)));
        assert!(c.satisfied_by(&SemVer::new(1, 3, 0)));
        assert!(c.satisfied_by(&SemVer::new(1, 2, 5)));
        assert!(!c.satisfied_by(&SemVer::new(1, 1, 0)));
        assert!(!c.satisfied_by(&SemVer::new(2, 0, 0)));
    }

    #[test]
    fn range_constraint() {
        let c = VersionConstraint::Range {
            min: SemVer::new(1, 0, 0),
            max: SemVer::new(2, 0, 0),
        };
        assert!(c.satisfied_by(&SemVer::new(1, 5, 0)));
        assert!(!c.satisfied_by(&SemVer::new(2, 0, 0))); // exclusive upper bound
        assert!(!c.satisfied_by(&SemVer::new(0, 9, 0)));
    }

    #[test]
    fn upgrade_compatibility() {
        let v1 = SemVer::new(1, 0, 0);
        let v2 = SemVer::new(1, 2, 0);
        let v3 = SemVer::new(2, 0, 0);

        assert_eq!(check_upgrade(&v1, &v2), UpgradeCompatibility::Compatible);
        assert_eq!(
            check_upgrade(&v1, &v3),
            UpgradeCompatibility::Breaking {
                from_major: 1,
                to_major: 2
            }
        );
        assert_eq!(check_upgrade(&v2, &v1), UpgradeCompatibility::Downgrade);
        assert_eq!(check_upgrade(&v1, &v1), UpgradeCompatibility::Same);
    }

    #[test]
    fn dependency_constraint() {
        let dep = PluginDependency {
            plugin_id: "my-plugin".into(),
            constraint: VersionConstraint::AtLeast {
                version: SemVer::new(0, 5, 0),
            },
            optional: false,
        };
        assert!(dep.constraint.satisfied_by(&SemVer::new(0, 5, 0)));
        assert!(dep.constraint.satisfied_by(&SemVer::new(1, 0, 0)));
        assert!(!dep.constraint.satisfied_by(&SemVer::new(0, 4, 9)));
    }
}
