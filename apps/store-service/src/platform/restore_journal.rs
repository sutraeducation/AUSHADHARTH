//! The restore journal.
//!
//! A restore replaces the database, so the database cannot be the thing that remembers it is
//! happening. This file lives beside it and survives the swap, the crash, and the power cut.
//!
//! It is read at startup **before** the pool opens, because if a restore was interrupted the
//! authoritative path may hold the old database, the new one, or nothing at all, and only the
//! journal can say which. When it cannot say, recovery is conservative: put back what was there.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

/// How far a restore had got when the process last wrote to disk.
///
/// The order is the order of the operation, and every transition is flushed and `sync_all`ed before
/// the step it describes is attempted — so the journal is never behind reality, only ever ahead of
/// it. Being ahead is safe; being behind would mean recovering the wrong way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreStage {
    /// Candidate validated and staged. Nothing has moved.
    Prepared,
    /// The current database has been backed up. Still nothing has moved.
    SafetyTaken,
    /// The live database has been renamed aside. The authoritative path may be empty.
    OldMoved,
    /// The candidate sits at the authoritative path, not yet validated.
    CandidateInstalled,
    /// The restored database opened and validated. Only cleanup remains.
    Validated,
}

/// What a restore needs to recover itself, and nothing more.
///
/// No password, no session token, no GSTIN, no caller-supplied path. The three paths here are all
/// produced by the service inside its own data directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreJournal {
    pub restore_id: String,
    pub stage: RestoreStage,
    pub started_at_utc: String,
    /// Where the validated candidate is staged.
    pub candidate_path: PathBuf,
    /// Where the live database is moved to before the candidate takes its place.
    pub superseded_path: PathBuf,
    /// The authoritative database path being replaced.
    pub database_path: PathBuf,
    /// Filename of the pre-restore safety backup, once taken.
    pub safety_backup_name: Option<String>,
    /// Facts the restored database will record about where it came from. Carried here because the
    /// provenance row can only be written after the restore succeeds, and by then the manifest is
    /// no longer in hand.
    pub source_store_id: String,
    pub source_installation_id: String,
    pub source_backup_created_at_utc: String,
    pub source_database_sha256: String,
    pub pre_restore_database_sha256: Option<String>,
    pub backup_format_version: u16,
    pub source_schema_version: i64,
    pub initiated_by_user_id: Option<String>,
    pub initiated_by_login: Option<String>,
}

/// Where the journal lives: beside the backups, never inside the database it describes.
pub fn journal_path(backups_directory: &Path) -> PathBuf {
    backups_directory.join("restore.journal")
}

/// Writes the journal durably.
///
/// Temp file, flush, `sync_all`, rename — so a crash mid-write leaves either the previous journal
/// or the new one, never a half-written state that recovery would have to guess about. The parent
/// directory is synced too, because on a crash a rename that has not reached the directory entry
/// did not happen.
pub fn write(backups_directory: &Path, journal: &RestoreJournal) -> std::io::Result<()> {
    fs::create_dir_all(backups_directory)?;
    let final_path = journal_path(backups_directory);
    let temp_path = final_path.with_extension("journal.writing");
    {
        let mut file = fs::File::create(&temp_path)?;
        let bytes = serde_json::to_vec_pretty(journal)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        file.write_all(&bytes)?;
        file.flush()?;
        file.sync_all()?;
    }
    fs::rename(&temp_path, &final_path)?;
    if let Ok(directory) = fs::File::open(backups_directory) {
        let _ = directory.sync_all();
    }
    Ok(())
}

/// Advances the stage and rewrites the journal durably.
pub fn advance(
    backups_directory: &Path,
    journal: &mut RestoreJournal,
    stage: RestoreStage,
) -> std::io::Result<()> {
    journal.stage = stage;
    write(backups_directory, journal)
}

/// Reads the journal, if one is present.
///
/// A journal that cannot be parsed is reported as [`JournalState::Unreadable`] rather than as
/// absent. The difference matters: absent means no restore was in flight, unreadable means one may
/// have been, and those call for opposite actions.
pub fn read(backups_directory: &Path) -> JournalState {
    let path = journal_path(backups_directory);
    match fs::read(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => JournalState::Absent,
        Err(error) => JournalState::Unreadable(error.to_string()),
        Ok(bytes) => match serde_json::from_slice::<RestoreJournal>(&bytes) {
            Ok(journal) => JournalState::Present(Box::new(journal)),
            Err(error) => JournalState::Unreadable(error.to_string()),
        },
    }
}

#[derive(Debug)]
pub enum JournalState {
    Absent,
    Present(Box<RestoreJournal>),
    /// Truncated, tampered with, or written by something else entirely.
    Unreadable(String),
}

pub fn clear(backups_directory: &Path) -> std::io::Result<()> {
    let path = journal_path(backups_directory);
    match fs::remove_file(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// What startup should do about an interrupted restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovery {
    /// No restore was in flight.
    Nothing,
    /// Discard the staged candidate; the live database was never touched.
    DiscardCandidate,
    /// Put the superseded database back at the authoritative path.
    RollBack,
    /// The candidate is installed but unvalidated: validate it, and roll back if it fails.
    ValidateInstalled,
    /// The restore succeeded; only cleanup is outstanding.
    FinishCleanup,
    /// Nothing can be proven. Refuse to start rather than open a database that may be half restored.
    Refuse(&'static str),
}

/// Decides recovery from the journal alone, without touching the filesystem.
///
/// Kept pure so every branch is testable without staging real databases; the caller performs the
/// filesystem work and applies its own checks on top.
pub fn recovery_for(state: &JournalState) -> Recovery {
    match state {
        JournalState::Absent => Recovery::Nothing,
        // A journal we cannot read may describe a restore that moved the live database. Rolling
        // back is the only action that is safe whether or not it did: if nothing moved, the
        // superseded file is simply absent and the caller carries on.
        JournalState::Unreadable(_) => Recovery::RollBack,
        JournalState::Present(journal) => match journal.stage {
            RestoreStage::Prepared | RestoreStage::SafetyTaken => Recovery::DiscardCandidate,
            RestoreStage::OldMoved => Recovery::RollBack,
            RestoreStage::CandidateInstalled => Recovery::ValidateInstalled,
            RestoreStage::Validated => Recovery::FinishCleanup,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn journal(stage: RestoreStage) -> RestoreJournal {
        RestoreJournal {
            restore_id: "01997000-0000-7000-8000-000000000001".to_owned(),
            stage,
            started_at_utc: "2026-09-15T10:00:00.000Z".to_owned(),
            candidate_path: PathBuf::from("candidate.sqlite3"),
            superseded_path: PathBuf::from("superseded.sqlite3"),
            database_path: PathBuf::from("aushadharth.sqlite3"),
            safety_backup_name: None,
            source_store_id: "store".to_owned(),
            source_installation_id: "installation".to_owned(),
            source_backup_created_at_utc: "2026-09-14T10:00:00.000Z".to_owned(),
            source_database_sha256: "0".repeat(64),
            pre_restore_database_sha256: None,
            backup_format_version: 1,
            source_schema_version: 16,
            initiated_by_user_id: None,
            initiated_by_login: None,
        }
    }

    #[test]
    fn a_journal_survives_a_write_and_read_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        write(temp.path(), &journal(RestoreStage::Prepared)).unwrap();
        match read(temp.path()) {
            JournalState::Present(found) => assert_eq!(found.stage, RestoreStage::Prepared),
            other => panic!("expected a journal, found {other:?}"),
        }
        clear(temp.path()).unwrap();
        assert!(matches!(read(temp.path()), JournalState::Absent));
    }

    #[test]
    fn advancing_rewrites_the_stage_durably() {
        let temp = tempfile::tempdir().unwrap();
        let mut record = journal(RestoreStage::Prepared);
        write(temp.path(), &record).unwrap();
        advance(temp.path(), &mut record, RestoreStage::OldMoved).unwrap();
        match read(temp.path()) {
            JournalState::Present(found) => assert_eq!(found.stage, RestoreStage::OldMoved),
            other => panic!("expected a journal, found {other:?}"),
        }
    }

    /// No journal means no restore was in flight, which must never be confused with a lost one.
    #[test]
    fn an_absent_journal_asks_for_nothing() {
        assert_eq!(recovery_for(&JournalState::Absent), Recovery::Nothing);
    }

    #[test]
    fn every_stage_has_exactly_one_deterministic_recovery() {
        let cases = [
            (RestoreStage::Prepared, Recovery::DiscardCandidate),
            (RestoreStage::SafetyTaken, Recovery::DiscardCandidate),
            (RestoreStage::OldMoved, Recovery::RollBack),
            (
                RestoreStage::CandidateInstalled,
                Recovery::ValidateInstalled,
            ),
            (RestoreStage::Validated, Recovery::FinishCleanup),
        ];
        for (stage, expected) in cases {
            assert_eq!(
                recovery_for(&JournalState::Present(Box::new(journal(stage)))),
                expected,
                "{stage:?} recovered the wrong way"
            );
        }
    }

    /// The conservative branch. A journal that has been truncated, corrupted or edited tells us
    /// nothing we can trust, so recovery does the one thing that is safe either way.
    #[test]
    fn an_unreadable_journal_rolls_back_rather_than_guessing() {
        let temp = tempfile::tempdir().unwrap();
        for rubbish in [
            b"".to_vec(),
            b"{\"stage\":".to_vec(),
            b"not json at all".to_vec(),
        ] {
            fs::create_dir_all(temp.path()).unwrap();
            fs::write(journal_path(temp.path()), rubbish).unwrap();
            let state = read(temp.path());
            assert!(matches!(state, JournalState::Unreadable(_)));
            assert_eq!(recovery_for(&state), Recovery::RollBack);
        }
    }

    /// A stage this build does not know is not a stage. Treated as unreadable, so it rolls back.
    #[test]
    fn an_unknown_stage_is_treated_as_unreadable() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path()).unwrap();
        let mut value = serde_json::to_value(journal(RestoreStage::Prepared)).unwrap();
        value["stage"] = serde_json::json!("something_from_the_future");
        fs::write(
            journal_path(temp.path()),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        let state = read(temp.path());
        assert!(matches!(state, JournalState::Unreadable(_)));
        assert_eq!(recovery_for(&state), Recovery::RollBack);
    }

    /// A half-written journal must never be observable: the temp-then-rename is what guarantees it.
    #[test]
    fn a_partial_write_never_replaces_a_good_journal() {
        let temp = tempfile::tempdir().unwrap();
        write(temp.path(), &journal(RestoreStage::SafetyTaken)).unwrap();
        // Simulate a crash during the next write by leaving only the temp file behind.
        fs::write(
            journal_path(temp.path()).with_extension("journal.writing"),
            b"half a journ",
        )
        .unwrap();
        match read(temp.path()) {
            JournalState::Present(found) => assert_eq!(found.stage, RestoreStage::SafetyTaken),
            other => panic!("the good journal was lost: {other:?}"),
        }
    }
}
