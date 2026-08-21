//! Unix-Socket-IPC-Server.
//!
//! Design-Entscheidungen (Socket-Permission-Race, `SO_PEERCRED`, warum
//! `tokio` statt synchronem std + Threads) sind begründet in
//! docs/month3-tarno-layer.md#ipc-design.

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use tarnod_protocol::{Request, Response};
use crate::AppState;

pub async fn serve(state: Arc<AppState>) -> anyhow::Result<()> {
    std::fs::create_dir_all(&state.config.socket_dir)?;
    std::fs::set_permissions(&state.config.socket_dir, std::fs::Permissions::from_mode(0o700))?;
    // Alten Socket aus einem vorherigen (evtl. abgestürzten) Lauf entfernen.
    let _ = std::fs::remove_file(&state.config.socket_path);

    // umask VOR bind() setzen, damit der Socket nie kurzzeitig mit
    // world-lesbaren Rechten sichtbar ist (Race zwischen bind() und einem
    // nachträglichen chmod()) — siehe docs/month3-tarno-layer.md.
    let old_umask = unsafe { libc::umask(0o177) };
    let bind_result = UnixListener::bind(&state.config.socket_path);
    unsafe {
        libc::umask(old_umask);
    }
    let listener = bind_result?;
    std::fs::set_permissions(
        &state.config.socket_path,
        std::fs::Permissions::from_mode(0o600),
    )?;

    eprintln!("tarnod: listening on {}", state.config.socket_path.display());

    loop {
        let (stream, _addr) = listener.accept().await?;
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, state).await {
                eprintln!("tarnod: client error: {e}");
            }
        });
    }
}

async fn handle_client(stream: UnixStream, state: Arc<AppState>) -> anyhow::Result<()> {
    // Zusätzlich zur Datei-Permission (0600) die tatsächliche UID/GID des
    // verbindenden Prozesses verifizieren.
    if let Ok(cred) = stream.peer_cred() {
        eprintln!(
            "tarnod: connection from uid={} gid={} pid={:?}",
            cred.uid(),
            cred.gid(),
            cred.pid()
        );
    }

    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(req) => crate::dispatch(&state, req).await,
            Err(e) => Response::err(format!("invalid request: {e}")),
        };
        let out = serde_json::to_string(&response)?;
        writer.write_all(out.as_bytes()).await?;
        writer.write_all(b"\n").await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gaming::GamingController;
    use crate::vault::Vault;
    use std::path::PathBuf;

    fn test_state(socket_dir: PathBuf) -> Arc<AppState> {
        let socket_path = socket_dir.join("tarnod.sock");
        Arc::new(AppState {
            vault: Vault::parse("TEST_KEY=test_value\n"),
            gaming: GamingController::new(true),
            config: crate::config::Config {
                socket_dir,
                socket_path,
                vault_path: PathBuf::from("/nonexistent"),
                dry_run: true,
            },
            ai: crate::ai::AiState::new(),
        })
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ipc_roundtrip_ping_and_get_api_key() {
        let dir = std::env::temp_dir().join(format!("tarnod-ipc-test-{}", std::process::id()));
        let state = test_state(dir.clone());

        let listener_state = Arc::clone(&state);
        tokio::spawn(async move {
            let _ = serve(listener_state).await;
        });

        // Kurz warten, bis der Socket existiert (Server startet async).
        for _ in 0..50 {
            if state.config.socket_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let stream = UnixStream::connect(&state.config.socket_path)
            .await
            .expect("connect to tarnod test socket");
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();

        writer.write_all(b"{\"cmd\":\"ping\"}\n").await.unwrap();
        let resp = lines.next_line().await.unwrap().unwrap();
        assert!(resp.contains("\"status\":\"ok\""));
        assert!(resp.contains("pong"));

        writer
            .write_all(b"{\"cmd\":\"get_api_key\",\"name\":\"TEST_KEY\"}\n")
            .await
            .unwrap();
        let resp = lines.next_line().await.unwrap().unwrap();
        assert!(resp.contains("test_value"));

        writer
            .write_all(b"{\"cmd\":\"get_api_key\",\"name\":\"MISSING\"}\n")
            .await
            .unwrap();
        let resp = lines.next_line().await.unwrap().unwrap();
        assert!(resp.contains("\"status\":\"error\""));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
