//! The account routes. No test starts a login for a real engine: the production adapters would
//! run the real CLI. The login state machine is tested with scripted processes in `login.rs`.

use axum::http::StatusCode;
use serde_json::{Value, json};

use super::super::tests::{test_state, workflow_call};
use super::manage::{logout_in, remove_owned_dir};
use crate::accounts::pool::{Holder, PoolState};
use crate::accounts::{
    Account, AccountId, AccountKind, AccountState, UNPINNED_KIND, account_dir, store,
};
use crate::project::rules::testing::Home;
use crate::state::AppState;

async fn fixture() -> (Home, AppState) {
    let home = Home::new();
    let state = test_state().await;
    (home, state)
}

async fn call(
    state: &AppState,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    workflow_call(state.clone(), method, uri, body).await
}

fn extra(hex: &str, label: &str, state: AccountState) -> Account {
    Account {
        id: AccountId(format!("claude:{hex}")),
        engine: "claude".into(),
        provider: "anthropic".into(),
        label: label.into(),
        kind: AccountKind::Subscription,
        credential_locator: Some("/not/ours".into()),
        state,
        created_at: "t".into(),
        updated_at: "t".into(),
    }
}

#[tokio::test]
async fn the_list_carries_the_migrated_defaults_without_the_credential_locator() {
    let (_home, state) = fixture().await;
    let (status, answer) = call(&state, "GET", "/accounts", None).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let accounts = answer["accounts"].as_array().unwrap();
    let claude = accounts
        .iter()
        .find(|a| a["id"] == "default:claude")
        .unwrap();
    assert_eq!(claude["provider"], "anthropic");
    assert_eq!(claude["state"], "active");
    assert_eq!(claude["is_default"], true);
    assert_eq!(claude["live_sessions"], 0);
    assert!(claude.get("credential_locator").is_none());
}

#[tokio::test]
async fn the_list_names_the_engines_the_pool_can_add() {
    let (_home, state) = fixture().await;
    let (status, answer) = call(&state, "GET", "/accounts", None).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(
        answer["engines"],
        serde_json::json!(crate::accounts::adapters::pooled_engines())
    );
    assert_eq!(answer["engines"], serde_json::json!(["claude", "codex"]));
}

#[tokio::test]
async fn starting_a_login_for_an_engine_outside_the_pool_is_refused() {
    let (_home, state) = fixture().await;
    let body = json!({ "engine": "local", "label": "x" });
    let (status, answer) = call(&state, "POST", "/accounts", Some(body)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{answer}");
    assert_eq!(answer["refusal"], "login_refused");

    for (method, uri) in [
        ("GET", "/account-logins/nope"),
        ("DELETE", "/account-logins/nope"),
    ] {
        let (status, _) = call(&state, method, uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
    }
    let (status, _) = call(
        &state,
        "POST",
        "/account-logins/nope/code",
        Some(json!({ "code": "1" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&state, "POST", "/accounts/claude:00000000/login", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn rename_changes_the_label_and_refuses_a_taken_or_bad_one() {
    let (_home, state) = fixture().await;
    store::insert(
        &state.pool,
        &extra("00000001", "work", AccountState::Active),
    )
    .await
    .unwrap();
    let uri = "/accounts/claude:00000001/rename";
    let (status, answer) = call(&state, "POST", uri, Some(json!({ "label": " home " }))).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["label"], "home");

    // `default` is the migrated claude login's label, and a label is unique per engine.
    let (status, answer) = call(&state, "POST", uri, Some(json!({ "label": "default" }))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_eq!(answer["refusal"], "label_taken");

    let (status, _) = call(&state, "POST", uri, Some(json!({ "label": "  " }))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = call(
        &state,
        "POST",
        "/accounts/claude:ffffffff/rename",
        Some(json!({ "label": "x" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn pause_and_resume_move_between_active_and_paused_but_not_out_of_needs_login() {
    let (_home, state) = fixture().await;
    store::insert(
        &state.pool,
        &extra("00000001", "work", AccountState::Active),
    )
    .await
    .unwrap();
    store::insert(
        &state.pool,
        &extra("00000002", "old", AccountState::NeedsLogin),
    )
    .await
    .unwrap();
    let (status, answer) = call(&state, "POST", "/accounts/claude:00000001/pause", None).await;
    assert_eq!(
        (status, &answer["state"]),
        (StatusCode::OK, &json!("paused"))
    );
    let (status, answer) = call(&state, "POST", "/accounts/claude:00000001/pause", None).await;
    assert_eq!(
        (status, &answer["state"]),
        (StatusCode::OK, &json!("paused"))
    );
    let (status, answer) = call(&state, "POST", "/accounts/claude:00000001/resume", None).await;
    assert_eq!(
        (status, &answer["state"]),
        (StatusCode::OK, &json!("active"))
    );

    let (status, answer) = call(&state, "POST", "/accounts/claude:00000002/resume", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_eq!(answer["refusal"], "needs_login");
    let (status, _) = call(&state, "POST", "/accounts/claude:ffffffff/pause", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_default_choice_is_read_written_and_checked_against_the_accounts() {
    let (_home, state) = fixture().await;
    store::insert(
        &state.pool,
        &extra("00000001", "work", AccountState::Active),
    )
    .await
    .unwrap();
    let (status, answer) = call(&state, "GET", "/account-pool/default", None).await;
    assert_eq!(
        (status, &answer["choice"]),
        (StatusCode::OK, &json!("auto"))
    );

    for (choice, expected) in [
        ("account:claude:00000001", StatusCode::OK),
        ("provider:anthropic", StatusCode::OK),
        ("provider:nobody", StatusCode::UNPROCESSABLE_ENTITY),
        ("account:claude:ffffffff", StatusCode::UNPROCESSABLE_ENTITY),
        ("bogus", StatusCode::UNPROCESSABLE_ENTITY),
    ] {
        let (status, answer) = call(
            &state,
            "PUT",
            "/account-pool/default",
            Some(json!({ "choice": choice })),
        )
        .await;
        assert_eq!(status, expected, "{choice}: {answer}");
    }
    let (_, answer) = call(&state, "GET", "/account-pool/default", None).await;
    assert_eq!(
        answer["choice"], "provider:anthropic",
        "a refused write changes nothing"
    );
}

#[tokio::test]
async fn a_chat_is_pinned_and_unpinned_and_unknown_targets_are_refused() {
    let (_home, state) = fixture().await;
    store::insert(
        &state.pool,
        &extra("00000001", "work", AccountState::Active),
    )
    .await
    .unwrap();
    let chat = crate::chats::create(&state.pool, crate::chats::Brain::Cloud, None)
        .await
        .unwrap();
    let uri = format!("/assistant/chats/{chat}/account");

    let (status, answer) = call(&state, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(
        (&answer["choice"], &answer["inherited"]),
        (&json!("auto"), &json!(true))
    );

    let (status, answer) = call(
        &state,
        "PUT",
        &uri,
        Some(json!({ "choice": "account:claude:00000001" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    let (_, answer) = call(&state, "GET", &uri, None).await;
    assert_eq!(answer["choice"], "account:claude:00000001");
    assert_eq!(answer["inherited"], false);

    let (status, _) = call(
        &state,
        "PUT",
        &uri,
        Some(json!({ "choice": "account:claude:ffffffff" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = call(
        &state,
        "PUT",
        "/assistant/chats/nope/account",
        Some(json!({ "choice": "auto" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&state, "PUT", &uri, Some(json!({ "choice": "auto" }))).await;
    assert_eq!(status, StatusCode::OK);
    let (_, answer) = call(&state, "GET", &uri, None).await;
    assert_eq!(answer["choice"], "auto");
}

#[tokio::test]
async fn removing_an_account_unpins_its_chats_resets_the_default_and_tells_the_feed() {
    let (_home, state) = fixture().await;
    store::insert(
        &state.pool,
        &extra("00000001", "work", AccountState::Active),
    )
    .await
    .unwrap();
    let chat = crate::chats::create(&state.pool, crate::chats::Brain::Cloud, None)
        .await
        .unwrap();
    let pinned = json!({ "choice": "account:claude:00000001" });
    call(
        &state,
        "PUT",
        &format!("/assistant/chats/{chat}/account"),
        Some(pinned.clone()),
    )
    .await;
    call(&state, "PUT", "/account-pool/default", Some(pinned)).await;

    let (status, answer) = call(&state, "DELETE", "/accounts/default:claude", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_eq!(answer["refusal"], "default_account");

    let (status, answer) = call(&state, "DELETE", "/accounts/claude:00000001", None).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["unpinned_chats"], 1);
    assert_eq!(answer["default_reset"], true);

    let (_, answer) = call(
        &state,
        "GET",
        &format!("/assistant/chats/{chat}/account"),
        None,
    )
    .await;
    assert_eq!(answer["choice"], "auto");
    let (_, answer) = call(&state, "GET", "/account-pool/default", None).await;
    assert_eq!(answer["choice"], "auto");
    assert!(
        store::get(&state.pool, &AccountId("claude:00000001".into()))
            .await
            .unwrap()
            .is_none()
    );
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE kind = ?")
        .bind(UNPINNED_KIND)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(rows, 1);
    let (status, _) = call(&state, "DELETE", "/accounts/claude:00000001", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_account_with_live_sessions_is_removed_only_when_forced() {
    let (_home, state) = fixture().await;
    store::insert(
        &state.pool,
        &extra("00000003", "busy", AccountState::Active),
    )
    .await
    .unwrap();
    let id = AccountId("claude:00000003".into());
    let reservation = PoolState::global().reserve(&id, Holder::Chat("live-chat".into()));

    let (status, answer) = call(&state, "DELETE", "/accounts/claude:00000003", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_eq!(
        (&answer["refusal"], &answer["live_sessions"]),
        (&json!("live_sessions"), &json!(1))
    );
    let (status, _) = call(
        &state,
        "DELETE",
        "/accounts/claude:00000003?force=true",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    drop(reservation);
}

#[tokio::test]
async fn only_the_directory_nucleos_made_is_deleted_with_the_account() {
    let data = tempfile::tempdir().unwrap();
    let mut account = extra("00000009", "x", AccountState::Active);
    let ours = account_dir(data.path(), &account.id);
    std::fs::create_dir_all(&ours).unwrap();
    let theirs = tempfile::tempdir().unwrap();

    account.credential_locator = Some(theirs.path().to_string_lossy().into_owned());
    remove_owned_dir(data.path(), &account).await;
    assert!(
        ours.exists() && theirs.path().exists(),
        "a foreign locator deletes nothing"
    );

    account.credential_locator = Some(ours.to_string_lossy().into_owned());
    remove_owned_dir(data.path(), &account).await;
    assert!(!ours.exists());
    assert!(theirs.path().exists());
}

#[tokio::test]
async fn logout_deletes_the_credential_file_and_marks_the_account_needs_login() {
    let (_home, state) = fixture().await;
    let data = tempfile::tempdir().unwrap();
    let mut account = extra("0000000a", "work", AccountState::Active);
    let ours = account_dir(data.path(), &account.id);
    std::fs::create_dir_all(&ours).unwrap();
    let credentials = ours.join(".credentials.json");
    std::fs::write(&credentials, "{}").unwrap();
    std::fs::write(ours.join("history.txt"), "kept").unwrap();
    account.credential_locator = Some(ours.to_string_lossy().into_owned());
    store::insert(&state.pool, &account).await.unwrap();

    let answer = logout_in(&state, account.id.as_str(), data.path())
        .await
        .unwrap();
    assert_eq!(answer.0["state"], "needs_login");
    assert!(!credentials.exists());
    assert!(ours.join("history.txt").exists(), "only credentials go");
    let stored = store::get(&state.pool, &account.id).await.unwrap().unwrap();
    assert_eq!(stored.state, AccountState::NeedsLogin);

    // A second logout finds nothing to delete and still succeeds.
    let again = logout_in(&state, account.id.as_str(), data.path()).await;
    assert!(again.is_ok());
}

#[tokio::test]
async fn logout_of_an_account_without_its_own_credentials_is_refused_and_deletes_nothing() {
    let (_home, state) = fixture().await;
    let data = tempfile::tempdir().unwrap();
    let theirs = tempfile::tempdir().unwrap();
    let foreign = theirs.path().join(".credentials.json");
    std::fs::write(&foreign, "{}").unwrap();

    // A locator outside the account's own directory, and no locator at all.
    let mut outside = extra("0000000b", "outside", AccountState::Active);
    outside.credential_locator = Some(theirs.path().to_string_lossy().into_owned());
    let mut bare = extra("0000000c", "bare", AccountState::Active);
    bare.credential_locator = None;
    for account in [&outside, &bare] {
        store::insert(&state.pool, account).await.unwrap();
        let (status, answer) = logout_in(&state, account.id.as_str(), data.path())
            .await
            .unwrap_err();
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{answer}");
        assert_eq!(answer["refusal"], "logout_not_owned");
        let stored = store::get(&state.pool, &account.id).await.unwrap().unwrap();
        assert_eq!(stored.state, AccountState::Active);
    }
    assert!(foreign.exists());

    // The same refusal through the route, whichever data directory the daemon resolves.
    let (status, answer) = call(&state, "POST", "/accounts/claude:0000000c/logout", None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{answer}");
    assert_eq!(answer["refusal"], "logout_not_owned");
}

#[tokio::test]
async fn logout_of_an_unknown_account_is_not_found() {
    let (_home, state) = fixture().await;
    let (status, answer) = call(&state, "POST", "/accounts/claude:ffffffff/logout", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{answer}");
}

#[tokio::test]
async fn logout_is_refused_while_the_account_has_live_sessions() {
    let (_home, state) = fixture().await;
    let data = tempfile::tempdir().unwrap();
    let mut account = extra("0000000d", "busy", AccountState::Active);
    let ours = account_dir(data.path(), &account.id);
    std::fs::create_dir_all(&ours).unwrap();
    let credentials = ours.join(".credentials.json");
    std::fs::write(&credentials, "{}").unwrap();
    account.credential_locator = Some(ours.to_string_lossy().into_owned());
    store::insert(&state.pool, &account).await.unwrap();
    let reservation = PoolState::global().reserve(&account.id, Holder::Chat("live-chat".into()));

    let (status, answer) = logout_in(&state, account.id.as_str(), data.path())
        .await
        .unwrap_err();
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_eq!(answer["refusal"], "live_sessions");
    assert!(credentials.exists());
    drop(reservation);
}
