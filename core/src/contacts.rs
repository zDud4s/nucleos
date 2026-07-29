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
}
