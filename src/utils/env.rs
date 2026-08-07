use std::env;

use crate::log;

const ENV_FILE: &str = ".env";
const ENV_KEY: &str = "TUNGSTEN_API_KEY";
const GLOBAL_VAR: &str = "TUNGSTEN_GLOBAL_APIKEY";

pub fn resolve_api_key(flag: Option<String>) -> Option<String> {
    // Explicit flag
    if flag.is_some() {
        log!(debug, "API key resolved from CLI flag");
        return flag;
    }

    // Local ENV file
    if let Ok(contents) = std::fs::read_to_string(ENV_FILE) {
        for line in contents.lines() {
            if let Some(val) = line.strip_prefix(ENV_KEY) {
                let val = val.trim().to_string();
                if !val.is_empty() && val.starts_with('=') {
                    let val = val[1..].trim().to_string();
                    if !val.is_empty() {
                        log!(debug, "API key resolved from {}", ENV_FILE);
                        return Some(val);
                    }
                }
            }
        }
    }

    // Global system env var
    match env::var(GLOBAL_VAR).ok() {
        Some(key) => {
            log!(debug, "API key resolved from {}", GLOBAL_VAR);
            Some(key)
        }
        None => {
            log!(
                debug,
                "No API key found (no CLI flag, no {}, no {})",
                ENV_FILE,
                GLOBAL_VAR
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_global_env_var() {
        // Backup existing .env if present so CI repo files don't interfere with this test
        let mut backed_up = false;
        if fs::metadata(".env").is_ok() {
            fs::rename(".env", ".env.backup").unwrap();
            backed_up = true;
        }

        unsafe { std::env::set_var(GLOBAL_VAR, "test_global_key") };
        let result = resolve_api_key(None);
        unsafe { std::env::remove_var(GLOBAL_VAR) };
        assert_eq!(result, Some("test_global_key".to_string()));

        // Restore .env if we backed it up
        if backed_up {
            fs::rename(".env.backup", ".env").unwrap();
        }
    }

    #[test]
    fn test_env_file_parsing() {
        // Create a temporary .env file
        let test_content = "TUNGSTEN_API_KEY=test_key_from_env\nOTHER_VAR=value";
        fs::write(".env.test", test_content).unwrap();

        // Temporarily rename for test
        if fs::metadata(".env").is_ok() {
            fs::rename(".env", ".env.backup").unwrap();
        }
        fs::rename(".env.test", ".env").unwrap();

        let result = resolve_api_key(None);
        assert_eq!(result, Some("test_key_from_env".to_string()));

        // Cleanup
        fs::rename(".env", ".env.test").unwrap();
        if fs::metadata(".env.backup").is_ok() {
            fs::rename(".env.backup", ".env").unwrap();
        }
    }
}
