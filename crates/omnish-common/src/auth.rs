use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::config::omnish_dir;

const TOKEN_BYTES: usize = 32;

/// Return the default auth token path: ~/.omnish/auth_token
pub fn default_token_path() -> PathBuf {
    omnish_dir().join("auth_token")
}

/// Load existing token from file, or generate a new one if it doesn't exist.
/// The file is created with permission 0600.
pub fn load_or_create_token(path: &Path) -> Result<String> {
    if path.exists() {
        let token = std::fs::read_to_string(path)?.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }

    let token = generate_token();

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(path, &token)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }

    Ok(token)
}

fn generate_token() -> String {
    use rand::Rng;
    let bytes: [u8; TOKEN_BYTES] = rand::thread_rng().gen();
    hex::encode(bytes)
}

/// Load token from file. Returns error if file doesn't exist or is empty.
pub fn load_token(path: &Path) -> Result<String> {
    let token = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read auth token from {}: {}", path.display(), e))?
        .trim()
        .to_string();
    if token.is_empty() {
        anyhow::bail!("auth token file {} is empty", path.display());
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_token_length() {
        let token = generate_token();
        assert_eq!(token.len(), TOKEN_BYTES * 2);
    }

    #[test]
    fn test_generate_token_unique() {
        let t1 = generate_token();
        let t2 = generate_token();
        assert_ne!(t1, t2);
    }

    #[test]
    fn test_load_or_create_token_creates_new() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth_token");
        let token = load_or_create_token(&path).unwrap();
        assert_eq!(token.len(), TOKEN_BYTES * 2);
        assert!(path.exists());
    }

    #[test]
    fn test_load_or_create_token_reuses_existing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth_token");
        let token1 = load_or_create_token(&path).unwrap();
        let token2 = load_or_create_token(&path).unwrap();
        assert_eq!(token1, token2);
    }

    #[test]
    fn test_load_token_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent");
        assert!(load_token(&path).is_err());
    }
}
