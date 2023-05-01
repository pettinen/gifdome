use deadpool_postgres::{
    tokio_postgres::{error::SqlState, NoTls},
    Config as DbConfig,
};
use frankenstein::ChatType;
use postgres_types::{FromSql, ToSql};
use serde::Serialize;

use crate::config::Config;
use macros::sql_enum;

#[sql_enum]
#[name("chat_type")]
pub enum ChatGroupType {
    Group,
    Supergroup,
}

#[derive(Debug, thiserror::Error)]
#[error("{0:?} is not a group type")]
pub struct TryFromChatTypeError(ChatType);

impl TryFrom<ChatType> for ChatGroupType {
    type Error = TryFromChatTypeError;

    fn try_from(value: ChatType) -> Result<Self, Self::Error> {
        match value {
            ChatType::Group => Ok(Self::Group),
            ChatType::Supergroup => Ok(Self::Supergroup),
            other_type => Err(TryFromChatTypeError(other_type)),
        }
    }
}

#[derive(PartialEq)]
#[sql_enum]
pub enum MatchupState {
    NotStarted,
    Started,
    Finished,
    Aborted,
}

#[derive(PartialEq)]
#[sql_enum]
pub enum TournamentState {
    Submitting,
    Voting,
    Finished,
    Aborted,
}

#[derive(Debug, thiserror::Error)]
pub enum InitDbError {
    #[error("failed to create database connection pool: {0}")]
    DbCreatePoolError(#[from] deadpool_postgres::CreatePoolError),
    #[error(transparent)]
    DbError(#[from] deadpool_postgres::tokio_postgres::Error),
    #[error("database error: {0}")]
    DbPoolError(#[from] deadpool_postgres::PoolError),
    #[error("missing dbname in init db config")]
    MissingDbName,
    #[error("missing init db config")]
    MissingInitConfig,
    #[error("missing password in init db config")]
    MissingPassword,
    #[error("missing user in init db config")]
    MissingUser,
    #[error("PostgreSQL identifiers must not contain null characters")]
    NullCharacterInIdentifier,
}

fn enum_variants(variants: Vec<String>) -> String {
    variants
        .into_iter()
        .map(|name| format!("'{}'", name))
        .collect::<Vec<_>>()
        .join(", ")
}

fn sanitize_db_identifier(value: &str) -> Result<String, InitDbError> {
    if value.contains('\0') {
        return Err(InitDbError::NullCharacterInIdentifier);
    }
    Ok(value.replace('"', "\"\""))
}

pub async fn init_db(
    config: &Config,
    create_user: bool,
    create_database: bool,
    drop_existing: bool,
) -> Result<(), InitDbError> {
    let init_config = config
        .dev
        .init_db
        .as_ref()
        .ok_or(InitDbError::MissingInitConfig)?;
    let pool = init_config.create_pool(None, NoTls)?;
    let db = pool.get().await?;
    let dbname = match config.db.dbname.as_ref() {
        Some(dbname) => Some(sanitize_db_identifier(&dbname)?),
        None => None,
    };
    let user = match config.db.user.as_ref() {
        Some(user) => Some(sanitize_db_identifier(&user)?),
        None => None,
    };

    if create_user {
        let user = user.as_ref().ok_or(InitDbError::MissingUser)?;
        let password = config
            .db
            .password
            .as_ref()
            .ok_or(InitDbError::MissingPassword)?
            .replace('\'', "''");
        if let Err(err) = db
            .execute(
                &format!(r#"CREATE USER "{}" PASSWORD '{}'"#, user, password),
                &[],
            )
            .await
        {
            if err.code() != Some(&SqlState::DUPLICATE_OBJECT) {
                return Err(err.into());
            }
        }
    }

    if create_database {
        let dbname = dbname.ok_or(InitDbError::MissingDbName)?;
        let user = user.as_ref().ok_or(InitDbError::MissingUser)?;
        if let Err(err) = db
            .execute(
                &format!(r#"CREATE DATABASE "{}" WITH OWNER "{}""#, dbname, user),
                &[],
            )
            .await
        {
            if err.code() != Some(&SqlState::DUPLICATE_DATABASE) {
                return Err(err.into());
            }
        }
    }

    let init_config = DbConfig {
        dbname: config.db.dbname.clone(),
        ..init_config.clone()
    };
    let pool = init_config.create_pool(None, NoTls)?;
    let db = pool.get().await?;

    if drop_existing {
        db.batch_execute(
            r#"
            DROP TABLE IF EXISTS
                "chats",
                "duplicates",
                "animations",
                "animation_filenames",
                "matchups",
                "submissions",
                "suggested_duplicates",
                "tournaments",
                "users"
            CASCADE;
            DROP TYPE IF EXISTS "chat_type", "matchup_state", "tournament_state";
            "#,
        )
        .await?;
    }

    db.batch_execute(&format!(
        r#"
        DO $$ BEGIN
            CREATE TYPE "chat_type" AS ENUM({chat_type_variants});
        EXCEPTION
            WHEN duplicate_object THEN null;
        END $$;
        DO $$ BEGIN
            CREATE TYPE "matchup_state" AS ENUM({matchup_state_variants});
        EXCEPTION
            WHEN duplicate_object THEN null;
        END $$;
        DO $$ BEGIN
            CREATE TYPE "tournament_state" AS ENUM({tournament_state_variants});
        EXCEPTION
            WHEN duplicate_object THEN null;
        END $$;

        CREATE TABLE IF NOT EXISTS "chats" (
            "id" bigint PRIMARY KEY,
            "type" chat_type NOT NULL,
            "title" text NOT NULL,
            "username" text
        );

        CREATE TABLE IF NOT EXISTS "animations" (
            "id" text PRIMARY KEY,
            "file_identifier" text NOT NULL,
            "width" integer NOT NULL,
            "height" integer NOT NULL,
            "mime_type" text NOT NULL,
            "frames" integer NOT NULL,
            "fps_num" integer NOT NULL,
            "fps_denom" integer NOT NULL,
            "description" text CHECK ("description" != '')
        );

        CREATE TABLE IF NOT EXISTS "animation_filenames" (
            "animation_id" text REFERENCES "animations"("id"),
            "filename" text,
            PRIMARY KEY ("animation_id", "filename")
        );

        CREATE TABLE IF NOT EXISTS "suggested_duplicates" (
            "primary_animation_id" text REFERENCES "animations"("id"),
            "duplicate_animation_id" text REFERENCES "animations"("id"),
            PRIMARY KEY ("primary_animation_id", "duplicate_animation_id"),
            CHECK ("primary_animation_id" != "duplicate_animation_id")
        );

        CREATE TABLE IF NOT EXISTS "duplicates" (
            "duplicate_animation_id" text PRIMARY KEY REFERENCES "animations"("id"),
            "primary_animation_id" text REFERENCES "animations"("id"),
            CHECK ("primary_animation_id" != "duplicate_animation_id")
        );

        CREATE TABLE IF NOT EXISTS "tournaments" (
            "id" text PRIMARY KEY CHECK (length("id") = {tournament_id_length}),
            "chat_id" bigint NOT NULL REFERENCES "chats"("id"),
            "state" tournament_state NOT NULL,
            "rounds" smallint CHECK ("rounds" >= 1 AND "rounds" <= {max_rounds}),
            "min_votes" smallint CHECK ("min_votes" >= 1),
            "created_at" timestamp (6) with time zone NOT NULL,
            CHECK (
                (
                    "state" = 'submitting' AND
                    "rounds" IS NULL AND
                    "min_votes" IS NULL
                ) OR
                (
                    "state" IN ('voting', 'finished') AND
                    "rounds" IS NOT NULL AND
                    "min_votes" IS NOT NULL
                ) OR "state" = 'aborted'
            )
        );
        CREATE UNIQUE INDEX IF NOT EXISTS "tournaments_chat_id_idx" ON "tournaments"("chat_id")
            WHERE "state" IN ('submitting', 'voting');

        CREATE TABLE IF NOT EXISTS "matchups" (
            "tournament_id" text REFERENCES "tournaments"("id"),
            "index" integer CHECK ("index" >= 0),
            "round" smallint NOT NULL CHECK ("round" >= 1 AND "round" <= {max_rounds}),
            "poll_id" text,
            "message_id" integer,
            "animation_a_id" text REFERENCES "animations"("id"),
            "animation_b_id" text REFERENCES "animations"("id"),
            "state" matchup_state NOT NULL,
            "animation_a_votes" integer CHECK ("animation_a_votes" >= 0),
            "animation_b_votes" integer CHECK ("animation_b_votes" >= 0),
            "duration_secs" integer NOT NULL,
            "started_at" timestamp (6) with time zone,
            "finished_at" timestamp (6) with time zone,
            PRIMARY KEY ("tournament_id", "index"),
            CHECK ("animation_a_id" != "animation_b_id"),
            CHECK (
                (
                    "state" = 'not_started' AND
                    "poll_id" IS NULL AND
                    "message_id" IS NULL AND
                    "animation_a_votes" IS NULL AND
                    "animation_b_votes" IS NULL AND
                    "started_at" IS NULL AND
                    "finished_at" IS NULL
                ) OR
                (
                    "state" IN ('started', 'aborted') AND
                    "poll_id" IS NOT NULL AND
                    "message_id" IS NOT NULL AND
                    "animation_a_id" IS NOT NULL AND
                    "animation_b_id" IS NOT NULL AND
                    "animation_a_votes" IS NOT NULL AND
                    "animation_b_votes" IS NOT NULL AND
                    "started_at" IS NOT NULL AND
                    "finished_at" IS NULL
                ) OR
                (
                    "state" = 'finished' AND
                    "poll_id" IS NOT NULL AND
                    "message_id" IS NOT NULL AND
                    "animation_a_id" IS NOT NULL AND
                    "animation_b_id" IS NOT NULL AND
                    "animation_a_votes" IS NOT NULL AND
                    "animation_b_votes" IS NOT NULL AND
                    "started_at" IS NOT NULL AND
                    "finished_at" IS NOT NULL
                )
            )
        );
        CREATE UNIQUE INDEX IF NOT EXISTS "matchups_tournament_id_index_idx"
            ON "matchups"("tournament_id", "index")
            WHERE "state" = 'started';

        CREATE TABLE IF NOT EXISTS "users" (
            "id" bigint PRIMARY KEY,
            "username" text NOT NULL
        );

        CREATE TABLE IF NOT EXISTS "submissions" (
            "tournament_id" text REFERENCES "tournaments"("id"),
            "animation_id" text REFERENCES "animations"("id"),
            "submitter_id" bigint NOT NULL REFERENCES "users"("id"),
            "created_at" timestamp (6) with time zone NOT NULL,
            PRIMARY KEY ("tournament_id", "animation_id", "submitter_id")
        );
        "#,
        chat_type_variants = enum_variants(ChatGroupType::variants()),
        matchup_state_variants = enum_variants(MatchupState::variants()),
        tournament_state_variants = enum_variants(TournamentState::variants()),
        tournament_id_length = config.tournament.id_length,
        max_rounds = config.tournament.max_rounds,
    ))
    .await?;

    if create_user {
        let user = user.as_ref().ok_or(InitDbError::MissingUser)?;
        db.execute(
            &format!(
                r#"GRANT ALL ON ALL TABLES IN SCHEMA "public" TO "{}""#,
                user
            ),
            &[],
        )
        .await?;
    }
    Ok(())
}
