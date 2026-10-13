//! Runs the fake Google Drive used by the tests as a standalone server, for
//! trying the desktop app without a Google account:
//!
//!   cargo run --example mock_drive -- 127.0.0.1:8099 [put_delay_ms]
//!
//! Point a config's [endpoints] at http://127.0.0.1:8099 (auth_url = …/auth,
//! token_url = …/token, api_base = …/drive/v3, upload_base = …/upload/drive/v3)
//! and use refresh token "good-refresh". Prints the Drive tree every few seconds.

#[path = "../tests/common/mod.rs"]
mod common;

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let addr = args.next().unwrap_or_else(|| "127.0.0.1:8099".into());
    let delay: u64 = args.next().and_then(|d| d.parse().ok()).unwrap_or(0);
    let mock = common::start_mock_at(&addr).await;
    mock.lock().unwrap().put_delay_ms = delay;
    println!("fake Drive at http://{addr}");
    let mut last = Vec::new();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let st = mock.lock().unwrap();
        let mut tree = Vec::new();
        st.tree("root", "", &mut tree);
        if tree != last {
            println!("--- My Drive ---");
            for (p, _) in &tree {
                println!("  {p}");
            }
            last = tree;
        }
    }
}
