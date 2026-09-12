//! Starts only the file server, for manual checks against a scratch vault.
//!
//! `cargo run --example files_server -- <vault> <port>`

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let vault = args.next().expect("usage: files_server <vault> <port>");
    let port: u16 = args
        .next()
        .expect("usage: files_server <vault> <port>")
        .parse()?;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    println!("serving {vault} on {port}");
    spacenotes::files_http::serve(vault.into(), listener).await
}
