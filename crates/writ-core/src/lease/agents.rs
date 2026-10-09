//! Agent registry rows on the lease store.

use rusqlite::params;

use super::query::{list_agents_on, list_leases_on};
use super::{AgentIdentity, AgentRecord, Lease, LeaseStore, Result, lease_err, now_secs};

impl LeaseStore {
    pub fn list_agents(&self) -> Result<Vec<AgentRecord>> {
        let conn = self.lock()?;
        list_agents_on(&conn)
    }

    /// Leases and agents from one read transaction.
    ///
    /// The mutex serializes this connection only; the transaction keeps other
    /// processes' writes from splitting the two reads.
    pub fn snapshot(&self) -> Result<(Vec<Lease>, Vec<AgentRecord>)> {
        let conn = self.lock()?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| lease_err("snapshot transaction", e))?;
        let leases = list_leases_on(&tx, "ORDER BY id", "list leases")?;
        let agents = list_agents_on(&tx)?;
        tx.commit().map_err(|e| lease_err("snapshot commit", e))?;
        Ok((leases, agents))
    }

    /// Upsert a live agent-registry row.
    pub fn upsert_agent(&self, identity: AgentIdentity<'_>) -> Result<()> {
        let now = now_secs();
        let conn = self.lock()?;
        conn.execute(
            "
            INSERT INTO agents (agent_id, agent_type, session_id, started_at, stopped_at)
            VALUES (?1, ?2, ?3, ?4, NULL)
            ON CONFLICT(agent_id) DO UPDATE SET
                agent_type = excluded.agent_type,
                session_id = excluded.session_id,
                started_at = excluded.started_at,
                stopped_at = NULL
            ",
            params![
                identity.agent_id,
                identity.agent_type,
                identity.session_id,
                now
            ],
        )
        .map_err(|e| lease_err("upsert agent", e))?;
        Ok(())
    }

    /// Retire an agent-registry row.
    pub fn retire_agent(&self, agent_id: &str) -> Result<()> {
        let now = now_secs();
        let conn = self.lock()?;
        conn.execute(
            "UPDATE agents SET stopped_at = ?1 WHERE agent_id = ?2",
            params![now, agent_id],
        )
        .map_err(|e| lease_err("retire agent", e))?;
        Ok(())
    }
}
