use std::env;
use std::fs;
use std::path::PathBuf;
use tracing::debug;

/// Integration with systemd-creds: reads encrypted/decrypted service credentials.
/// When systemd manages a service using LoadCredential= or LoadCredentialEncrypted=,
/// the decrypted credentials are placed in the directory defined by $CREDENTIALS_DIRECTORY.
///
/// Load a plaintext credential by name from $CREDENTIALS_DIRECTORY.
pub fn load_credential(name: &str) -> Option<String> {
    let creds_dir = env::var("CREDENTIALS_DIRECTORY").ok()?;
    let path = PathBuf::from(creds_dir).join(name);
    match fs::read_to_string(&path) {
        Ok(content) => {
            debug!("Loaded systemd credential '{}' from {:?}", name, path);
            Some(content.trim_end_matches(['\r', '\n']).to_string())
        }
        Err(_) => None,
    }
}

/// Load raw credential bytes by name from $CREDENTIALS_DIRECTORY.
#[allow(dead_code)]
pub fn load_credential_bytes(name: &str) -> Option<Vec<u8>> {
    let creds_dir = env::var("CREDENTIALS_DIRECTORY").ok()?;
    let path = PathBuf::from(creds_dir).join(name);
    fs::read(&path).ok()
}

/// Resolve a configuration secret, preferring $CREDENTIALS_DIRECTORY (systemd-creds)
/// and falling back to a standard environment variable.
pub fn resolve_secret(env_var: &str, cred_name: &str) -> Option<String> {
    load_credential(cred_name).or_else(|| env::var(env_var).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_load_credential_unset_env() {
        let _guard = LOCK.lock().unwrap();
        env::remove_var("CREDENTIALS_DIRECTORY");
        assert_eq!(load_credential("nonexistent"), None);
    }

    #[test]
    fn test_load_credential_lifecycle() {
        let _guard = LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        let cred_path = dir.path().join("api_token");
        fs::write(&cred_path, "super_secret_token_12345\n").unwrap();

        env::set_var("CREDENTIALS_DIRECTORY", dir.path());
        assert_eq!(
            load_credential("api_token"),
            Some("super_secret_token_12345".to_string())
        );

        assert_eq!(
            resolve_secret("FALLBACK_TOKEN", "api_token"),
            Some("super_secret_token_12345".to_string())
        );

        env::remove_var("CREDENTIALS_DIRECTORY");
    }

    #[test]
    fn test_resolve_secret_fallback_to_env() {
        let _guard = LOCK.lock().unwrap();
        env::remove_var("CREDENTIALS_DIRECTORY");
        env::set_var("TEST_FALLBACK_KEY", "env_secret_value");
        assert_eq!(
            resolve_secret("TEST_FALLBACK_KEY", "missing_cred"),
            Some("env_secret_value".to_string())
        );
        env::remove_var("TEST_FALLBACK_KEY");
    }
}
