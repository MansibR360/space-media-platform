use crate::error::{ApiError, ApiResult};
use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};

pub fn hash_password(password: &str, salt: &SaltString) -> ApiResult<String> {
    let hashed = Argon2::default()
        .hash_password(password.as_bytes(), salt)
        .map_err(|e| ApiError::internal_msg(format!("password hashing failed: {e}")))?;
    Ok(hashed.to_string())
}

pub fn new_salt() -> SaltString {
    use argon2::password_hash::rand_core::OsRng as HashRng;
    SaltString::generate(&mut HashRng)
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    let parsed = match PasswordHash::new(hash) {
        Ok(parsed) => parsed,
        Err(_) => return false,
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// Cheap complexity gate before Argon2 (which is intentionally slow).
pub fn password_policy_error(password: &str, min_length: usize) -> crate::error::ApiResult<()> {
    use crate::error::ApiError;

    if password.len() < min_length {
        return Err(ApiError::bad_request(format!(
            "password must be at least {min_length} characters"
        )));
    }
    if password.len() > 128 {
        return Err(ApiError::bad_request(
            "password must be at most 128 characters",
        ));
    }

    let has_lower = password.chars().any(|c| c.is_lowercase());
    let has_upper = password.chars().any(|c| c.is_uppercase());
    let has_digit = password.chars().any(|c| c.is_ascii_digit());
    let classes = has_lower as u32 + has_upper as u32 + has_digit as u32;

    if classes < 2 && min_length <= 8 {
        return Err(ApiError::bad_request(
            "password must mix letters and digits",
        ));
    }
    Ok(())
}

pub fn password_meets_policy(password: &str, min_length: usize) -> bool {
    password_policy_error(password, min_length).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_and_verify_roundtrip() {
        let salt = new_salt();
        let hash = hash_password("correct horse battery staple", &salt).unwrap();
        assert!(verify_password("correct horse battery staple", &hash));
        assert!(!verify_password("wrong", &hash));
    }
}
