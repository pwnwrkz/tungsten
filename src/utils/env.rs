use std::env;

const ENV_FILE: &str = "tungsten_api_key.env";
const GLOBAL_VAR: &str = "TUNGSTEN_GLOBAL_APIKEY";

pub fn resolve_api_key(flag: Option<String>) -> Option<String> {
    // Explicit flag
    if flag.is_some() {
        return flag;
    }

    // Local ENV file
    if let Ok(contents) = std::fs::read_to_string(ENV_FILE) {
        for line in contents.lines() {
            if let Some(val) = line.strip_prefix("API_KEY=") {
                let val = val.trim().to_string();
                if !val.is_empty() {
                    return Some(val);
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

    #[test]
    fn test_global_env_var() {
        unsafe { std::env::set_var(GLOBAL_VAR, "test_global_key") };
        let result = resolve_api_key(None);
        unsafe { std::env::remove_var(GLOBAL_VAR) };
        assert_eq!(result, Some("test_global_key".to_string()));
    }
}
