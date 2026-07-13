use std::env;

const ENV_FILE: &str = ".env";
const ENV_KEY: &str = "TUNGSTEN_API_KEY";
const GLOBAL_VAR: &str = "TUNGSTEN_GLOBAL_APIKEY";

pub fn resolve_api_key(flag: Option<String>) -> Option<String> {
    // Explicit flag
    if flag.is_some() {
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
                        return Some(val);
                    }
                }
            }
        }
    }

    // Global system env var
    env::var(GLOBAL_VAR).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_global_env_var() {
        unsafe { std::env::set_var(GLOBAL_VAR, "test_global_key") };
        let result = resolve_api_key(None);
        unsafe { std::env::remove_var(GLOBAL_VAR) };
        assert_eq!(result, Some("test_global_key".to_string()));
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
