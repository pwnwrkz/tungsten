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
