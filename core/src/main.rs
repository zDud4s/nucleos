mod http;
mod storage;

#[tokio::main]
async fn main() {
    let dirs = directories::ProjectDirs::from("dev", "nucleos", "NucleOS")
        .expect("could not resolve local app data directory");
    let db_path = dirs.data_local_dir().join("nucleos.db");
    let _pool = storage::open(&db_path)
        .await
        .expect("failed to open local database");
    println!("nucleos-core database ready at {}", db_path.display());

    let app = http::build_router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8791")
        .await
        .unwrap();
    println!(
        "nucleos-core listening on {}",
        listener.local_addr().unwrap()
    );
    axum::serve(listener, app).await.unwrap();
}
