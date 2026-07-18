mod auth;
mod http;
mod secrets;
mod storage;

use auth::Token;

const TOKEN_KEY: &str = "daemon-token";

#[tokio::main]
async fn main() {
    // Dev affordance: `nucleos-core --print-token` reads the token back out of the OS Credential
    // Manager and prints only that, so smoke tests can grab it without a plaintext token file.
    if std::env::args().any(|a| a == "--print-token") {
        match secrets::load_secret(TOKEN_KEY) {
            Ok(Some(t)) => println!("{t}"),
            Ok(None) => {
                eprintln!("no daemon token stored yet — start the daemon once to generate one");
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("failed to read token from Credential Manager: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    let dirs = directories::ProjectDirs::from("dev", "nucleos", "NucleOS")
        .expect("could not resolve local app data directory");

    let db_path = dirs.data_local_dir().join("nucleos.db");
    let _pool = storage::open(&db_path)
        .await
        .expect("failed to open local database");
    println!("nucleos-core database ready at {}", db_path.display());

    // The token lives in the OS Credential Manager (spec §3.4/§5), not a file on disk — generated
    // once on first run, loaded back on every start after that.
    let token_value =
        match secrets::load_secret(TOKEN_KEY).expect("failed to read Credential Manager") {
            Some(existing) => existing,
            None => {
                let fresh = auth::generate_token();
                secrets::store_secret(TOKEN_KEY, &fresh).expect("failed to persist daemon token");
                fresh
            }
        };
    println!("nucleos-core token loaded from Credential Manager");
    let token = Token(token_value);

    let app = http::build_router(token);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8791")
        .await
        .unwrap();
    println!(
        "nucleos-core listening on {}",
        listener.local_addr().unwrap()
    );
    axum::serve(listener, app).await.unwrap();
}
