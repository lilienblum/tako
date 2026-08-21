use tako_core::UpgradeMode;

use super::{SqliteStateStore, StateStoreError};

impl SqliteStateStore {
    pub fn set_server_mode(&self, mode: UpgradeMode) -> Result<(), StateStoreError> {
        let conn = self.lock_conn()?;
        conn.execute(
            "UPDATE server_state SET server_mode = ?1 WHERE id = 1;",
            (server_mode_to_str(mode),),
        )?;
        Ok(())
    }

    pub fn server_mode(&self) -> Result<UpgradeMode, StateStoreError> {
        let conn = self.lock_conn()?;
        let mode_str = match conn.query_row(
            "SELECT server_mode FROM server_state WHERE id = 1;",
            [],
            |row| row.get::<_, String>(0),
        ) {
            Ok(mode) => Some(mode),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };

        match mode_str {
            Some(s) => server_mode_from_str(&s),
            None => Ok(UpgradeMode::Normal),
        }
    }

    /// Stale lock threshold: locks older than this are force-acquired.
    pub(crate) const UPGRADE_LOCK_STALE_SECS: i64 = 600; // 10 minutes

    pub fn try_acquire_upgrade_lock(&self, owner: &str) -> Result<bool, StateStoreError> {
        let conn = self.lock_conn()?;
        let tx = conn.unchecked_transaction()?;
        let existing = match tx.query_row(
            "SELECT owner, acquired_at_unix_secs FROM upgrade_lock WHERE id = 1;",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        ) {
            Ok(row) => Some(row),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };
        let now: i64 =
            tx.query_row("SELECT CAST(strftime('%s','now') AS INTEGER);", [], |row| {
                row.get(0)
            })?;

        let acquired = match &existing {
            Some((existing_owner, _)) if existing_owner == owner => true,
            Some((_, acquired_at)) if now - acquired_at > Self::UPGRADE_LOCK_STALE_SECS => {
                tx.execute(
                    "UPDATE upgrade_lock SET owner = ?1, acquired_at_unix_secs = ?2 WHERE id = 1;",
                    (owner, now),
                )?;
                true
            }
            Some(_) => false,
            None => {
                tx.execute(
                    "INSERT INTO upgrade_lock (id, owner, acquired_at_unix_secs)
                     VALUES (1, ?1, CAST(strftime('%s','now') AS INTEGER));",
                    (owner,),
                )?;
                true
            }
        };
        tx.commit()?;
        Ok(acquired)
    }

    pub fn release_upgrade_lock(&self, owner: &str) -> Result<bool, StateStoreError> {
        let conn = self.lock_conn()?;
        let tx = conn.unchecked_transaction()?;
        let existing =
            match tx.query_row("SELECT owner FROM upgrade_lock WHERE id = 1;", [], |row| {
                row.get::<_, String>(0)
            }) {
                Ok(owner) => Some(owner),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => return Err(e.into()),
            };

        let released = match existing {
            Some(existing) if existing == owner => {
                tx.execute("DELETE FROM upgrade_lock WHERE id = 1;", [])?;
                true
            }
            _ => false,
        };
        tx.commit()?;
        Ok(released)
    }

    pub fn upgrade_lock_owner(&self) -> Result<Option<String>, StateStoreError> {
        let conn = self.lock_conn()?;
        match conn.query_row("SELECT owner FROM upgrade_lock WHERE id = 1;", [], |row| {
            row.get(0)
        }) {
            Ok(owner) => Ok(Some(owner)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

fn server_mode_to_str(mode: UpgradeMode) -> &'static str {
    match mode {
        UpgradeMode::Normal => "normal",
        UpgradeMode::Upgrading => "upgrading",
    }
}

fn server_mode_from_str(value: &str) -> Result<UpgradeMode, StateStoreError> {
    match value {
        "normal" => Ok(UpgradeMode::Normal),
        "upgrading" => Ok(UpgradeMode::Upgrading),
        other => Err(StateStoreError::InvalidData(format!(
            "unknown server_mode value: {}",
            other
        ))),
    }
}
