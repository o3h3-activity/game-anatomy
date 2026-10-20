//! Offline RetroAchievements-compatible game hashes with automatic platform detection.
//!
//! Supports PlayStation, PlayStation 2, and PlayStation Portable media. The
//! returned MD5 identifies selected game content, not the complete image file.
//! Streaming MD5 uses RustCrypto's `md-5` crate; lowercase encoding uses `hex`.
//! MD5 is required for compatibility, not cryptographic integrity.
//!
//! The CLI accepts exactly one file path, with no platform flags:
//!
//! ```text
//! cargo run --quiet -- "games/Tokyo Xtreme Racer 3.ISO"
//! ```
//!
//! Successful output is only the lowercase MD5 followed by a newline. Errors
//! go to stderr with a nonzero exit status and no hash on stdout.
//! Supply an ISO, BIN, MDF, CUE sheet, or PSP PBP; for MDS/MDF pairs, use the MDF.
//!
//! # Python bindings
//!
//! Build the Linux extension with `make -C bindings/python`. The default target
//! is `x86_64-unknown-linux-gnu` (override with `TARGET=...`); the output is
//! `bindings/python/out/game_anatomy.abi3.so`, compatible with CPython 3.10+.
//! Add that directory to `PYTHONPATH` before importing:
//!
//! ```python
//! from pathlib import Path
//! from game_anatomy import generate_hash
//!
//! info = generate_hash(Path("game.iso"))  # str paths also work
//! md5 = info["identity_md5"]
//! ```
//!
//! The binding releases the GIL while hashing and returns a dictionary:
//!
//! - `platform`: detected slug (`ps1`, `ps2`, or `psp`).
//! - `serial`: raw boot filename or PSP `DISC_ID` text; `None` if unavailable.
//!   Case, punctuation, and text whitespace are preserved; no normalization or
//!   serial-format validation is performed.
//! - `identity_md5`: lowercase, 32-character RetroAchievements-compatible MD5.
//! - `retroachievements_platform_id`: console ID (PS1 = 12, PS2 = 21, PSP = 41),
//!   as defined in [rcheevos](https://github.com/RetroAchievements/rcheevos/blob/master/include/rc_consoles.h).
//!
//! Hashing failures raise `RuntimeError`. The Rust [`generate_hash`] API and
//! CLI remain hash-only; Rust callers can use [`game_info`] for metadata.

mod hash;
mod serial;

use std::fmt;
use std::path::Path;

/// An input, format, detection, or selected-content read failure.
#[derive(Debug)]
pub struct HashError {
    message: String,
}

impl HashError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for HashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HashError {}

impl From<String> for HashError {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

/// Detected game metadata and its RetroAchievements-compatible identity hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameInfo {
    /// Platform slug: `ps1`, `ps2`, or `psp`.
    pub platform: &'static str,
    /// Raw boot filename or PSP `DISC_ID` text, without normalization.
    pub serial: Option<String>,
    /// Lowercase, 32-character MD5 of the platform-selected identity content.
    pub identity_md5: String,
    /// RetroAchievements console ID: PS1 = 12, PS2 = 21, PSP = 41.
    pub retroachievements_platform_id: u32,
}

/// Hash game content and return its detected platform and optional serial.
///
/// Console serials come from the resolved boot executable; PSP serials come
/// from `DISC_ID` in PARAM.SFO (inside the ISO or PBP). Missing or malformed
/// serial metadata yields `None` without preventing identity hashing.
///
/// # Errors
///
/// Returns the same input, detection, and content errors as [`generate_hash`].
pub fn game_info(input: &Path) -> Result<GameInfo, HashError> {
    hash::game_info(input)
}

/// Compute a lowercase, 32-character RetroAchievements-compatible MD5.
///
/// Automatically detects PlayStation, PlayStation 2, or PSP from media contents.
/// Reads only filesystem metadata and content selected by the platform's hash
/// algorithm. Does not scan physical sectors, build reports, extract serials,
/// calculate full-image integrity hashes, or access the network.
///
/// # Errors
///
/// Returns an error for unreadable or unsupported media, ambiguous detection,
/// or missing/truncated selected content.
///
/// ```no_run
/// use game_anatomy::generate_hash;
/// use std::path::Path;
///
/// let md5 = generate_hash(Path::new("game.iso"))?;
/// println!("{md5}");
/// # Ok::<(), game_anatomy::HashError>(())
/// ```
pub fn generate_hash(input: &Path) -> Result<String, HashError> {
    hash::generate_hash(input)
}
