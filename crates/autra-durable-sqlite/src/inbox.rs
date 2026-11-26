//! Inbox publication and consumption share the Journal's write transactions.
use crate::SqliteEffectJournal;
use autra_core::{EffectId, EffectRecord, EffectStatus, RunId};
use autra_execution::{EffectResult, JournalError, OutboxEntry, ResultIdentity};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use std::time::Duration;
fn err(e: impl std::fmt::Display) -> JournalError {
    JournalError::new(format!("result inbox: {e}"))
}
pub(crate) fn check_owner(
    tx: &Transaction<'_>,
    run: &RunId,
    owner: &str,
) -> Result<(), JournalError> {
    let owned: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM run_leases WHERE run_id=?1 AND owner_id=?2 AND deadline_ms>?3",
            params![run.as_str(), owner, autra_execution::now_unix_ms() as i64],
            |r| r.get(0),
        )
        .optional()
        .map_err(err)?;
    if owned.is_none() {
        return Err(JournalError::new("coordinator run lease was lost"));
    }
    Ok(())
}
pub(crate) fn consume(
    tx: &Transaction<'_>,
    run: &RunId,
    source: &ResultIdentity,
) -> Result<(), JournalError> {
    let n=tx.execute("UPDATE result_inbox SET consumed=1 WHERE run_id=?1 AND effect_id=?2 AND attempt_id=?3 AND consumed=0",params![run.as_str(),source.effect_id.as_str(),source.attempt_id]).map_err(err)?;
    if n != 1 {
        return Err(JournalError::new("result is missing or already consumed"));
    }
    Ok(())
}
impl SqliteEffectJournal {
    pub(crate) fn resolve_effect_inner(
        &self,
        run: &RunId,
        effect: &EffectRecord,
        owner: Option<&str>,
    ) -> Result<(), JournalError> {
        if !matches!(
            effect.status,
            EffectStatus::Authorized | EffectStatus::Rejected
        ) || effect.intent_json.is_empty()
        {
            return Err(JournalError::new("invalid authorization state or intent"));
        }
        let payload = serde_json::to_string(effect).map_err(err)?;
        let mut c = self.writer()?;
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(err)?;
        if let Some(owner) = owner {
            check_owner(&tx, run, owner)?;
        }
        let live:Option<i64>=tx.query_row("SELECT 1 FROM effect_outbox WHERE effect_id=?1 AND claim_id IS NOT NULL AND claim_deadline>datetime('now')",params![effect.id.as_str()],|r|r.get(0)).optional().map_err(err)?;
        if live.is_some() {
            return Err(JournalError::new("cannot authorize a claimed effect"));
        }
        tx.execute(
            "UPDATE result_inbox SET consumed=2 WHERE run_id=?1 AND effect_id=?2 AND consumed=0",
            params![run.as_str(), effect.id.as_str()],
        )
        .map_err(err)?;
        tx.execute("INSERT INTO effects(effect_id,run_id,payload_json) VALUES (?1,?2,?3) ON CONFLICT(effect_id) DO UPDATE SET payload_json=excluded.payload_json,updated_at=CURRENT_TIMESTAMP",params![effect.id.as_str(),run.as_str(),payload]).map_err(err)?;
        if effect.status == EffectStatus::Authorized {
            tx.execute("INSERT OR IGNORE INTO effect_outbox(effect_id,run_id,intent_json,attempts) VALUES (?1,?2,?3,?4)",params![effect.id.as_str(),run.as_str(),effect.intent_json,effect.claim_generation()]).map_err(err)?;
        } else {
            tx.execute(
                "DELETE FROM effect_outbox WHERE run_id=?1 AND effect_id=?2",
                params![run.as_str(), effect.id.as_str()],
            )
            .map_err(err)?;
        }
        tx.commit().map_err(err)
    }
    pub(crate) fn finish_result(
        &self,
        run: &RunId,
        effect: &EffectRecord,
        claim: &str,
        result: &EffectResult,
    ) -> Result<(), JournalError> {
        if result.identity.effect_id != effect.id {
            return Err(JournalError::new("result effect identity mismatch"));
        }
        let result_json = serde_json::to_string(result).map_err(err)?;
        let effect_json = serde_json::to_string(effect).map_err(err)?;
        let mut c = self.writer()?;
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(err)?;
        let old:Option<String>=tx.query_row("SELECT payload_json FROM result_inbox WHERE run_id=?1 AND effect_id=?2 AND attempt_id=?3",params![run.as_str(),effect.id.as_str(),result.identity.attempt_id],|r|r.get(0)).optional().map_err(err)?;
        if let Some(old) = old {
            return if old == result_json {
                Ok(())
            } else {
                Err(JournalError::new("conflicting result identity"))
            };
        }
        let owned:Option<i64>=tx.query_row("SELECT 1 FROM effect_outbox WHERE run_id=?1 AND effect_id=?2 AND claim_id=?3 AND claim_deadline>datetime('now')",params![run.as_str(),effect.id.as_str(),claim],|r|r.get(0)).optional().map_err(err)?;
        if owned.is_none() {
            return Err(JournalError::claim_lost(&effect.id, claim));
        }
        let n=tx.execute("UPDATE effects SET payload_json=?1,updated_at=CURRENT_TIMESTAMP WHERE run_id=?2 AND effect_id=?3",params![effect_json,run.as_str(),effect.id.as_str()]).map_err(err)?;
        if n != 1 {
            return Err(JournalError::new("effect record is missing"));
        }
        tx.execute("INSERT INTO result_inbox(run_id,effect_id,attempt_id,payload_json) VALUES (?1,?2,?3,?4)",params![run.as_str(),effect.id.as_str(),result.identity.attempt_id,result_json]).map_err(err)?;
        tx.execute(
            "DELETE FROM effect_outbox WHERE effect_id=?1 AND claim_id=?2",
            params![effect.id.as_str(), claim],
        )
        .map_err(err)?;
        tx.commit().map_err(err)
    }
    pub(crate) fn read_results(
        &self,
        run: &RunId,
        limit: usize,
    ) -> Result<Vec<EffectResult>, JournalError> {
        let c = self.reader()?;
        let mut q=c.prepare("SELECT payload_json FROM result_inbox WHERE run_id=?1 AND consumed=0 ORDER BY ordinal LIMIT ?2").map_err(err)?;
        let rows = q
            .query_map(
                params![run.as_str(), limit.min(i64::MAX as usize) as i64],
                |r| r.get::<_, String>(0),
            )
            .map_err(err)?;
        rows.map(|r| serde_json::from_str(&r.map_err(err)?).map_err(err))
            .collect()
    }
    pub(crate) fn peek_entries(
        &self,
        run: Option<&RunId>,
        limit: usize,
    ) -> Result<Vec<OutboxEntry>, JournalError> {
        let c = self.reader()?;
        let mut q=c.prepare("SELECT effect_id,run_id,intent_json,attempts FROM effect_outbox WHERE (claim_id IS NULL OR claim_deadline<=datetime('now')) AND (?1 IS NULL OR run_id=?1) ORDER BY created_at,effect_id LIMIT ?2").map_err(err)?;
        let rows = q
            .query_map(
                params![run.map(RunId::as_str), limit.min(i64::MAX as usize) as i64],
                |r| {
                    Ok(OutboxEntry {
                        effect_id: EffectId::new(r.get::<_, String>(0)?),
                        run_id: RunId::new(r.get::<_, String>(1)?),
                        intent_json: r.get(2)?,
                        attempts: r.get(3)?,
                    })
                },
            )
            .map_err(err)?;
        rows.map(|r| r.map_err(err)).collect()
    }
    pub(crate) fn claim_entry(
        &self,
        id: &EffectId,
        claim: &str,
        lease: Duration,
    ) -> Result<Option<OutboxEntry>, JournalError> {
        let modifier = format!("+{} seconds", lease.as_secs().max(1));
        let c = self.writer()?;
        c.query_row("UPDATE effect_outbox SET claim_id=?1,claim_deadline=datetime('now',?3),attempts=attempts+1 WHERE effect_id=?2 AND (claim_id IS NULL OR claim_deadline<=datetime('now')) RETURNING effect_id,run_id,intent_json,attempts",params![claim,id.as_str(),modifier],|r|Ok(OutboxEntry {effect_id:EffectId::new(r.get::<_,String>(0)?),run_id:RunId::new(r.get::<_,String>(1)?),intent_json:r.get(2)?,attempts:r.get(3)?})).optional().map_err(err)
    }
}
