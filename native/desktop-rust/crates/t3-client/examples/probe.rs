//! Connects to a running T3 Code server and prints what the desktop app would see.
//!
//! ```text
//! cargo run -p t3-client --example probe -- http://127.0.0.1:3773/ <bootstrap-or-pairing-token> [new-project-dir]
//! ```

use std::time::Duration;

use futures::StreamExt;
use t3_client::{AuthClient, ClientMetadata, Connection, Keepalive};
use t3_contracts::{ShellStreamItem, ThreadStreamItem};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(http_base_url), Some(credential)) = (args.next(), args.next()) else {
        eprintln!("usage: probe <http-base-url> <bootstrap-or-pairing-token>");
        std::process::exit(2);
    };
    let ws_base_url = http_base_url.replacen("http", "ws", 1);
    let auth = AuthClient::default();
    let metadata = ClientMetadata::desktop();
    let token = auth
        .exchange_bootstrap_credential(&http_base_url, &credential, &metadata)
        .await?;
    println!(
        "authorized: {} token, scope {:?}",
        token.token_type, token.scope
    );
    let url = auth
        .websocket_connection_url(&http_base_url, &ws_base_url, &token.access_token, &metadata)
        .await?;
    let connection =
        Connection::connect(&url, Duration::from_secs(15), Keepalive::default()).await?;
    let config = connection.get_config().await?;
    println!(
        "environment {:?}, cwd {:?}, {} providers ({} selectable)",
        config.environment_label(),
        config.cwd,
        config.providers.len(),
        config.selectable_providers().count()
    );
    let new_project = args.next();
    let mut shell = connection.subscribe_shell(None);
    let mut first_thread = None;
    while let Some(item) = tokio::time::timeout(Duration::from_secs(10), shell.next()).await? {
        match item? {
            ShellStreamItem::Snapshot { snapshot, .. } => {
                println!(
                    "shell snapshot @{}: {} projects, {} threads",
                    snapshot.snapshot_sequence,
                    snapshot.projects.len(),
                    snapshot.threads.len()
                );
                first_thread = snapshot.threads.first().map(|thread| thread.id.clone());
            }
            ShellStreamItem::Synchronized => {
                println!("shell synchronized");
                let Some(path) = new_project.as_deref() else {
                    break;
                };
                let project = connection
                    .create_project(&t3_contracts::ProjectCreate::for_path(path))
                    .await?;
                println!("created project {} ({})", project.title, project.id);
            }
            ShellStreamItem::ProjectUpdated { sequence, project } => {
                println!(
                    "shell @{sequence}: project.updated {} at {}",
                    project.title, project.workspace_root
                );
                break;
            }
            other => println!("shell item: {other:?}"),
        }
    }
    if let Some(thread_id) = first_thread {
        let mut thread = connection.subscribe_thread(&thread_id, None);
        while let Some(item) = tokio::time::timeout(Duration::from_secs(10), thread.next()).await? {
            match item? {
                ThreadStreamItem::Snapshot {
                    projection,
                    snapshot_sequence,
                    ..
                } => {
                    let transcript = t3_state::transcript::build_transcript(&projection);
                    println!(
                        "thread {thread_id} @{snapshot_sequence}: {} turn items, {} transcript entries",
                        projection.turn_items.len(),
                        transcript.len()
                    );
                }
                ThreadStreamItem::Synchronized => break,
                other => println!("thread item: {other:?}"),
            }
        }
    }
    Ok(())
}
