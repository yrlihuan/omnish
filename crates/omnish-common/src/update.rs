use std::path::{Path, PathBuf};

/// Maximum number of update packages to keep per platform.
pub const MAX_CACHED_PACKAGES: usize = 3;

/// Remove old update packages in `dir`, keeping the latest `keep` versions.
/// Package filenames must match `omnish-{version}-{os}-{arch}.tar.gz`.
pub fn prune_packages(dir: &Path, os: &str, arch: &str, keep: usize) {
    let suffix = format!("-{}-{}.tar.gz", os, arch);
    let mut packages: Vec<(String, PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("omnish-") && name.ends_with(&suffix) && !name.starts_with(".tmp-") {
                if let Some(version) = extract_version(&name, os, arch) {
                    packages.push((version, entry.path()));
                }
            }
        }
    }
    // Sort by version descending (highest first)
    packages.sort_by(|a, b| compare_versions(&b.0, &a.0));
    for (_ver, path) in packages.into_iter().skip(keep) {
        let _ = std::fs::remove_file(&path);
    }
}

/// Extract version string from a package filename like `omnish-0.8.4-linux-x86_64.tar.gz`.
pub fn extract_version(name: &str, os: &str, arch: &str) -> Option<String> {
    let prefix = "omnish-";
    let suffix = format!("-{}-{}.tar.gz", os, arch);
    if name.starts_with(prefix) && name.ends_with(&suffix) {
        let ver = &name[prefix.len()..name.len() - suffix.len()];
        if !ver.is_empty() {
            return Some(ver.to_string());
        }
    }
    None
}

/// Normalize a version string for comparison: strip git hash suffix, replace `-` with `.`.
pub fn normalize_version(v: &str) -> String {
    let v = if let Some(pos) = v.rfind("-g") {
        let suffix = &v[pos + 2..];
        if suffix.chars().all(|c| c.is_ascii_hexdigit()) {
            &v[..pos]
        } else {
            v
        }
    } else {
        v
    };
    v.replace('-', ".")
}

/// Compare two version strings using semver-like ordering.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let a_norm = normalize_version(a);
    let b_norm = normalize_version(b);
    let a_parts: Vec<&str> = a_norm.split('.').collect();
    let b_parts: Vec<&str> = b_norm.split('.').collect();
    for (ap, bp) in a_parts.iter().zip(b_parts.iter()) {
        match (ap.parse::<u64>(), bp.parse::<u64>()) {
            (Ok(an), Ok(bn)) => match an.cmp(&bn) {
                std::cmp::Ordering::Equal => continue,
                ord => return ord,
            },
            _ => match ap.cmp(bp) {
                std::cmp::Ordering::Equal => continue,
                ord => return ord,
            },
        }
    }
    a_parts.len().cmp(&b_parts.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_version() {
        assert_eq!(extract_version("omnish-0.8.4-linux-x86_64.tar.gz", "linux", "x86_64"), Some("0.8.4".to_string()));
        assert_eq!(extract_version("omnish-0.8.4-1-g1234abc-linux-x86_64.tar.gz", "linux", "x86_64"), Some("0.8.4-1-g1234abc".to_string()));
        assert_eq!(extract_version("other-file.tar.gz", "linux", "x86_64"), None);
    }

    #[test]
    fn test_compare_versions() {
        assert_eq!(compare_versions("0.8.4", "0.8.3"), std::cmp::Ordering::Greater);
        assert_eq!(compare_versions("0.8.4", "0.8.4"), std::cmp::Ordering::Equal);
        assert_eq!(compare_versions("0.8.4", "0.9.0"), std::cmp::Ordering::Less);
    }

    #[test]
    fn test_normalize_version() {
        assert_eq!(normalize_version("0.8.4-1-g1234abc"), "0.8.4.1");
        assert_eq!(normalize_version("0.8.4"), "0.8.4");
    }

    #[test]
    fn test_prune_packages() {
        let dir = tempfile::tempdir().unwrap();
        let os = "linux";
        let arch = "x86_64";
        // Create 5 fake packages
        for v in &["0.8.1", "0.8.2", "0.8.3", "0.8.4", "0.8.5"] {
            std::fs::write(dir.path().join(format!("omnish-{}-{}-{}.tar.gz", v, os, arch)), "").unwrap();
        }
        prune_packages(dir.path(), os, arch, 3);
        let remaining: Vec<String> = std::fs::read_dir(dir.path()).unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(remaining.len(), 3);
        assert!(remaining.contains(&"omnish-0.8.5-linux-x86_64.tar.gz".to_string()));
        assert!(remaining.contains(&"omnish-0.8.4-linux-x86_64.tar.gz".to_string()));
        assert!(remaining.contains(&"omnish-0.8.3-linux-x86_64.tar.gz".to_string()));
    }
}
