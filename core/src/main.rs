mod http;

#[tokio::main]
async fn main() {
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
