use std::{collections::HashMap, convert::Infallible, os::unix::fs::PermissionsExt};

use chrono::Utc;
use frankenstein::{
    Animation, AsyncTelegramApi, Message, Poll, SendMessageParams, Update, UpdateContent,
};
use hyper::{
    body::Buf,
    service::{make_service_fn, service_fn},
    Body, Method, Request, Response, Server, StatusCode,
};
use secstr::SecStr;
use tokio::{
    net::UnixListener,
    sync::mpsc::{error::TryRecvError, unbounded_channel, UnboundedReceiver, UnboundedSender},
};
use tokio_stream::wrappers::UnixListenerStream;

use crate::{
    animation::{
        generate_thumbnail, get_animation_params, save_animation, GenerateThumbnailError,
        GetAnimationParamsError, SaveAnimationError,
    },
    command::{handle_command, parse_command},
    util::{unexpected_error_reply, Kaomoji},
    API, CONFIG, DB,
};

#[derive(Debug, thiserror::Error)]
pub enum WebhookListenerError {
    #[error("webhook server error: {0}")]
    ServerError(#[from] hyper::Error),
    #[error("failed to bind socket: {0}")]
    SocketBindError(std::io::Error),
    #[error("failed to set socket permissions: {0}")]
    SocketSetPermissionsError(std::io::Error),
}

pub async fn listen() -> Result<(), WebhookListenerError> {
    let config = CONFIG.wait();

    let (poll_update_tx, poll_update_rx) = unbounded_channel::<(u32, Poll)>();

    let service = make_service_fn(move |_conn| {
        let poll_update_tx = poll_update_tx.clone();
        async move {
            Ok::<_, Infallible>(service_fn(move |req| {
                let poll_update_tx = poll_update_tx.clone();
                async move { handle_request(req, &poll_update_tx).await }
            }))
        }
    });

    _ = std::fs::remove_file(&config.webhook.socket_path);

    let listener = UnixListener::bind(&config.webhook.socket_path)
        .map_err(WebhookListenerError::SocketBindError)?;

    std::fs::set_permissions(
        &config.webhook.socket_path,
        std::fs::Permissions::from_mode(config.webhook.socket_permissions),
    )
    .map_err(WebhookListenerError::SocketSetPermissionsError)?;
    let acceptor = hyper::server::accept::from_stream(UnixListenerStream::new(listener));

    let handle_poll_updates_thread = tokio::spawn(handle_poll_updates(poll_update_rx));
    let server_thread = tokio::spawn(Server::builder(acceptor).serve(service));

    match tokio::try_join!(handle_poll_updates_thread, server_thread) {
        Ok(results) => match results {
            (Ok(()), Ok(())) => {
                eprintln!("webhook threads exited");
            }
            (Err(err), _) => {
                eprintln!("handle_poll_updates thread failed: {err}");
            }
            (_, Err(err)) => {
                eprintln!("server thread failed: {err}");
            }
        },
        Err(err) => {
            eprintln!("try_join! in webhook listener failed: {err}");
        }
    }

    Ok(())
}

fn empty_response(status: StatusCode) -> Result<Response<Body>, hyper::http::Error> {
    Response::builder().status(status).body(Body::empty())
}

async fn handle_request(
    req: Request<Body>,
    poll_update_tx: &UnboundedSender<(u32, Poll)>,
) -> Result<Response<Body>, hyper::http::Error> {
    let config = CONFIG.wait();

    if req.method() != Method::POST {
        return empty_response(StatusCode::NOT_FOUND);
    }
    let secret_header = match req.headers().get("X-Telegram-Bot-Api-Secret-Token") {
        Some(header) => SecStr::new(header.as_bytes().to_vec()),
        None => return empty_response(StatusCode::NOT_FOUND),
    };
    if secret_header != SecStr::new(config.webhook.secret.clone().into()) {
        return empty_response(StatusCode::NOT_FOUND);
    }

    let body = match hyper::body::aggregate(req.into_body()).await {
        Ok(body) => body,
        Err(err) => {
            eprintln!("failed to read update body: {}", err);
            return empty_response(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    let update = match serde_json::from_reader::<_, Update>(body.reader()) {
        Ok(update) => update,
        Err(err) => {
            eprintln!("failed to parse update: {}", err);
            return empty_response(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    match update.content {
        UpdateContent::Message(message) => {
            handle_message_update(&message).await;
        }
        UpdateContent::Poll(poll) => {
            if let Err(err) = poll_update_tx.send((update.update_id, poll)) {
                eprintln!("failed to send poll update: {err}");
            }
        }
        _ => eprintln!("unknown update type {:?}", update.content),
    }
    empty_response(StatusCode::OK)
}

async fn handle_message_update(message: &Message) {
    if let Ok(Some(command)) = parse_command(&message) {
        handle_command(&command, &message).await;
    }
    if let Some(animation) = &message.animation {
        if let Err(err) = handle_submission(message, animation).await {
            eprintln!("failed to handle submission: {err}");
            unexpected_error_reply(message).await;
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum HandlePollUpdatesError {
    #[error("poll update channel closed")]
    Disconnected,
}

async fn handle_poll_updates(
    mut poll_update_rx: UnboundedReceiver<(u32, Poll)>,
) -> Result<(), HandlePollUpdatesError> {
    'outer: loop {
        let mut updates = Vec::new();
        match poll_update_rx.recv().await {
            Some(data) => updates.push(data),
            None => break,
        }
        loop {
            match poll_update_rx.try_recv() {
                Ok(update) => updates.push(update),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break 'outer,
            }
        }

        let mut updates_by_poll_id = HashMap::<String, (u32, Poll)>::new();
        for (update_id, poll) in updates {
            let entry = updates_by_poll_id.get_mut(&poll.id);
            if let Some(entry) = entry {
                if entry.0 < update_id {
                    *entry = (update_id, poll);
                }
            } else {
                updates_by_poll_id.insert(poll.id.clone(), (update_id, poll));
            }
        }

        for (_, poll) in updates_by_poll_id.values() {
            if let Err(err) = handle_poll_update(&poll).await {
                eprintln!("failed to handle poll update: {err}");
            }
        }
    }
    Err(HandlePollUpdatesError::Disconnected)
}

#[derive(Debug, thiserror::Error)]
enum HandlePollUpdateError {
    #[error("API error: {0}")]
    ApiError(#[from] frankenstein::Error),
    #[error(transparent)]
    DbError(#[from] deadpool_postgres::tokio_postgres::Error),
    #[error("db integrity error: {0}")]
    DbIntegrityError(String),
    #[error("error converting vote count")]
    TryFromIntError(#[from] std::num::TryFromIntError),
}

async fn handle_poll_update(poll: &Poll) -> Result<(), HandlePollUpdateError> {
    if poll.is_closed {
        // Telegram sends nonsensical vote counts for closed polls, so don't use those
        return Ok(());
    }

    let mut db = DB.wait().lock().await;
    let t = db.transaction().await?;
    let config = CONFIG.wait();

    let mut votes_a: Option<u32> = None;
    let mut votes_b: Option<u32> = None;
    for option in &poll.options {
        if option.text == config.poll.option_a_text {
            if votes_a.is_some() {
                eprintln!("duplicate poll option: {}", option.text);
                return Ok(());
            }
            votes_a = Some(option.voter_count);
        } else if option.text == config.poll.option_b_text {
            if votes_b.is_some() {
                eprintln!("duplicate poll option: {}", option.text);
                return Ok(());
            }
            votes_b = Some(option.voter_count);
        }
    }
    let (votes_a, votes_b) = match (votes_a, votes_b) {
        (Some(votes_a), Some(votes_b)) => (votes_a, votes_b),
        _ => {
            eprintln!("missing poll option");
            return Ok(());
        }
    };

    let count = t
        .execute(
            r#"
            UPDATE "matchups" SET "animation_a_votes" = $1, "animation_b_votes" = $2
            WHERE "poll_id" = $3 AND "state" = 'started'
            "#,
            &[&i32::try_from(votes_a)?, &i32::try_from(votes_b)?, &poll.id],
        )
        .await?;
    if count > 1 {
        return Err(HandlePollUpdateError::DbIntegrityError(format!(
            "{count} rows updated"
        )));
    }
    t.commit().await?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum HandleSubmissionError {
    #[error("API error: {0}")]
    ApiError(#[from] frankenstein::Error),
    #[error(transparent)]
    DbError(#[from] deadpool_postgres::tokio_postgres::Error),
    #[error("db integrity error: {0}")]
    DbIntegrityError(String),
    #[error("failed to generate thumbnail: {0}")]
    GenerateThumbnailError(#[from] GenerateThumbnailError),
    #[error("failed to get animation params: {0}")]
    GetAnimationParamsError(#[from] GetAnimationParamsError),
    #[error("invalid user ID: {0}")]
    InvalidUserId(#[from] std::num::TryFromIntError),
    #[error("failed to save animation: {0}")]
    SaveAnimationError(#[from] SaveAnimationError),
}

async fn handle_submission(
    message: &Message,
    animation: &Animation,
) -> Result<(), HandleSubmissionError> {
    let api = API.wait();
    let config = CONFIG.wait();

    match &animation.mime_type {
        Some(mime_type) => {
            if !config.animation.allowed_mime_types.contains(mime_type) {
                api.send_message(
                    &SendMessageParams::builder()
                        .chat_id(message.chat.id)
                        .text(format!("I\u{2019}m not designed to handle GIFs of that file type ({mime_type})."))
                        .reply_to_message_id(message.message_id)
                        .build(),
                )
                .await?;
                return Ok(());
            }
        }
        None => {
            api.send_message(
                &SendMessageParams::builder()
                    .chat_id(message.chat.id)
                    .text("I couldn\u{2019}t determine the file type of that GIF.")
                    .reply_to_message_id(message.message_id)
                    .build(),
            )
            .await?;
            return Ok(());
        }
    }

    let mut db = DB.wait().lock().await;
    let t = db.transaction().await?;

    let tournament_id = match t
        .query_opt(
            r#"SELECT "id" FROM "tournaments" WHERE "chat_id" = $1 AND "state" = 'submitting'"#,
            &[&message.chat.id],
        )
        .await?
    {
        Some(row) => row.get::<_, String>("id"),
        None => return Ok(()),
    };

    let exists = match t
        .query_one(
            r#"SELECT count(*) AS "count" FROM "animations" WHERE "id" = $1"#,
            &[&animation.file_unique_id],
        )
        .await?
        .get::<_, i64>("count")
    {
        0 => false,
        1 => true,
        count => {
            return Err(HandleSubmissionError::DbIntegrityError(format!(
                "{count} animations with id {id}",
                id = animation.file_unique_id,
            )));
        }
    };

    if !exists {
        if let Err(err) = save_animation(&animation.file_unique_id, &animation.file_id).await {
            eprintln!("failed to save animation: {err}");
            return match err {
                SaveAnimationError::TooLarge(_) => {
                    api.send_message(
                        &SendMessageParams::builder()
                            .chat_id(message.chat.id)
                            .text(format!(
                                "The file size is too big {shocked}",
                                shocked = Kaomoji::SHOCKED,
                            ))
                            .reply_to_message_id(message.message_id)
                            .build(),
                    )
                    .await?;
                    Ok(())
                }
                _ => Err(err.into()),
            };
        }

        if let Err(err) = generate_thumbnail(&animation.file_unique_id) {
            eprintln!("failed to save animation: {err}");
            return Err(err.into());
        }

        let params = match get_animation_params(&animation.file_unique_id).await {
            Ok(params) => params,
            Err(err) => {
                eprintln!("failed to get animation params: {err}");
                return Err(err.into());
            }
        };
        let duration = params.duration();
        if duration > config.animation.max_duration_secs.into() {
            api.send_message(
                &SendMessageParams::builder()
                    .chat_id(message.chat.id)
                    .text(format!(
                        "GIFs longer than {max_duration} seconds are not accepted.",
                        max_duration = config.animation.max_duration_secs,
                    ))
                    .reply_to_message_id(message.message_id)
                    .build(),
            )
            .await?;
            return Ok(());
        }

        let count = t
            .execute(
                r#"
                INSERT INTO "animations" (
                    "id",
                    "file_identifier",
                    "width",
                    "height",
                    "mime_type",
                    "frames",
                    "fps_num",
                    "fps_denom"
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                "#,
                &[
                    &animation.file_unique_id,
                    &animation.file_id,
                    &params.width,
                    &params.height,
                    &animation.mime_type,
                    &params.frames,
                    &params.fps_num,
                    &params.fps_denom,
                ],
            )
            .await?;
        if count != 1 {
            return Err(HandleSubmissionError::DbIntegrityError(format!(
                "inserted {count} animations with id {id}, expected 1",
                id = animation.file_unique_id,
            )));
        }
    }

    if let Some(filename) = &animation.file_name {
        t.execute(
            r#"
            INSERT INTO "animation_filenames" ("animation_id", "filename") VALUES ($1, $2)
            ON CONFLICT DO NOTHING
            "#,
            &[&animation.file_unique_id, filename],
        )
        .await?;
    }

    let user_id = match &message.from {
        Some(user) => i64::try_from(user.id)?,
        None => {
            eprintln!("message has no sender; probably a channel post; ignoring");
            return Ok(());
        }
    };
    let count = t
        .execute(
            r#"
            INSERT INTO "users" ("id", "username") VALUES ($1, $2)
            ON CONFLICT ("id") DO UPDATE SET "username" = $2
            "#,
            &[
                &user_id,
                &message
                    .from
                    .as_ref()
                    .map(|user| user.username.as_ref())
                    .flatten(),
            ],
        )
        .await?;
    if count != 1 {
        return Err(HandleSubmissionError::DbIntegrityError(format!(
            "expected to upsert one user, upserted {count} rows"
        )));
    }

    let (is_primary, is_duplicate): (bool, bool) = {
        let counts = t
            .query_one(
                r#"
                SELECT "primary_subquery"."is_primary", "duplicate_subquery"."is_duplicate"
                FROM
                    (
                        SELECT count(*) > 0 AS "is_primary" FROM "duplicates"
                        WHERE "primary_animation_id" = $1
                    ) AS "primary_subquery"
                    CROSS JOIN
                    (
                        SELECT count(*) > 0 AS "is_duplicate" FROM "duplicates"
                        WHERE "duplicate_animation_id" = $1
                    ) AS "duplicate_subquery"
                "#,
                &[&animation.file_unique_id],
            )
            .await?;
        (counts.get("is_primary"), counts.get("is_duplicate"))
    };

    if is_primary && is_duplicate {
        return Err(HandleSubmissionError::DbIntegrityError(format!(
            "animation {id} is both primary and duplicate",
            id = animation.file_unique_id,
        )));
    }

    let similar: Vec<String> = if is_primary {
        t.query(
            r#"
            SELECT "duplicate_animation_id" FROM "duplicates"
            WHERE "primary_animation_id" = $1
            "#,
            &[&animation.file_unique_id],
        )
        .await?
        .into_iter()
        .map(|row| row.get("duplicate_animation_id"))
        .collect()
    } else if is_duplicate {
        t.query(
            r#"
            SELECT "duplicate_animation_id" AS "animation_id" FROM "duplicates"
            WHERE "primary_animation_id" = (
                SELECT "primary_animation_id" FROM "duplicates" WHERE "duplicate_animation_id" = $1
            ) AND "duplicate_animation_id" != $1
            UNION
            SELECT "primary_animation_id" AS "animation_id" FROM "duplicates" WHERE "duplicate_animation_id" = $1
            "#,
            &[&animation.file_unique_id],
        )
        .await?
        .into_iter()
        .map(|row| row.get("animation_id"))
        .collect()
    } else {
        Vec::new()
    };

    let already_submitted = t
        .query_opt(
            r#"
            SELECT NULL FROM "submissions"
            WHERE "tournament_id" = $1 AND "animation_id" = $2 AND "submitter_id" = $3
            "#,
            &[&tournament_id, &animation.file_unique_id, &user_id],
        )
        .await?
        .is_some();

    let already_submitted_similar = !similar.is_empty()
        && t.query_opt(
            r#"
            SELECT NULL FROM "submissions"
            WHERE "tournament_id" = $1 AND "animation_id" = ANY($2) AND "submitter_id" = $3
            "#,
            &[&tournament_id, &similar, &user_id],
        )
        .await?
        .is_some();

    if !already_submitted {
        let count = t
            .execute(
                r#"
                INSERT INTO "submissions" (
                    "tournament_id",
                    "animation_id",
                    "submitter_id",
                    "created_at"
                )
                VALUES ($1, $2, $3, $4)
                "#,
                &[
                    &tournament_id,
                    &animation.file_unique_id,
                    &user_id,
                    &Utc::now(),
                ],
            )
            .await?;
        if count != 1 {
            return Err(HandleSubmissionError::DbIntegrityError(format!(
                "expected to insert one submission, inserted {count} rows"
            )));
        }
    }

    let submission_count: i64 = t
        .query_one(
            r#"
            SELECT count(DISTINCT "submitter_id") AS "count" FROM "submissions"
            WHERE "tournament_id" = $1 AND ("animation_id" = $2 OR "animation_id" = ANY($3))
            "#,
            &[&tournament_id, &animation.file_unique_id, &similar],
        )
        .await?
        .get("count");

    t.commit().await?;

    let reply_text = if already_submitted {
        format!(
            "You have already sent this GIF. It has been sent {submissions}.",
            submissions = match submission_count {
                1 => "once".to_string(),
                2 => "twice".to_string(),
                _ => format!("{submission_count} times"),
            },
        )
    } else if already_submitted_similar {
        format!(
            "You have already sent a similar GIF. It has been sent {submissions}.",
            submissions = match submission_count {
                1 => "once".to_string(),
                2 => "twice".to_string(),
                _ => format!("{submission_count} times"),
            },
        )
    } else {
        match submission_count {
            1 => format!(
                "Thanks for the GIF, you are the first to send it! {happy}",
                happy = Kaomoji::HAPPY,
            ),
            2 => "Your vote has been counted. This GIF has now been sent twice.".to_string(),
            _ => format!(
                "Your vote has been counted. This GIF has now been sent {submission_count} times.",
            ),
        }
    };

    api.send_message(
        &SendMessageParams::builder()
            .chat_id(message.chat.id)
            .text(reply_text)
            .reply_to_message_id(message.message_id)
            .build(),
    )
    .await?;
    Ok(())
}
