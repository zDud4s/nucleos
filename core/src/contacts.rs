use sqlx::{Sqlite, SqlitePool, Transaction};

pub struct Profile {
    pub messages_in: i64,
    // Consumed by the later contact display surface.
    #[allow(dead_code)]
    pub first_seen: String,
    // Consumed by the later contact display surface.
    #[allow(dead_code)]
    pub last_seen: String,
    pub outbound_ever: bool,
}

pub fn normalize_address(address: &str) -> String {
    let trimmed = address.trim();
    let address_only = trimmed
        .rfind('<')
        .and_then(|start| {
            trimmed[start + 1..]
                .find('>')
                .map(|end| &trimmed[start + 1..start + 1 + end])
        })
        .unwrap_or(trimmed);

    address_only.trim().to_lowercase()
}

#[derive(Clone, Copy)]
enum MessageDirection {
    Inbound,
    // Consumed by the Sent-folder ingestion packet (P4).
    #[allow(dead_code)]
    Outbound,
}

impl MessageDirection {
    fn update_statement(self) -> &'static str {
        match self {
            Self::Inbound => {
                "UPDATE contact_addresses
                 SET first_seen = MIN(first_seen, ?),
                     last_seen = MAX(last_seen, ?),
                     messages_in = messages_in + 1
                 WHERE address = ?"
            }
            Self::Outbound => {
                "UPDATE contact_addresses
                 SET first_seen = MIN(first_seen, ?),
                     last_seen = MAX(last_seen, ?),
                     outbound_ever = 1
                 WHERE address = ?"
            }
        }
    }

    fn insert_statement(self) -> &'static str {
        match self {
            Self::Inbound => {
                "INSERT INTO contact_addresses (
                     address, contact_id, first_seen, last_seen, messages_in, outbound_ever
                 )
                 VALUES (?, ?, ?, ?, 1, 0)"
            }
            Self::Outbound => {
                "INSERT INTO contact_addresses (
                     address, contact_id, first_seen, last_seen, messages_in, outbound_ever
                 )
                 VALUES (?, ?, ?, ?, 0, 1)"
            }
        }
    }
}

async fn record_message(
    transaction: &mut Transaction<'_, Sqlite>,
    address: &str,
    occurred_at: &str,
    direction: MessageDirection,
) -> sqlx::Result<()> {
    let address = normalize_address(address);
    let contact_id: Option<i64> =
        sqlx::query_scalar("SELECT contact_id FROM contact_addresses WHERE address = ?")
            .bind(&address)
            .fetch_optional(&mut **transaction)
            .await?;

    if contact_id.is_some() {
        sqlx::query(direction.update_statement())
            .bind(occurred_at)
            .bind(occurred_at)
            .bind(&address)
            .execute(&mut **transaction)
            .await?;
    } else {
        let contact_id =
            sqlx::query("INSERT INTO contacts (display_name, created_at) VALUES (NULL, ?)")
                .bind(occurred_at)
                .execute(&mut **transaction)
                .await?
                .last_insert_rowid();

        sqlx::query(direction.insert_statement())
            .bind(&address)
            .bind(contact_id)
            .bind(occurred_at)
            .bind(occurred_at)
            .execute(&mut **transaction)
            .await?;
    }

    Ok(())
}

pub async fn record_inbound(
    transaction: &mut Transaction<'_, Sqlite>,
    from_addr: &str,
    received_at: &str,
) -> sqlx::Result<()> {
    record_message(
        transaction,
        from_addr,
        received_at,
        MessageDirection::Inbound,
    )
    .await
}

// Consumed by the Sent-folder ingestion packet (P4).
#[allow(dead_code)]
pub async fn record_outbound(
    transaction: &mut Transaction<'_, Sqlite>,
    to_addrs: &[&str],
    sent_at: &str,
) -> sqlx::Result<()> {
    for to_addr in to_addrs {
        record_message(transaction, to_addr, sent_at, MessageDirection::Outbound).await?;
    }

    Ok(())
}

pub async fn profile_for(pool: &SqlitePool, address: &str) -> sqlx::Result<Option<Profile>> {
    let address = normalize_address(address);
    let row: Option<(i64, String, String, i64)> = sqlx::query_as(
        "SELECT SUM(facts.messages_in),
                MIN(facts.first_seen),
                MAX(facts.last_seen),
                MAX(facts.outbound_ever)
         FROM contact_addresses AS requested
         JOIN contact_addresses AS facts ON facts.contact_id = requested.contact_id
         WHERE requested.address = ?
         GROUP BY requested.contact_id",
    )
    .bind(address)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(
        |(messages_in, first_seen, last_seen, outbound_ever)| Profile {
            messages_in,
            first_seen,
            last_seen,
            outbound_ever: outbound_ever != 0,
        },
    ))
}

// Consumed by the merge-proposal packet.
#[allow(dead_code)]
pub async fn merge(pool: &SqlitePool, keep_id: i64, absorb_id: i64) -> sqlx::Result<()> {
    let linked_at = chrono::Utc::now().to_rfc3339();
    let mut transaction = pool.begin().await?;

    sqlx::query(
        "UPDATE contact_addresses
         SET contact_id = ?,
             linked_by = 'human',
             linked_at = ?
         WHERE contact_id = ?",
    )
    .bind(keep_id)
    .bind(linked_at)
    .bind(absorb_id)
    .execute(&mut *transaction)
    .await?;

    transaction.commit().await
}

// Consumed by the merge-proposal packet.
#[allow(dead_code)]
pub async fn unmerge(pool: &SqlitePool, address: &str) -> sqlx::Result<()> {
    let address = normalize_address(address);
    let created_at = chrono::Utc::now().to_rfc3339();
    let mut transaction = pool.begin().await?;

    let contact_id =
        sqlx::query("INSERT INTO contacts (display_name, created_at) VALUES (NULL, ?)")
            .bind(created_at)
            .execute(&mut *transaction)
            .await?
            .last_insert_rowid();

    sqlx::query(
        "UPDATE contact_addresses
         SET contact_id = ?,
             linked_by = 'implicit',
             linked_at = NULL
         WHERE address = ?",
    )
    .bind(contact_id)
    .bind(address)
    .execute(&mut *transaction)
    .await?;

    transaction.commit().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone, Utc};

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn os_factos_sobrevivem_ao_prune() {
        let pool = test_pool().await;
        let address = "alice@example.com";
        let now = Utc.with_ymd_and_hms(2026, 7, 29, 12, 0, 0).unwrap();
        let received_at =
            (now - Duration::days(crate::triage::ROW_RETENTION_DAYS + 1)).to_rfc3339();

        let mut transaction = pool.begin().await.unwrap();
        sqlx::query(
            "INSERT INTO emails (
                 message_id, mailbox, uidvalidity, uid, from_addr, received_at, ingested_at,
                 triage_class, triaged_at, direction
             )
             VALUES (?, 'INBOX', 1, 1, ?, ?, ?, 'info', ?, 'inbound')",
        )
        .bind("<old-inbound@example.com>")
        .bind(address)
        .bind(&received_at)
        .bind(&received_at)
        .bind(&received_at)
        .execute(&mut *transaction)
        .await
        .unwrap();
        record_inbound(&mut transaction, address, &received_at)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let before = profile_for(&pool, address)
            .await
            .unwrap()
            .expect("the accumulated contact profile must exist before pruning");
        assert_eq!(before.messages_in, 1);
        assert_eq!(before.first_seen, received_at);
        assert!(!before.outbound_ever);

        let (_, rows_removed) = crate::triage::prune(&pool, 7, now).await.unwrap();
        assert_eq!(rows_removed, 1);
        let emails_left: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM emails WHERE message_id = '<old-inbound@example.com>'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(emails_left, 0);

        let after = profile_for(&pool, address)
            .await
            .unwrap()
            .expect("pruning the source email must not prune accumulated contact facts");
        assert_eq!(after.messages_in, before.messages_in);
        assert_eq!(after.first_seen, before.first_seen);
        assert_eq!(after.outbound_ever, before.outbound_ever);
    }

    #[tokio::test]
    async fn a_acumulacao_e_monotonica() {
        let pool = test_pool().await;
        let address = "bob@example.com";
        let earlier = "2026-07-20T09:00:00+00:00";
        let outbound_between = "2026-07-22T09:00:00+00:00";
        let later = "2026-07-24T09:00:00+00:00";

        let mut transaction = pool.begin().await.unwrap();
        record_inbound(&mut transaction, address, later)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let after_first = profile_for(&pool, address)
            .await
            .unwrap()
            .expect("the first inbound fact must create a profile");
        assert_eq!(after_first.messages_in, 1);
        assert_eq!(after_first.first_seen, later);
        assert_eq!(after_first.last_seen, later);
        assert!(!after_first.outbound_ever);

        let mut transaction = pool.begin().await.unwrap();
        record_outbound(&mut transaction, &[address], outbound_between)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let after_outbound = profile_for(&pool, address)
            .await
            .unwrap()
            .expect("the outbound fact must preserve the profile");
        assert_eq!(after_outbound.messages_in, 1);
        assert!(after_outbound.outbound_ever);

        let mut transaction = pool.begin().await.unwrap();
        record_inbound(&mut transaction, address, earlier)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let after_second = profile_for(&pool, address)
            .await
            .unwrap()
            .expect("the second inbound fact must preserve the profile");
        assert_eq!(after_second.messages_in, after_first.messages_in + 1);
        assert_eq!(after_second.messages_in, 2);
        assert_eq!(after_second.first_seen, earlier);
        assert_eq!(after_second.last_seen, later);
        assert!(after_second.outbound_ever);
    }

    #[tokio::test]
    async fn o_insert_e_a_acumulacao_sao_atomicos() {
        let pool = test_pool().await;
        let address = "carol@example.com";
        let received_at = "2026-07-29T10:00:00+00:00";

        let mut transaction = pool.begin().await.unwrap();
        sqlx::query(
            "INSERT INTO emails (
                 message_id, mailbox, uidvalidity, uid, from_addr, received_at, ingested_at,
                 direction
             )
             VALUES (?, 'INBOX', 1, 2, ?, ?, ?, 'inbound')",
        )
        .bind("<rolled-back@example.com>")
        .bind(address)
        .bind(received_at)
        .bind(received_at)
        .execute(&mut *transaction)
        .await
        .unwrap();
        record_inbound(&mut transaction, address, received_at)
            .await
            .unwrap();
        transaction.rollback().await.unwrap();

        let emails_left: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM emails WHERE message_id = '<rolled-back@example.com>'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let contacts_left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM contacts")
            .fetch_one(&pool)
            .await
            .unwrap();
        let addresses_left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM contact_addresses")
            .fetch_one(&pool)
            .await
            .unwrap();

        assert_eq!(emails_left, 0);
        assert_eq!(contacts_left, 0);
        assert_eq!(addresses_left, 0);
        assert!(profile_for(&pool, address).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn o_perfil_de_uma_pessoa_soma_os_enderecos() {
        let pool = test_pool().await;
        let keep_address = "ana@example.com";
        let absorbed_address = "ana@work.example";
        let earliest = "2026-07-20T09:00:00+00:00";
        let second = "2026-07-21T09:00:00+00:00";
        let third = "2026-07-22T09:00:00+00:00";
        let absorbed_inbound = "2026-07-23T09:00:00+00:00";
        let latest = "2026-07-24T09:00:00+00:00";

        let mut transaction = pool.begin().await.unwrap();
        record_inbound(&mut transaction, keep_address, earliest)
            .await
            .unwrap();
        record_inbound(&mut transaction, keep_address, second)
            .await
            .unwrap();
        record_inbound(&mut transaction, keep_address, third)
            .await
            .unwrap();
        record_inbound(&mut transaction, absorbed_address, absorbed_inbound)
            .await
            .unwrap();
        record_outbound(&mut transaction, &[absorbed_address], latest)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let keep_id: i64 =
            sqlx::query_scalar("SELECT contact_id FROM contact_addresses WHERE address = ?")
                .bind(keep_address)
                .fetch_one(&pool)
                .await
                .unwrap();
        let absorb_id: i64 =
            sqlx::query_scalar("SELECT contact_id FROM contact_addresses WHERE address = ?")
                .bind(absorbed_address)
                .fetch_one(&pool)
                .await
                .unwrap();

        merge(&pool, keep_id, absorb_id).await.unwrap();

        let profile_from_keep = profile_for(&pool, keep_address)
            .await
            .unwrap()
            .expect("the kept address must resolve to the merged profile");
        let profile_from_absorbed = profile_for(&pool, absorbed_address)
            .await
            .unwrap()
            .expect("the absorbed address must resolve to the merged profile");

        assert_eq!(profile_from_keep.messages_in, 4);
        assert_eq!(profile_from_keep.first_seen, earliest);
        assert_eq!(profile_from_keep.last_seen, latest);
        assert!(profile_from_keep.outbound_ever);
        assert_eq!(
            profile_from_absorbed.messages_in,
            profile_from_keep.messages_in
        );
        assert_eq!(
            profile_from_absorbed.first_seen,
            profile_from_keep.first_seen
        );
        assert_eq!(profile_from_absorbed.last_seen, profile_from_keep.last_seen);
        assert_eq!(
            profile_from_absorbed.outbound_ever,
            profile_from_keep.outbound_ever
        );

        let (linked_by, linked_at): (String, Option<String>) =
            sqlx::query_as("SELECT linked_by, linked_at FROM contact_addresses WHERE address = ?")
                .bind(absorbed_address)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(linked_by, "human");
        assert!(linked_at.is_some());
    }

    #[tokio::test]
    async fn fundir_e_desfundir_e_exacto() {
        let pool = test_pool().await;
        let keep_address = "bruno@example.com";
        let absorbed_address = "bruna@example.com";
        let keep_first = "2026-07-10T08:00:00+00:00";
        let keep_last = "2026-07-12T08:00:00+00:00";
        let absorbed_first = "2026-07-11T08:00:00+00:00";
        let absorbed_last = "2026-07-15T08:00:00+00:00";

        let mut transaction = pool.begin().await.unwrap();
        record_inbound(&mut transaction, keep_address, keep_last)
            .await
            .unwrap();
        record_inbound(&mut transaction, keep_address, keep_first)
            .await
            .unwrap();
        record_inbound(&mut transaction, absorbed_address, absorbed_first)
            .await
            .unwrap();
        record_outbound(&mut transaction, &[absorbed_address], absorbed_last)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let keep_id: i64 =
            sqlx::query_scalar("SELECT contact_id FROM contact_addresses WHERE address = ?")
                .bind(keep_address)
                .fetch_one(&pool)
                .await
                .unwrap();
        let absorb_id: i64 =
            sqlx::query_scalar("SELECT contact_id FROM contact_addresses WHERE address = ?")
                .bind(absorbed_address)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_ne!(keep_id, absorb_id);

        let keep_before: (String, String, i64, i64, String, Option<String>) = sqlx::query_as(
            "SELECT first_seen, last_seen, messages_in, outbound_ever, linked_by, linked_at
             FROM contact_addresses
             WHERE address = ?",
        )
        .bind(keep_address)
        .fetch_one(&pool)
        .await
        .unwrap();
        let absorbed_before: (String, String, i64, i64, String, Option<String>) = sqlx::query_as(
            "SELECT first_seen, last_seen, messages_in, outbound_ever, linked_by, linked_at
             FROM contact_addresses
             WHERE address = ?",
        )
        .bind(absorbed_address)
        .fetch_one(&pool)
        .await
        .unwrap();

        merge(&pool, keep_id, absorb_id).await.unwrap();

        let merged_from_keep = profile_for(&pool, keep_address)
            .await
            .unwrap()
            .expect("the kept address must resolve after merging");
        let merged_from_absorbed = profile_for(&pool, absorbed_address)
            .await
            .unwrap()
            .expect("the absorbed address must resolve after merging");
        assert_eq!(merged_from_keep.messages_in, 3);
        assert_eq!(merged_from_keep.first_seen, keep_first);
        assert_eq!(merged_from_keep.last_seen, absorbed_last);
        assert!(merged_from_keep.outbound_ever);
        assert_eq!(
            merged_from_absorbed.messages_in,
            merged_from_keep.messages_in
        );
        assert_eq!(merged_from_absorbed.first_seen, merged_from_keep.first_seen);
        assert_eq!(merged_from_absorbed.last_seen, merged_from_keep.last_seen);
        assert_eq!(
            merged_from_absorbed.outbound_ever,
            merged_from_keep.outbound_ever
        );

        let merged_contact_ids: (i64, i64) = sqlx::query_as(
            "SELECT kept.contact_id, absorbed.contact_id
             FROM contact_addresses AS kept
             JOIN contact_addresses AS absorbed
             WHERE kept.address = ? AND absorbed.address = ?",
        )
        .bind(keep_address)
        .bind(absorbed_address)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(merged_contact_ids, (keep_id, keep_id));

        let (linked_by, linked_at): (String, Option<String>) =
            sqlx::query_as("SELECT linked_by, linked_at FROM contact_addresses WHERE address = ?")
                .bind(absorbed_address)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(linked_by, "human");
        assert!(linked_at.is_some());

        unmerge(&pool, absorbed_address).await.unwrap();

        let keep_after: (String, String, i64, i64, String, Option<String>) = sqlx::query_as(
            "SELECT first_seen, last_seen, messages_in, outbound_ever, linked_by, linked_at
             FROM contact_addresses
             WHERE address = ?",
        )
        .bind(keep_address)
        .fetch_one(&pool)
        .await
        .unwrap();
        let absorbed_after: (String, String, i64, i64, String, Option<String>) = sqlx::query_as(
            "SELECT first_seen, last_seen, messages_in, outbound_ever, linked_by, linked_at
             FROM contact_addresses
             WHERE address = ?",
        )
        .bind(absorbed_address)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(keep_after, keep_before);
        assert_eq!(absorbed_after, absorbed_before);

        let separated_contact_ids: (i64, i64) = sqlx::query_as(
            "SELECT kept.contact_id, absorbed.contact_id
             FROM contact_addresses AS kept
             JOIN contact_addresses AS absorbed
             WHERE kept.address = ? AND absorbed.address = ?",
        )
        .bind(keep_address)
        .bind(absorbed_address)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(separated_contact_ids.0, keep_id);
        assert_ne!(separated_contact_ids.0, separated_contact_ids.1);

        let separated_keep = profile_for(&pool, keep_address)
            .await
            .unwrap()
            .expect("the kept address must retain its original profile");
        assert_eq!(separated_keep.messages_in, 2);
        assert_eq!(separated_keep.first_seen, keep_first);
        assert_eq!(separated_keep.last_seen, keep_last);
        assert!(!separated_keep.outbound_ever);

        let separated_absorbed = profile_for(&pool, absorbed_address)
            .await
            .unwrap()
            .expect("the unmerged address must regain its original profile");
        assert_eq!(separated_absorbed.messages_in, 1);
        assert_eq!(separated_absorbed.first_seen, absorbed_first);
        assert_eq!(separated_absorbed.last_seen, absorbed_last);
        assert!(separated_absorbed.outbound_ever);
    }

    #[tokio::test]
    async fn fundir_nao_move_contadores() {
        let pool = test_pool().await;
        let keep_address = "carlos@example.com";
        let absorbed_address = "carla@example.com";
        let keep_first = "2026-07-01T07:00:00+00:00";
        let keep_last = "2026-07-03T07:00:00+00:00";
        let absorbed_first = "2026-07-02T07:00:00+00:00";
        let absorbed_last = "2026-07-05T07:00:00+00:00";

        let mut transaction = pool.begin().await.unwrap();
        record_inbound(&mut transaction, keep_address, keep_first)
            .await
            .unwrap();
        record_inbound(&mut transaction, keep_address, keep_last)
            .await
            .unwrap();
        record_inbound(&mut transaction, absorbed_address, absorbed_first)
            .await
            .unwrap();
        record_outbound(&mut transaction, &[absorbed_address], absorbed_last)
            .await
            .unwrap();
        transaction.commit().await.unwrap();

        let keep_before: (i64, String, String, i64, i64) = sqlx::query_as(
            "SELECT contact_id, first_seen, last_seen, messages_in, outbound_ever
             FROM contact_addresses
             WHERE address = ?",
        )
        .bind(keep_address)
        .fetch_one(&pool)
        .await
        .unwrap();
        let absorbed_before: (i64, String, String, i64, i64) = sqlx::query_as(
            "SELECT contact_id, first_seen, last_seen, messages_in, outbound_ever
             FROM contact_addresses
             WHERE address = ?",
        )
        .bind(absorbed_address)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_ne!(keep_before.0, absorbed_before.0);

        merge(&pool, keep_before.0, absorbed_before.0)
            .await
            .unwrap();

        let keep_after: (i64, String, String, i64, i64) = sqlx::query_as(
            "SELECT contact_id, first_seen, last_seen, messages_in, outbound_ever
             FROM contact_addresses
             WHERE address = ?",
        )
        .bind(keep_address)
        .fetch_one(&pool)
        .await
        .unwrap();
        let absorbed_after: (i64, String, String, i64, i64) = sqlx::query_as(
            "SELECT contact_id, first_seen, last_seen, messages_in, outbound_ever
             FROM contact_addresses
             WHERE address = ?",
        )
        .bind(absorbed_address)
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(keep_after, keep_before);
        assert_eq!(absorbed_after.0, keep_before.0);
        assert_ne!(absorbed_after.0, absorbed_before.0);
        assert_eq!(absorbed_after.1, absorbed_before.1);
        assert_eq!(absorbed_after.2, absorbed_before.2);
        assert_eq!(absorbed_after.3, absorbed_before.3);
        assert_eq!(absorbed_after.4, absorbed_before.4);

        let (linked_by, linked_at): (String, Option<String>) =
            sqlx::query_as("SELECT linked_by, linked_at FROM contact_addresses WHERE address = ?")
                .bind(absorbed_address)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(linked_by, "human");
        assert!(linked_at.is_some());
    }
}
