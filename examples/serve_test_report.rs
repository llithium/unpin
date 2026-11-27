//! Serve a saved scan through the review server for browser-level tests,
//! with Pinterest replaced by a local stub at `--api-root`.

use std::path::PathBuf;

use clap::Parser;
use unpin::pinterest::PinterestClient;
use unpin::review_store::ReviewStore;
use unpin::server::ReviewServer;
use url::Url;

#[derive(Debug, Parser)]
struct Args {
    /// Saved scan JSON.
    input: PathBuf,
    /// Base URL of the stub that answers Pinterest resource requests.
    #[arg(long)]
    api_root: Option<Url>,
    /// Review file to use instead of the user's own.
    #[arg(long)]
    store: PathBuf,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let report = unpin::saved_report::read(&args.input).expect("read saved scan");
    let client = args.api_root.map(|api_root| {
        PinterestClient::with_api_root(Url::parse("https://www.pinterest.com/").unwrap(), api_root)
            .expect("stub client")
    });
    let server = ReviewServer::bind(report, client, ReviewStore::new(args.store), Some(0))
        .await
        .expect("bind review server");
    // The test reads this line to find the server.
    println!("{}", server.url());
    server.run().await.expect("serve");
}
