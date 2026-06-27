use serde::{Deserialize, Serialize};

pub(crate) const FORMULA_API_URL: &str = "https://formulae.brew.sh/api/formula.json";
pub(crate) const CASK_API_URL: &str = "https://formulae.brew.sh/api/cask.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Formula {
    pub name: String,
    pub full_name: String,
    pub desc: Option<String>,
    pub homepage: String,
    pub versions: Versions,
    #[serde(default)]
    pub revision: u32,
    pub installed: Option<Vec<InstalledVersion>>,
    pub dependencies: Option<Vec<String>>,
    pub build_dependencies: Option<Vec<String>>,
    pub bottle: Option<BottleInfo>,
    #[serde(default)]
    pub deprecated: bool,
    #[serde(default)]
    pub disabled: bool,
    pub deprecation_reason: Option<String>,
    pub disable_reason: Option<String>,
    pub keg_only: Option<bool>,
    pub keg_only_reason: Option<serde_json::Value>,
    #[serde(default)]
    pub post_install_defined: bool,
    /// Path to the local .rb file (set for tap formulae; not serialized).
    #[serde(skip, default)]
    pub rb_path: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BottleInfo {
    pub stable: Option<BottleStable>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BottleStable {
    #[serde(default)]
    pub rebuild: u32,
    pub files: std::collections::HashMap<String, BottleFile>,
}

impl BottleStable {
    /// Resolve the bottle tarball for this OS/arch tag, matching Homebrew JSON keys.
    ///
    /// Linux ARM bottles have appeared as both `arm64_linux` and `aarch64_linux` in
    /// formulae; we accept either when the runtime tag is the other.
    pub fn file_for_platform(&self, platform: &str) -> Option<&BottleFile> {
        self.files
            .get(platform)
            .or_else(|| self.files.get("all"))
            .or_else(|| match platform {
                "arm64_linux" => self.files.get("aarch64_linux"),
                "aarch64_linux" => self.files.get("arm64_linux"),
                _ => None,
            })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BottleFile {
    pub url: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Versions {
    pub stable: String,
    pub bottle: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledVersion {
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cask {
    pub token: String,
    pub full_token: String,
    pub name: Vec<String>,
    pub desc: Option<String>,
    pub homepage: String,
    pub version: String,
    #[serde(default)]
    pub deprecated: bool,
    #[serde(default)]
    pub disabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaskDetails {
    pub token: String,
    pub name: Vec<String>,
    pub desc: Option<String>,
    pub homepage: String,
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub artifacts: Option<Vec<CaskArtifact>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CaskArtifact {
    App {
        app: Vec<serde_json::Value>,
    },
    Pkg {
        pkg: Vec<serde_json::Value>,
    },
    Binary {
        binary: Vec<serde_json::Value>,
    },
    Font {
        font: Vec<serde_json::Value>,
    },
    Manpage {
        manpage: Vec<serde_json::Value>,
    },
    Dictionary {
        dictionary: Vec<serde_json::Value>,
    },
    Colorpicker {
        colorpicker: Vec<serde_json::Value>,
    },
    Prefpane {
        prefpane: Vec<serde_json::Value>,
    },
    Qlplugin {
        qlplugin: Vec<serde_json::Value>,
    },
    ScreenSaver {
        screen_saver: Vec<serde_json::Value>,
    },
    Service {
        service: Vec<serde_json::Value>,
    },
    Suite {
        suite: Vec<serde_json::Value>,
    },
    Artifact {
        artifact: Vec<serde_json::Value>,
    },
    BashCompletion {
        bash_completion: Vec<serde_json::Value>,
    },
    ZshCompletion {
        zsh_completion: Vec<serde_json::Value>,
    },
    FishCompletion {
        fish_completion: Vec<serde_json::Value>,
    },
    Uninstall {
        uninstall: Vec<serde_json::Value>,
    },
    Zap {
        zap: Vec<serde_json::Value>,
    },
    Preflight {
        preflight: Option<String>,
    },
    Postflight {
        postflight: Option<String>,
    },
    Other(serde_json::Value),
}

impl Formula {
    pub fn full_version(&self) -> String {
        if self.revision > 0 {
            format!("{}_{}", self.versions.stable, self.revision)
        } else {
            self.versions.stable.clone()
        }
    }

    pub fn bottle_rebuild(&self) -> u32 {
        self.bottle
            .as_ref()
            .and_then(|b| b.stable.as_ref())
            .map(|s| s.rebuild)
            .unwrap_or(0)
    }
}

#[derive(Debug)]
pub struct FetchResult<T> {
    pub data: Option<T>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub not_modified: bool,
}

#[cfg(test)]
mod bottle_stable_tests {
    use super::*;
    use std::collections::HashMap;

    fn sample_file() -> BottleFile {
        BottleFile {
            url: "https://example.com/bottle.tar.gz".into(),
            sha256: "deadbeef".into(),
        }
    }

    #[test]
    fn file_for_platform_matches_arm64_when_json_has_aarch64_linux() {
        let mut files = HashMap::new();
        files.insert("aarch64_linux".into(), sample_file());
        let stable = BottleStable { rebuild: 0, files };
        let f = stable
            .file_for_platform("arm64_linux")
            .expect("aarch64_linux alias");
        assert_eq!(f.sha256, "deadbeef");
    }

    #[test]
    fn file_for_platform_matches_aarch64_when_json_has_arm64_linux() {
        let mut files = HashMap::new();
        files.insert("arm64_linux".into(), sample_file());
        let stable = BottleStable { rebuild: 0, files };
        let f = stable
            .file_for_platform("aarch64_linux")
            .expect("arm64_linux alias");
        assert_eq!(f.sha256, "deadbeef");
    }

    #[test]
    fn file_for_platform_exact_match() {
        let mut files = HashMap::new();
        files.insert("x86_64_linux".into(), sample_file());
        let stable = BottleStable { rebuild: 0, files };
        let f = stable
            .file_for_platform("x86_64_linux")
            .expect("exact match");
        assert_eq!(f.sha256, "deadbeef");
    }

    #[test]
    fn file_for_platform_fallback_to_all() {
        let mut files = HashMap::new();
        files.insert("all".into(), sample_file());
        let stable = BottleStable { rebuild: 0, files };
        let f = stable
            .file_for_platform("x86_64_linux")
            .expect("fallback to all");
        assert_eq!(f.sha256, "deadbeef");
    }

    #[test]
    fn file_for_platform_no_match_returns_none() {
        let files = HashMap::new();
        let stable = BottleStable { rebuild: 0, files };
        let f = stable.file_for_platform("x86_64_linux");
        assert!(f.is_none());
    }
}

#[cfg(test)]
mod formula_tests {
    use super::*;

    fn create_mock_formula(stable: &str, revision: u32) -> Formula {
        Formula {
            name: "test".into(),
            full_name: "test".into(),
            desc: None,
            homepage: "https://example.com".into(),
            versions: Versions {
                stable: stable.into(),
                bottle: false,
            },
            revision,
            installed: None,
            dependencies: None,
            build_dependencies: None,
            bottle: None,
            deprecated: false,
            disabled: false,
            deprecation_reason: None,
            disable_reason: None,
            keg_only: None,
            keg_only_reason: None,
            post_install_defined: false,
            rb_path: None,
        }
    }

    #[test]
    fn test_full_version_without_revision() {
        let f = create_mock_formula("1.2.3", 0);
        assert_eq!(f.full_version(), "1.2.3");
    }

    #[test]
    fn test_full_version_with_revision() {
        let f = create_mock_formula("1.2.3", 2);
        assert_eq!(f.full_version(), "1.2.3_2");
    }

    fn dummy_formula(bottle: Option<BottleInfo>) -> Formula {
        Formula {
            name: "test-formula".into(),
            full_name: "test-formula".into(),
            desc: None,
            homepage: "https://example.com".into(),
            versions: Versions {
                stable: "1.0.0".into(),
                bottle: bottle.is_some(),
            },
            revision: 0,
            installed: None,
            dependencies: None,
            build_dependencies: None,
            bottle,
            deprecated: false,
            disabled: false,
            deprecation_reason: None,
            disable_reason: None,
            keg_only: None,
            keg_only_reason: None,
            post_install_defined: false,
            rb_path: None,
        }
    }

    #[test]
    fn test_bottle_rebuild_none() {
        let f = dummy_formula(None);
        assert_eq!(f.bottle_rebuild(), 0);
    }

    #[test]
    fn test_bottle_rebuild_no_stable() {
        let f = dummy_formula(Some(BottleInfo { stable: None }));
        assert_eq!(f.bottle_rebuild(), 0);
    }

    #[test]
    fn test_bottle_rebuild_with_rebuild() {
        let f = dummy_formula(Some(BottleInfo {
            stable: Some(BottleStable {
                rebuild: 42,
                files: std::collections::HashMap::new(),
            }),
        }));
        assert_eq!(f.bottle_rebuild(), 42);
    }
}
