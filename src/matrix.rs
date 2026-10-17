//! Matrix side: login / session persistence, invite handling, and turning
//! room messages into replies.

use crate::config::{Config, MatrixConfig};
use crate::providers::USER_AGENT;
use crate::translate::{self, Translator};
use anyhow::{Context as _, Result, ensure};
use matrix_sdk::authentication::matrix::MatrixSession;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::event_handler::Ctx;
use matrix_sdk::ruma::events::room::member::StrippedRoomMemberEvent;
use matrix_sdk::ruma::events::room::message::{
    AddMentions, ForwardThread, MessageType, OriginalSyncRoomMessageEvent, Relation,
    RoomMessageEventContent,
};
use matrix_sdk::ruma::{MilliSecondsSinceUnixEpoch, OwnedRoomId, OwnedUserId, RoomId, UserId};
use matrix_sdk::store::RoomLoadSettings;
use matrix_sdk::{Client, Room, RoomState};
use std::sync::Arc;
use std::time::Duration;

struct BotCtx {
    translator: Translator,
    allowed_rooms: Vec<OwnedRoomId>,
    auto_join_from: Vec<OwnedUserId>,
    reply_on_miss: bool,
    /// Messages older than this (i.e. backlog delivered by the first sync) are ignored.
    started_at: MilliSecondsSinceUnixEpoch,
}

/// Build the client and either restore the saved session or log in once with
/// the password. The session (access token + device id) is persisted next to
/// the sqlite stores so restarts keep the same E2EE device.
pub async fn build_client(cfg: &MatrixConfig) -> Result<Client> {
    std::fs::create_dir_all(&cfg.data_dir)
        .with_context(|| format!("creating data dir {}", cfg.data_dir.display()))?;

    let client = Client::builder()
        .homeserver_url(&cfg.homeserver)
        .sqlite_store(cfg.data_dir.join("store"), None)
        .user_agent(USER_AGENT)
        .build()
        .await
        .context("building matrix client")?;

    let session_path = cfg.data_dir.join("session.json");
    if session_path.exists() {
        let raw = std::fs::read_to_string(&session_path).context("reading session.json")?;
        let session: MatrixSession = serde_json::from_str(&raw).context("parsing session.json")?;
        client
            .matrix_auth()
            .restore_session(session, RoomLoadSettings::default())
            .await
            .context("restoring saved session (delete session.json to log in afresh)")?;
        tracing::info!(
            user = %client.user_id().map(|u| u.to_string()).unwrap_or_default(),
            device = %client.device_id().map(|d| d.to_string()).unwrap_or_default(),
            "restored session"
        );
    } else {
        ensure!(
            !cfg.password.is_empty(),
            "no saved session in {} and matrix.password is empty",
            cfg.data_dir.display()
        );
        client
            .matrix_auth()
            .login_username(&cfg.user_id, &cfg.password)
            .initial_device_display_name(&cfg.device_name)
            .send()
            .await
            .context("password login failed")?;
        let session = client
            .matrix_auth()
            .session()
            .context("no session after login")?;
        write_private(&session_path, &serde_json::to_string_pretty(&session)?)?;
        tracing::info!(
            user = %session.meta.user_id,
            device = %session.meta.device_id,
            path = %session_path.display(),
            "logged in and saved session"
        );
    }
    Ok(client)
}

fn write_private(path: &std::path::Path, contents: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("writing {}", path.display()))?;
    f.write_all(contents.as_bytes())?;
    Ok(())
}

/// Log in, register handlers and sync until Ctrl-C / SIGTERM.
pub async fn run(cfg: Config) -> Result<()> {
    let translator = Translator::new(&cfg)?;
    let client = build_client(&cfg.matrix).await?;

    let allowed_rooms = cfg
        .matrix
        .allowed_rooms
        .iter()
        .map(|r| RoomId::parse(r).with_context(|| format!("bad room id in allowed_rooms: {r}")))
        .collect::<Result<Vec<_>>>()?;
    let auto_join_from = cfg
        .matrix
        .auto_join_invites_from
        .iter()
        .map(|u| {
            UserId::parse(u).with_context(|| format!("bad user id in auto_join_invites_from: {u}"))
        })
        .collect::<Result<Vec<_>>>()?;

    client.add_event_handler_context(Arc::new(BotCtx {
        translator,
        allowed_rooms,
        auto_join_from,
        reply_on_miss: cfg.behaviour.reply_on_miss,
        started_at: MilliSecondsSinceUnixEpoch::now(),
    }));
    client.add_event_handler(on_invite);
    client.add_event_handler(on_message);

    tracing::info!(
        homeserver = %cfg.matrix.homeserver,
        targets = ?cfg.behaviour.targets,
        "tunebridge is syncing"
    );
    let settings = SyncSettings::default().timeout(Duration::from_secs(30));
    tokio::select! {
        r = client.sync(settings) => r.context("sync loop ended")?,
        _ = shutdown_signal() => tracing::info!("shutdown signal received, exiting"),
    }
    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = term.recv() => {},
    }
}

async fn on_invite(ev: StrippedRoomMemberEvent, room: Room, client: Client, ctx: Ctx<Arc<BotCtx>>) {
    if Some(ev.state_key.as_ref()) != client.user_id() || room.state() != RoomState::Invited {
        return;
    }
    if !ctx.auto_join_from.iter().any(|u| u == &ev.sender) {
        tracing::warn!(room = %room.room_id(), from = %ev.sender, "ignoring invite from user not in auto_join_invites_from");
        return;
    }
    // Synapse occasionally needs a moment before the join is accepted.
    let mut delay = Duration::from_secs(2);
    for attempt in 1..=5 {
        match room.join().await {
            Ok(()) => {
                tracing::info!(room = %room.room_id(), from = %ev.sender, "joined room on invite");
                return;
            }
            Err(e) => {
                tracing::warn!(room = %room.room_id(), attempt, error = %e, "join failed, retrying");
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
        }
    }
    tracing::error!(room = %room.room_id(), "giving up joining room");
}

async fn on_message(
    ev: OriginalSyncRoomMessageEvent,
    room: Room,
    client: Client,
    ctx: Ctx<Arc<BotCtx>>,
) {
    if room.state() != RoomState::Joined || Some(ev.sender.as_ref()) == client.user_id() {
        return;
    }
    if ev.origin_server_ts < ctx.started_at {
        return; // backlog from the initial sync
    }
    if !ctx.allowed_rooms.is_empty() && !ctx.allowed_rooms.iter().any(|r| r == room.room_id()) {
        return;
    }
    if matches!(ev.content.relates_to, Some(Relation::Replacement(_))) {
        return; // an edit; we already answered the original
    }
    let (body, formatted) = match &ev.content.msgtype {
        MessageType::Text(t) => (
            t.body.as_str(),
            t.formatted.as_ref().map(|f| f.body.as_str()),
        ),
        MessageType::Notice(n) => (
            n.body.as_str(),
            n.formatted.as_ref().map(|f| f.body.as_str()),
        ),
        _ => return,
    };

    let replies = ctx.translator.translate_message(body, formatted).await;
    let Some((text, html)) = translate::format_replies(&replies, ctx.reply_on_miss) else {
        return;
    };
    let content = RoomMessageEventContent::text_html(text, html).make_reply_to(
        &ev,
        ForwardThread::Yes,
        AddMentions::No,
    );
    match room.send(content).await {
        Ok(resp) => {
            tracing::info!(room = %room.room_id(), in_reply_to = %ev.event_id, event = %resp.response.event_id, "sent reply")
        }
        Err(e) => tracing::error!(room = %room.room_id(), error = %e, "failed to send reply"),
    }
}
