use data_encoding::HEXLOWER;
use rand::RngCore;
use scrypt::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use scrypt::{Params, Scrypt};
use sha2::{Digest, Sha256};

use crate::db::Db;

pub const SESSION_TTL_SECS: i64 = 7 * 24 * 60 * 60;
const PASSWORD_HASH_KEY: &str = "password_hash";
const TOKEN_BYTES: usize = 32;

/// scrypt work factor. The crate's `recommended` (log2_n = 17, ~128 MiB) is
/// tuned for release-mode server hardware and takes ~10s in a debug build,
/// making local dev and login miserable. log2_n = 15 (32 MiB, memory-hard —
/// still far stronger than bcrypt) is the right balance for a single-user
/// personal tool that also sits behind an SSO gateway: ~1s in debug,
/// sub-300ms in release. r = 8, p = 1, 32-byte output.
fn scrypt_params() -> Params {
    Params::new(15, 8, 1, 32).expect("valid scrypt params")
}

pub fn password_is_set(db: &Db) -> anyhow::Result<bool> {
    Ok(db.get_setting(PASSWORD_HASH_KEY)?.is_some())
}

pub fn set_password(db: &Db, password: &str) -> anyhow::Result<()> {
    anyhow::ensure!(password.len() >= 8, "password must be at least 8 characters");
    let salt = SaltString::generate(&mut rand08());
    let hash = Scrypt
        .hash_password_customized(
            password.as_bytes(),
            None,
            None,
            scrypt_params(),
            &salt,
        )
        .map_err(|err| anyhow::anyhow!("hashing password: {err}"))?
        .to_string();
    db.set_setting(PASSWORD_HASH_KEY, &hash)
}

pub fn verify_password(db: &Db, password: &str) -> anyhow::Result<bool> {
    let Some(stored) = db.get_setting(PASSWORD_HASH_KEY)? else {
        return Ok(false);
    };
    let parsed = PasswordHash::new(&stored)
        .map_err(|err| anyhow::anyhow!("stored password hash is corrupt: {err}"))?;
    Ok(Scrypt.verify_password(password.as_bytes(), &parsed).is_ok())
}

/// Create a session, returning the raw token (never stored; only its hash is).
pub fn create_session(db: &Db) -> anyhow::Result<String> {
    let mut bytes = [0u8; TOKEN_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    let token = HEXLOWER.encode(&bytes);
    db.insert_auth_session(&hash_token(&token), SESSION_TTL_SECS)?;
    Ok(token)
}

pub fn validate_token(db: &Db, token: &str) -> bool {
    db.auth_session_valid(&hash_token(token)).unwrap_or(false)
}

fn hash_token(token: &str) -> String {
    HEXLOWER.encode(&Sha256::digest(token.as_bytes()))
}

/// scrypt's password-hash API wants a rand_core 0.6 CryptoRngCore; adapt the
/// rand 0.9 OS rng to it.
fn rand08() -> impl scrypt::password_hash::rand_core::CryptoRngCore {
    struct OsRngAdapter;
    impl scrypt::password_hash::rand_core::RngCore for OsRngAdapter {
        fn next_u32(&mut self) -> u32 {
            let mut b = [0u8; 4];
            self.fill_bytes(&mut b);
            u32::from_le_bytes(b)
        }
        fn next_u64(&mut self) -> u64 {
            let mut b = [0u8; 8];
            self.fill_bytes(&mut b);
            u64::from_le_bytes(b)
        }
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            rand::rng().fill_bytes(dest);
        }
        fn try_fill_bytes(
            &mut self,
            dest: &mut [u8],
        ) -> Result<(), scrypt::password_hash::rand_core::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }
    impl scrypt::password_hash::rand_core::CryptoRng for OsRngAdapter {}
    OsRngAdapter
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_lifecycle() {
        let db = Db::open_in_memory().unwrap();
        assert!(!password_is_set(&db).unwrap());
        assert!(set_password(&db, "short").is_err()); // < 8 chars rejected

        set_password(&db, "correct horse battery").unwrap();
        assert!(password_is_set(&db).unwrap());
        assert!(verify_password(&db, "correct horse battery").unwrap());
        assert!(!verify_password(&db, "wrong password").unwrap());
    }

    #[test]
    fn password_hash_uses_tuned_params_not_the_slow_default() {
        let db = Db::open_in_memory().unwrap();
        set_password(&db, "a strong enough password").unwrap();
        let stored = db.get_setting(PASSWORD_HASH_KEY).unwrap().unwrap();
        // PHC scrypt encodes ln,r,p — log2_n must be our 15, not the crate
        // default 17 (which makes debug login ~10s).
        assert!(
            stored.contains("ln=15,r=8,p=1"),
            "unexpected scrypt params in hash: {stored}"
        );
    }

    #[test]
    fn token_lifecycle() {
        let db = Db::open_in_memory().unwrap();
        let token = create_session(&db).unwrap();
        assert_eq!(token.len(), TOKEN_BYTES * 2); // hex encoding doubles length
        assert!(validate_token(&db, &token));
        assert!(!validate_token(&db, "forged-token"));
        // Only the hash is stored — the raw token must not validate as a hash.
        assert!(!db.auth_session_valid(&token).unwrap());
    }
}
