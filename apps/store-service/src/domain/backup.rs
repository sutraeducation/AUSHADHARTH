//! The `.aushbackup` container.
//!
//! One deterministic version-1 framing, parsed defensively because this is the only place in
//! AUSHADHARTH where a file chosen by a human — possibly the wrong file, possibly a damaged one,
//! possibly one crafted to be unpleasant — is read before anything else is known about it.
//!
//! ```text
//! offset  size  field
//! 0       8     MAGIC           "AUSHBAK\x1A"
//! 8       2     FORMAT_VERSION  u16 big-endian, currently 1
//! 10      2     RESERVED        u16 big-endian, must be 0
//! 12      4     MANIFEST_LEN    u32 big-endian, 1..=65_536
//! 16      8     PAYLOAD_LEN     u64 big-endian, 1..=2_147_483_648
//! 24      M     MANIFEST        UTF-8 JSON
//! 24+M    P     PAYLOAD         SQLite snapshot
//! 24+M+P        EOF             required
//! ```
//!
//! Every length is validated against the file's real size *before* a single byte is allocated, and
//! the payload is never held in memory: a 500 MB backup must cost the same few kilobytes of RAM as
//! a 500 KB one. The `\x1A` in the magic is a courtesy — it stops a curious operator's `type`
//! command from spraying a gigabyte of binary at their console.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom, Write};

/// Identifies the container. Never changes.
pub const MAGIC: &[u8; 8] = b"AUSHBAK\x1A";

/// The only framing this build writes, and the highest it can read.
pub const FORMAT_VERSION: u16 = 1;

/// The fixed header that precedes the manifest.
pub const HEADER_BYTES: u64 = 24;

/// A manifest is small by construction; anything larger is a malformed or hostile file.
pub const MAX_MANIFEST_BYTES: u32 = 65_536;

/// 2 GiB. Derived from measured growth — roughly 150–200 MB a year for a busy counter — with
/// headroom for attachments, rather than chosen because it looked like a round number.
pub const MAX_PAYLOAD_BYTES: u64 = 2_147_483_648;

/// SQLite keeps this at offset 68 of a database header. `0x41555348` is ASCII `AUSH`.
///
/// Databases created before the backup migration carry `0`, and that is legitimate: they are ours,
/// they just predate the marker. Identity for those rests on the migration checksum chain.
pub const APPLICATION_ID: i64 = 0x4155_5348;

/// How many bytes move between disk and hash at a time. Small enough to bound memory on any
/// machine, large enough that a gigabyte does not become a million syscalls.
pub const CHUNK_BYTES: usize = 64 * 1024;

/// Why a container could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainerError {
    /// Shorter than the fixed header, so nothing can be said about it.
    TooSmall,
    /// The magic is absent. Almost certainly not an AUSHADHARTH backup at all.
    NotABackup,
    /// Framing this build does not know how to read.
    UnsupportedFormatVersion(u16),
    /// A declared length is outside its permitted range, or the parts do not fill the file exactly.
    Malformed(&'static str),
    /// The manifest is not UTF-8, is not JSON, or lacks a required field.
    ManifestUnreadable,
    /// The payload's digest does not match the manifest's.
    ChecksumMismatch,
    /// The underlying file could not be read or written.
    Io(String),
}

impl std::fmt::Display for ContainerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooSmall => write!(formatter, "file is too small to be a backup"),
            Self::NotABackup => write!(formatter, "file is not an AUSHADHARTH backup"),
            Self::UnsupportedFormatVersion(version) => {
                write!(
                    formatter,
                    "backup format version {version} is not supported"
                )
            }
            Self::Malformed(what) => write!(formatter, "backup framing is malformed: {what}"),
            Self::ManifestUnreadable => write!(formatter, "backup manifest could not be read"),
            Self::ChecksumMismatch => {
                write!(formatter, "backup contents do not match its checksum")
            }
            Self::Io(message) => write!(formatter, "backup file could not be read: {message}"),
        }
    }
}

impl From<std::io::Error> for ContainerError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

/// What a backup says about itself.
///
/// Deliberately absent: passwords, password hashes, session tokens, GSTIN, and any filesystem path.
/// The snapshot legitimately contains Argon2 verifiers because they are application data; repeating
/// anything sensitive here would put it in a file support staff read casually.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub backup_format_version: u16,
    /// Always `AUSHADHARTH`. A second, human-readable identity check beside the magic.
    pub product: String,
    pub backup_id: String,
    pub created_at_utc: String,
    pub application_version: String,
    /// Highest applied migration in the snapshot.
    pub schema_version: i64,
    /// Every applied migration, so compatibility is decided on the whole chain rather than a number.
    pub applied_migrations: Vec<AppliedMigration>,
    pub installation_id: String,
    pub store_id: String,
    pub store_display_name: String,
    /// Lower-case hex SHA-256 over **exactly the payload bytes**.
    pub database_sha256: String,
    pub database_bytes: u64,
    pub backup_kind: String,
    pub source_platform: String,
    /// Empty until attachments exist. Reserved so adding them is not a format change.
    #[serde(default)]
    pub attachments: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppliedMigration {
    pub version: i64,
    pub description: String,
    /// Lower-case hex of the migration checksum sqlx recorded.
    pub checksum: String,
}

/// The fixed part of a container, read without touching the payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerHeader {
    pub format_version: u16,
    pub manifest_len: u32,
    pub payload_len: u64,
}

impl ContainerHeader {
    pub fn payload_offset(&self) -> u64 {
        HEADER_BYTES + u64::from(self.manifest_len)
    }
}

/// Reads and validates the fixed header against the file's real size.
///
/// This is the gate every other operation stands behind: after it returns, the declared lengths are
/// known to be in range and to describe exactly this file, so later code can seek and stream
/// without arithmetic that could wrap or read past the end.
pub fn read_header<R: Read + Seek>(
    reader: &mut R,
    file_len: u64,
) -> Result<ContainerHeader, ContainerError> {
    if file_len < HEADER_BYTES {
        return Err(ContainerError::TooSmall);
    }
    reader.seek(SeekFrom::Start(0))?;
    let mut header = [0_u8; HEADER_BYTES as usize];
    reader.read_exact(&mut header)?;

    if &header[0..8] != MAGIC.as_slice() {
        return Err(ContainerError::NotABackup);
    }
    let format_version = u16::from_be_bytes([header[8], header[9]]);
    if format_version == 0 || format_version > FORMAT_VERSION {
        return Err(ContainerError::UnsupportedFormatVersion(format_version));
    }
    let reserved = u16::from_be_bytes([header[10], header[11]]);
    if reserved != 0 {
        return Err(ContainerError::UnsupportedFormatVersion(format_version));
    }
    let manifest_len = u32::from_be_bytes([header[12], header[13], header[14], header[15]]);
    if manifest_len == 0 || manifest_len > MAX_MANIFEST_BYTES {
        return Err(ContainerError::Malformed("manifest length is out of range"));
    }
    let payload_len = u64::from_be_bytes([
        header[16], header[17], header[18], header[19], header[20], header[21], header[22],
        header[23],
    ]);
    if payload_len == 0 || payload_len > MAX_PAYLOAD_BYTES {
        return Err(ContainerError::Malformed("payload length is out of range"));
    }

    // Checked throughout: a crafted file must not be able to make this wrap and look plausible.
    let declared = HEADER_BYTES
        .checked_add(u64::from(manifest_len))
        .and_then(|value| value.checked_add(payload_len))
        .ok_or(ContainerError::Malformed("declared lengths overflow"))?;
    if declared != file_len {
        // Both directions are the same answer to the operator — the file is not intact — but they
        // are different faults, and the distinction belongs in the log.
        return Err(ContainerError::Malformed(if declared > file_len {
            "file is shorter than its own framing claims"
        } else {
            "file has trailing bytes after the payload"
        }));
    }
    Ok(ContainerHeader {
        format_version,
        manifest_len,
        payload_len,
    })
}

/// Reads the manifest. Only ever called after `read_header` has bounded its length.
pub fn read_manifest<R: Read + Seek>(
    reader: &mut R,
    header: &ContainerHeader,
) -> Result<Manifest, ContainerError> {
    reader.seek(SeekFrom::Start(HEADER_BYTES))?;
    let mut bytes = vec![0_u8; header.manifest_len as usize];
    reader.read_exact(&mut bytes)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| ContainerError::ManifestUnreadable)?;
    serde_json::from_str(text).map_err(|_| ContainerError::ManifestUnreadable)
}

/// Streams the payload to `destination`, hashing as it goes, and verifies the digest.
///
/// The payload never exists in memory as a whole. `CHUNK_BYTES` at a time is the entire cost,
/// whether the backup is a megabyte or two gigabytes.
pub fn stream_payload<R: Read + Seek, W: Write>(
    reader: &mut R,
    header: &ContainerHeader,
    manifest: &Manifest,
    destination: &mut W,
) -> Result<(), ContainerError> {
    reader.seek(SeekFrom::Start(header.payload_offset()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; CHUNK_BYTES];
    let mut remaining = header.payload_len;
    while remaining > 0 {
        let wanted = remaining.min(CHUNK_BYTES as u64) as usize;
        reader.read_exact(&mut buffer[..wanted])?;
        hasher.update(&buffer[..wanted]);
        destination.write_all(&buffer[..wanted])?;
        remaining -= wanted as u64;
    }
    destination.flush()?;
    if hex_digest(hasher) != manifest.database_sha256 {
        return Err(ContainerError::ChecksumMismatch);
    }
    Ok(())
}

/// Hashes a file without loading it, for the snapshot a backup is about to package.
pub fn hash_file(path: &std::path::Path) -> Result<(String, u64), ContainerError> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; CHUNK_BYTES];
    let mut total = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total += read as u64;
    }
    Ok((hex_digest(hasher), total))
}

fn hex_digest(hasher: Sha256) -> String {
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Writes a complete container: header, manifest, then the snapshot streamed in from `payload`.
pub fn write_container<W: Write>(
    destination: &mut W,
    manifest: &Manifest,
    payload: &std::path::Path,
) -> Result<(), ContainerError> {
    let manifest_json =
        serde_json::to_vec(manifest).map_err(|_| ContainerError::ManifestUnreadable)?;
    let manifest_len = u32::try_from(manifest_json.len())
        .map_err(|_| ContainerError::Malformed("manifest is too large"))?;
    if manifest_len == 0 || manifest_len > MAX_MANIFEST_BYTES {
        return Err(ContainerError::Malformed("manifest length is out of range"));
    }
    let payload_len = std::fs::metadata(payload)?.len();
    if payload_len == 0 || payload_len > MAX_PAYLOAD_BYTES {
        return Err(ContainerError::Malformed("payload length is out of range"));
    }

    destination.write_all(MAGIC)?;
    destination.write_all(&FORMAT_VERSION.to_be_bytes())?;
    destination.write_all(&0_u16.to_be_bytes())?;
    destination.write_all(&manifest_len.to_be_bytes())?;
    destination.write_all(&payload_len.to_be_bytes())?;
    destination.write_all(&manifest_json)?;

    let mut file = std::fs::File::open(payload)?;
    let mut buffer = vec![0_u8; CHUNK_BYTES];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        destination.write_all(&buffer[..read])?;
    }
    destination.flush()?;
    Ok(())
}

/// A filename an operator can recognise in a folder of downloads.
///
/// The store name is a convenience, never an authority — product, version and integrity all come
/// from the magic and the manifest, so a renamed file restores perfectly well.
pub fn backup_filename(store_display_name: &str, local_stamp: &str, backup_id: &str) -> String {
    let slug = slugify(store_display_name);
    // The TAIL of the id, not the head. A UUIDv7 begins with a millisecond timestamp whose leading
    // digits change only every few hours, so two backups taken in the same second would otherwise
    // be handed the same name — and the second would silently replace the first.
    let digits: String = backup_id.chars().filter(|c| *c != '-').collect();
    let short: String = digits
        .chars()
        .skip(digits.chars().count().saturating_sub(8))
        .collect();
    if slug.is_empty() {
        format!("AUSHADHARTH-{local_stamp}-{short}.aushbackup")
    } else {
        format!("AUSHADHARTH-{slug}-{local_stamp}-{short}.aushbackup")
    }
}

/// Lower-cases and reduces a store name to characters that are safe on Windows.
///
/// A name with no ASCII letters or digits at all — written entirely in Devanagari, say — yields an
/// empty slug and is simply left out of the filename. Transliterating it would produce something
/// the owner does not recognise, which is worse than omitting it.
fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(character.to_ascii_lowercase());
            if slug.len() >= 32 {
                break;
            }
        } else {
            pending_dash = true;
        }
    }
    slug
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn manifest() -> Manifest {
        Manifest {
            backup_format_version: 1,
            product: "AUSHADHARTH".to_owned(),
            backup_id: "01997000-0000-7000-8000-000000000001".to_owned(),
            created_at_utc: "2026-09-15T10:00:00.000Z".to_owned(),
            application_version: "0.0.0".to_owned(),
            schema_version: 16,
            applied_migrations: vec![AppliedMigration {
                version: 1,
                description: "foundation".to_owned(),
                checksum: "aa".to_owned(),
            }],
            installation_id: "01997000-0000-7000-8000-000000000002".to_owned(),
            store_id: "01997000-0000-7000-8000-000000000003".to_owned(),
            store_display_name: "Care Pharmacy".to_owned(),
            database_sha256: "0".repeat(64),
            database_bytes: 3,
            backup_kind: "manual".to_owned(),
            source_platform: "windows".to_owned(),
            attachments: Vec::new(),
        }
    }

    /// Builds a container by hand so a test can corrupt exactly one field of it.
    fn container(
        manifest: &Manifest,
        payload: &[u8],
        format_version: u16,
        reserved: u16,
    ) -> Vec<u8> {
        let json = serde_json::to_vec(manifest).unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&format_version.to_be_bytes());
        bytes.extend_from_slice(&reserved.to_be_bytes());
        bytes.extend_from_slice(&(json.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        bytes.extend_from_slice(&json);
        bytes.extend_from_slice(payload);
        bytes
    }

    fn digest_of(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hex_digest(hasher)
    }

    fn valid() -> (Vec<u8>, Manifest) {
        let payload = b"abc".to_vec();
        let mut manifest = manifest();
        manifest.database_sha256 = digest_of(&payload);
        manifest.database_bytes = payload.len() as u64;
        (container(&manifest, &payload, 1, 0), manifest)
    }

    #[test]
    fn a_well_formed_container_round_trips() {
        let (bytes, expected) = valid();
        let mut cursor = Cursor::new(bytes.clone());
        let header = read_header(&mut cursor, bytes.len() as u64).unwrap();
        assert_eq!(header.format_version, 1);
        assert_eq!(header.payload_len, 3);
        let manifest = read_manifest(&mut cursor, &header).unwrap();
        assert_eq!(manifest, expected);
        let mut out = Vec::new();
        stream_payload(&mut cursor, &header, &manifest, &mut out).unwrap();
        assert_eq!(out, b"abc");
    }

    #[test]
    fn a_file_too_small_to_hold_a_header_says_so() {
        let mut cursor = Cursor::new(vec![0_u8; 10]);
        assert_eq!(read_header(&mut cursor, 10), Err(ContainerError::TooSmall));
    }

    #[test]
    fn a_file_that_is_not_ours_is_refused_on_the_magic_alone() {
        let mut bytes = valid().0;
        bytes[0] = b'X';
        let length = bytes.len() as u64;
        let mut cursor = Cursor::new(bytes);
        assert_eq!(
            read_header(&mut cursor, length),
            Err(ContainerError::NotABackup)
        );
    }

    #[test]
    fn a_newer_format_is_refused_rather_than_guessed_at() {
        let (_, manifest) = valid();
        let bytes = container(&manifest, b"abc", 2, 0);
        let length = bytes.len() as u64;
        let mut cursor = Cursor::new(bytes);
        assert_eq!(
            read_header(&mut cursor, length),
            Err(ContainerError::UnsupportedFormatVersion(2))
        );
    }

    /// The reserved word exists so a future flags field costs no version bump. Until then it must
    /// be zero, or a file written by something that used it would be misread as version 1.
    #[test]
    fn a_non_zero_reserved_word_is_refused() {
        let (_, manifest) = valid();
        let bytes = container(&manifest, b"abc", 1, 1);
        let length = bytes.len() as u64;
        let mut cursor = Cursor::new(bytes);
        assert!(matches!(
            read_header(&mut cursor, length),
            Err(ContainerError::UnsupportedFormatVersion(_))
        ));
    }

    #[test]
    fn a_zero_or_oversized_manifest_length_is_refused_before_allocating() {
        for length_bytes in [0_u32, MAX_MANIFEST_BYTES + 1, u32::MAX] {
            let mut bytes = valid().0;
            bytes[12..16].copy_from_slice(&length_bytes.to_be_bytes());
            let length = bytes.len() as u64;
            let mut cursor = Cursor::new(bytes);
            assert!(
                matches!(
                    read_header(&mut cursor, length),
                    Err(ContainerError::Malformed(_))
                ),
                "manifest length {length_bytes} was accepted"
            );
        }
    }

    #[test]
    fn a_zero_or_oversized_payload_length_is_refused_before_allocating() {
        for length_bytes in [0_u64, MAX_PAYLOAD_BYTES + 1, u64::MAX] {
            let mut bytes = valid().0;
            bytes[16..24].copy_from_slice(&length_bytes.to_be_bytes());
            let length = bytes.len() as u64;
            let mut cursor = Cursor::new(bytes);
            assert!(
                matches!(
                    read_header(&mut cursor, length),
                    Err(ContainerError::Malformed(_))
                ),
                "payload length {length_bytes} was accepted"
            );
        }
    }

    /// Lengths chosen so that header + manifest + payload wraps `u64`. Without checked arithmetic
    /// this would compute a plausible-looking total and sail past the size check.
    #[test]
    fn lengths_that_would_overflow_are_refused() {
        let mut bytes = valid().0;
        bytes[12..16].copy_from_slice(&MAX_MANIFEST_BYTES.to_be_bytes());
        bytes[16..24].copy_from_slice(&u64::MAX.to_be_bytes());
        let length = bytes.len() as u64;
        let mut cursor = Cursor::new(bytes);
        assert!(matches!(
            read_header(&mut cursor, length),
            Err(ContainerError::Malformed(_))
        ));
    }

    #[test]
    fn trailing_bytes_are_refused() {
        let mut bytes = valid().0;
        bytes.push(0);
        let length = bytes.len() as u64;
        let mut cursor = Cursor::new(bytes);
        assert_eq!(
            read_header(&mut cursor, length),
            Err(ContainerError::Malformed(
                "file has trailing bytes after the payload"
            ))
        );
    }

    #[test]
    fn a_truncated_file_is_refused() {
        let mut bytes = valid().0;
        bytes.pop();
        let length = bytes.len() as u64;
        let mut cursor = Cursor::new(bytes);
        assert_eq!(
            read_header(&mut cursor, length),
            Err(ContainerError::Malformed(
                "file is shorter than its own framing claims"
            ))
        );
    }

    #[test]
    fn a_manifest_that_is_not_utf8_or_not_json_is_refused() {
        for corruption in [vec![0xff_u8, 0xfe], b"{not json".to_vec()] {
            let payload = b"abc";
            let mut bytes = Vec::new();
            bytes.extend_from_slice(MAGIC);
            bytes.extend_from_slice(&1_u16.to_be_bytes());
            bytes.extend_from_slice(&0_u16.to_be_bytes());
            bytes.extend_from_slice(&(corruption.len() as u32).to_be_bytes());
            bytes.extend_from_slice(&(payload.len() as u64).to_be_bytes());
            bytes.extend_from_slice(&corruption);
            bytes.extend_from_slice(payload);
            let length = bytes.len() as u64;
            let mut cursor = Cursor::new(bytes);
            let header = read_header(&mut cursor, length).unwrap();
            assert_eq!(
                read_manifest(&mut cursor, &header),
                Err(ContainerError::ManifestUnreadable)
            );
        }
    }

    #[test]
    fn a_manifest_missing_a_required_field_is_refused() {
        let payload = b"abc";
        let json = br#"{"product":"AUSHADHARTH"}"#;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&1_u16.to_be_bytes());
        bytes.extend_from_slice(&0_u16.to_be_bytes());
        bytes.extend_from_slice(&(json.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        bytes.extend_from_slice(json);
        bytes.extend_from_slice(payload);
        let length = bytes.len() as u64;
        let mut cursor = Cursor::new(bytes);
        let header = read_header(&mut cursor, length).unwrap();
        assert_eq!(
            read_manifest(&mut cursor, &header),
            Err(ContainerError::ManifestUnreadable)
        );
    }

    /// Forward compatibility: a field this build has never heard of must not break it.
    #[test]
    fn an_unknown_additive_manifest_field_is_tolerated() {
        let (_, manifest) = valid();
        let mut value = serde_json::to_value(&manifest).unwrap();
        value["somethingAddedLater"] = serde_json::json!("hello");
        let json = serde_json::to_vec(&value).unwrap();
        let payload = b"abc";
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&1_u16.to_be_bytes());
        bytes.extend_from_slice(&0_u16.to_be_bytes());
        bytes.extend_from_slice(&(json.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        bytes.extend_from_slice(&json);
        bytes.extend_from_slice(payload);
        let length = bytes.len() as u64;
        let mut cursor = Cursor::new(bytes);
        let header = read_header(&mut cursor, length).unwrap();
        assert_eq!(read_manifest(&mut cursor, &header).unwrap(), manifest);
    }

    #[test]
    fn a_payload_that_does_not_match_its_digest_is_refused() {
        let payload = b"abc".to_vec();
        let mut manifest = manifest();
        manifest.database_sha256 = digest_of(b"something else");
        let bytes = container(&manifest, &payload, 1, 0);
        let length = bytes.len() as u64;
        let mut cursor = Cursor::new(bytes);
        let header = read_header(&mut cursor, length).unwrap();
        let read = read_manifest(&mut cursor, &header).unwrap();
        let mut out = Vec::new();
        assert_eq!(
            stream_payload(&mut cursor, &header, &read, &mut out),
            Err(ContainerError::ChecksumMismatch)
        );
    }

    /// The payload is streamed, so a container far larger than the buffer must still round-trip
    /// exactly — including the final partial chunk, which is where an off-by-one would hide.
    #[test]
    fn a_payload_larger_than_one_chunk_round_trips_exactly() {
        let payload: Vec<u8> = (0..(CHUNK_BYTES * 2 + 12345))
            .map(|index| (index % 251) as u8)
            .collect();
        let mut manifest = manifest();
        manifest.database_sha256 = digest_of(&payload);
        manifest.database_bytes = payload.len() as u64;
        let bytes = container(&manifest, &payload, 1, 0);
        let length = bytes.len() as u64;
        let mut cursor = Cursor::new(bytes);
        let header = read_header(&mut cursor, length).unwrap();
        let read = read_manifest(&mut cursor, &header).unwrap();
        let mut out = Vec::new();
        stream_payload(&mut cursor, &header, &read, &mut out).unwrap();
        assert_eq!(out.len(), payload.len());
        assert_eq!(out, payload);
    }

    /// Anti-vacuity for the whole parser: random bytes must never panic, only ever be refused.
    #[test]
    fn arbitrary_rubbish_is_refused_and_never_panics() {
        for seed in 0_u32..256 {
            let bytes: Vec<u8> = (0..(seed as usize % 200))
                .map(|index| ((index as u32).wrapping_mul(seed).wrapping_add(7) % 256) as u8)
                .collect();
            let length = bytes.len() as u64;
            let mut cursor = Cursor::new(bytes);
            let _ = read_header(&mut cursor, length);
        }
        // And the same with a valid magic, so the length fields are reached with hostile values.
        for seed in 0_u32..256 {
            let mut bytes = MAGIC.to_vec();
            bytes.extend((0..24).map(|index| ((index as u32).wrapping_mul(seed) % 256) as u8));
            let length = bytes.len() as u64;
            let mut cursor = Cursor::new(bytes);
            let _ = read_header(&mut cursor, length);
        }
    }

    #[test]
    fn a_filename_identifies_the_product_the_store_and_the_moment() {
        let name = backup_filename(
            "Care Pharmacy",
            "20260915-143052",
            "01997000-0000-7000-8000-abcdef123456",
        );
        assert_eq!(
            name,
            "AUSHADHARTH-care-pharmacy-20260915-143052-ef123456.aushbackup"
        );
    }

    /// The stamp only resolves to the second, so the id is what has to keep two backups apart.
    #[test]
    fn two_backups_taken_in_the_same_second_are_never_given_the_same_name() {
        let first = uuid::Uuid::now_v7().to_string();
        let second = uuid::Uuid::now_v7().to_string();
        assert_ne!(first, second);
        assert_ne!(
            backup_filename("Care Pharmacy", "20260915-143052", &first),
            backup_filename("Care Pharmacy", "20260915-143052", &second),
            "a backup would have silently replaced another"
        );
    }

    #[test]
    fn a_store_name_with_nothing_safe_in_it_is_simply_left_out() {
        for name in ["", "   ", "।।।", "🙂🙂"] {
            let filename = backup_filename(name, "20260915-143052", "01997000-aaaa");
            assert_eq!(filename, "AUSHADHARTH-20260915-143052-7000aaaa.aushbackup");
            assert!(!filename.contains("--"));
        }
    }

    /// Windows reserved device names cannot occur, because every name starts with the product.
    #[test]
    fn a_pathological_store_name_cannot_produce_an_unsafe_filename() {
        for name in [
            "CON",
            "PRN/../..",
            "a<b>c:d\"e/f\\g|h?i*j",
            &"very long pharmacy name that goes on and on".repeat(4),
        ] {
            let filename = backup_filename(name, "20260915-143052", "01997000-aaaa");
            assert!(filename.starts_with("AUSHADHARTH-"));
            assert!(filename.ends_with(".aushbackup"));
            for bad in ['<', '>', ':', '"', '/', '\\', '|', '?', '*'] {
                assert!(!filename.contains(bad), "{filename} contains {bad}");
            }
            assert!(
                filename.len() <= 80,
                "{filename} is {} chars",
                filename.len()
            );
        }
    }

    #[test]
    fn the_application_id_is_the_ascii_the_migration_writes() {
        assert_eq!(APPLICATION_ID, 1_096_110_920);
        let bytes = (APPLICATION_ID as u32).to_be_bytes();
        assert_eq!(&bytes, b"AUSH");
    }
}
