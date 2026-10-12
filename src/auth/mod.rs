pub mod handlers;
pub mod middleware;
pub mod password;
pub mod permissions;
pub mod routes;
pub mod service;
pub mod totp;

use crate::error::ApiResult;
use crate::state::AppState;
use crate::users::repo::{self, NewUser};
use argon2::password_hash::SaltString;
use argon2::password_hash::rand_core::OsRng;

pub async fn bootstrap_admin(state: &AppState) -> ApiResult<()> {
    let (Some(email), Some(password)) = (&state.cfg.admin_email, &state.cfg.admin_password) else {
        return Ok(());
    };

    if repo::find_by_email(&state.pool, email).await?.is_some() {
        return Ok(());
    }

    let salt = SaltString::generate(&mut OsRng);
    let hash = password::hash_password(password, &salt)?;
    let username = email.split('@').next().unwrap_or("admin").to_string();
    let user = repo::insert(
        &state.pool,
        NewUser {
            id: crate::utils::new_id(),
            username: &username,
            email,
            display_name: "Administrator",
            password_hash: &hash,
            role: crate::users::models::Role::Admin,
        },
    )
    .await?;
    tracing::info!(user_id = %user.id, "bootstrap admin created");
    Ok(())
}
