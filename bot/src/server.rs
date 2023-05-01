use std::{collections::HashSet, os::unix::fs::PermissionsExt};

use poem::{
    error::{IntoResult, NotFound, ResponseError},
    get, handler,
    http::StatusCode,
    listener::{UnixListener, Listener},
    web::Query,
    Body, IntoResponse, Route, Server,
};
use serde::Deserialize;

use crate::{CONFIG, DB, POSSIBLE_DUPLICATES};

#[derive(Debug, thiserror::Error)]
pub enum ServerListenerError {
    #[error("thread join error: {0}")]
    JoinError(#[from] tokio::task::JoinError),
    #[error("server error: {0}")]
    ServerError(#[source] std::io::Error),
    #[error("failed to bind socket: {0}")]
    SocketBindError(#[source] std::io::Error),
    #[error("failed to set socket permissions: {0}")]
    SocketSetPermissionsError(#[source] std::io::Error),
}

pub async fn listen() -> Result<(), ServerListenerError> {
    let config = CONFIG.wait();

    _ = std::fs::remove_file(&config.server.socket_path);

    let listener = UnixListener::bind(&config.server.socket_path);
    let acceptor = listener.into_acceptor().await.map_err(ServerListenerError::SocketBindError)?;

    std::fs::set_permissions(
        &config.server.socket_path,
        std::fs::Permissions::from_mode(config.server.socket_permissions),
    )
    .map_err(ServerListenerError::SocketSetPermissionsError)?;

    let app = Route::new().at("/duplicates/suggestions", get(serve_duplicates_suggestions));
    Server::new_with_acceptor(acceptor).run(app).await.map_err(ServerListenerError::ServerError)
}

async fn get_tournament_id(input: &str) -> Option<String> {
    let db = DB.wait().lock().await;

    if input.starts_with('@') {
        let chat_username = &input[1..];
        match db
            .query_opt(
                r#"
                SELECT "tournaments"."id" AS "tournament_id"
                FROM "tournaments" JOIN "chats" ON "chats"."id" = "tournaments"."chat_id"
                WHERE "chats"."username" = $1 AND "tournaments"."state" != 'aborted'
                ORDER BY "tournaments"."created_at" DESC
                LIMIT 1
                "#,
                &[&chat_username],
            )
            .await
        {
            Ok(Some(row)) => row.get("tournament_id"),
            Ok(None) => return None,
            Err(err) => {
                eprintln!("failed to query tournament ID: {err}");
                return None;
            }
        }
    } else {
        match db
            .query_opt(
                r#"SELECT NULL FROM "tournaments" WHERE "id" = $1"#,
                &[&input],
            )
            .await
        {
            Ok(Some(_)) => Some(input.into()),
            Ok(None) => None,
            Err(err) => {
                eprintln!("failed to query tournament ID: {err}");
                return None;
            }
        }
    }
}

#[derive(Deserialize)]
struct TournamentQuery {
    tournament: String,
}

#[derive(Debug, thiserror::Error)]
enum ServeDuplicatesSuggestionsError {
    #[error("db error: {0}")]
    DbError(#[from] deadpool_postgres::tokio_postgres::Error),
    #[error("serialization error: {0}")]
    SerializeError(#[from] serde_json::Error),
    #[error("tournament not found")]
    TournamentNotFound,
}

impl ResponseError for ServeDuplicatesSuggestionsError {
    fn status(&self) -> StatusCode {
        match self {
            Self::TournamentNotFound => StatusCode::NOT_FOUND,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[handler]
async fn serve_duplicates_suggestions(
    Query(TournamentQuery { tournament }): Query<TournamentQuery>,
) -> poem::Result<impl IntoResponse> {
    let tournament_id = match get_tournament_id(&tournament).await {
        Some(tournament_id) => tournament_id,
        None => {
            return Err(NotFound(
                ServeDuplicatesSuggestionsError::TournamentNotFound,
            ))
        }
    };
    let db = DB.wait().lock().await;

    let submissions: HashSet<String> = db
        .query(
            r#"
            SELECT "submissions"."animation_id"
            FROM "submissions"
                LEFT JOIN "duplicates"
                ON "submissions"."animation_id" = "duplicates"."duplicate_animation_id"
            WHERE "submissions"."tournament_id" = $1 AND
                "duplicates"."duplicate_animation_id" IS NULL
            "#,
            &[&tournament_id],
        )
        .await
        .map_err(ServeDuplicatesSuggestionsError::from)?
        .into_iter()
        .map(|row| row.get("animation_id"))
        .collect();

    let mut rv = Vec::new();

    let possible_duplicates = POSSIBLE_DUPLICATES.lock().await;
    for set in possible_duplicates.iter() {
        let filtered = set
            .into_iter()
            .filter(|animation_id| submissions.contains(*animation_id))
            .collect::<Vec<_>>();
        if filtered.len() >= 2 {
            rv.push(filtered);
        }
    }
    Body::from_json(rv)
        .map_err(ServeDuplicatesSuggestionsError::from)?
        .into_result()
}
