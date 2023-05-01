use std::str::FromStr;

use chrono::Utc;
use frankenstein::{
    AsyncTelegramApi, ChatMember, ChatType, GetChatMemberParams, Message, MessageEntityType,
    PinChatMessageParams, SendMessageParams,
};
use regex::Regex;
use strum_macros::EnumString;

use crate::{
    db::{ChatGroupType, TournamentState},
    tournament::{create_bracket, send_poll, CreateBracketError, SendPollError},
    util::{generate_token, unexpected_error_reply, update_chat_commands, Kaomoji},
    API, BOT_USERNAME, CONFIG, DB,
};

#[derive(Debug, EnumString)]
#[strum(serialize_all = "lowercase")]
pub enum Command {
    Abort,
    Help,
    Start,
    StartVoting,
}

#[derive(Debug, thiserror::Error)]
pub enum ParseCommandError {
    #[error("no text in message")]
    MissingText,
    #[error("multiple commands in message")]
    MultipleCommands,
}

pub fn parse_command(message: &Message) -> Result<Option<Command>, ParseCommandError> {
    let mut command_entities = Vec::new();
    let text = match (message.entities.as_ref(), message.caption_entities.as_ref()) {
        (Some(entities), None) => {
            for entity in entities {
                if entity.type_field == MessageEntityType::BotCommand {
                    command_entities.push(entity);
                }
            }
            &message
                .text
                .as_ref()
                .ok_or(ParseCommandError::MissingText)?
        }
        (None, Some(entities)) => {
            for entity in entities {
                if entity.type_field == MessageEntityType::BotCommand {
                    command_entities.push(entity);
                }
            }
            message
                .caption
                .as_ref()
                .ok_or(ParseCommandError::MissingText)?
        }
        (Some(_), Some(_)) => return Err(ParseCommandError::MultipleCommands),
        (None, None) => return Ok(None),
    };

    let command_string = match command_entities[..] {
        [] => return Ok(None),
        [entity] => {
            let offset = entity.offset as usize;
            let length = entity.length as usize;
            match text.get(offset..offset + length) {
                Some(command) => command,
                None => return Ok(None),
            }
        }
        _ => return Err(ParseCommandError::MultipleCommands),
    };

    let (regex, bot_username_lc) = match BOT_USERNAME.wait() {
        Some(bot_username) => (
            Regex::new(r"^/(?P<cmd>[0-9A-Za-z_]+)(@(?P<username>[0-9A-Za-z_]+))?$").unwrap(),
            Some(bot_username.to_lowercase()),
        ),
        None => (Regex::new(r"^/(?P<cmd>[0-9A-Za-z_]+)$").unwrap(), None),
    };
    let captures = match regex.captures(command_string) {
        Some(captures) => captures,
        None => return Ok(None),
    };
    if let Some(username) = captures.name("username") {
        let username_lc = username.as_str().to_lowercase();
        if let Some(bot_username_lc) = bot_username_lc {
            if username_lc != bot_username_lc {
                return Ok(None);
            }
        } else {
            return Ok(None);
        }
    };
    let command_name = match captures.name("cmd") {
        Some(command_name) => command_name.as_str(),
        None => return Ok(None),
    };
    Ok(Command::from_str(command_name).ok())
}

pub async fn handle_command(command: &Command, message: &Message) {
    match command {
        Command::Abort => {
            if let Err(err) = handle_abort(message).await {
                eprintln!("error handling /abort command: {err}");
                unexpected_error_reply(message).await;
            }
        }
        Command::Help => {
            if let Err(err) = handle_help(message).await {
                eprintln!("error handling /help command: {err}");
                unexpected_error_reply(message).await;
            }
        }
        Command::Start => {
            if let Err(err) = handle_start(message).await {
                eprintln!("error handling /start command: {err}");
                unexpected_error_reply(message).await;
            }
        }
        Command::StartVoting => {
            if let Err(err) = handle_startvoting(message).await {
                eprintln!("error handling /startvoting command: {err}");
                unexpected_error_reply(message).await;
            }
        }
    }
}

fn is_in_group(message: &Message) -> bool {
    match message.chat.type_field {
        ChatType::Group | ChatType::Supergroup => true,
        _ => false,
    }
}

#[derive(Debug, thiserror::Error)]
enum IsFromGroupAdminError {
    #[error("failed to fetch chat member: {0}")]
    ChatMemberFetchFailed(#[from] frankenstein::Error),
    #[error("message has no sender")]
    NoUser,
}

async fn is_from_group_admin(message: &Message) -> Result<bool, IsFromGroupAdminError> {
    let user_id = match &message.from {
        Some(user) => user.id,
        None => return Err(IsFromGroupAdminError::NoUser),
    };
    let api = API.wait();
    let chat_member = api
        .get_chat_member(
            &GetChatMemberParams::builder()
                .chat_id(message.chat.id)
                .user_id(user_id)
                .build(),
        )
        .await?
        .result;
    Ok(match chat_member {
        ChatMember::Creator(_) | ChatMember::Administrator(_) => true,
        _ => false,
    })
}

async fn reply_not_from_group_admin(message: &Message) -> Result<(), frankenstein::Error> {
    let api = API.wait();
    api.send_message(
        &SendMessageParams::builder()
            .chat_id(message.chat.id)
            .text(format!(
                "Only group admins can use that command {wink}",
                wink = Kaomoji::WINK,
            ))
            .reply_to_message_id(message.message_id)
            .build(),
    )
    .await?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum AbortError {
    #[error("failed to commit transaction: {0}")]
    CommitTransactionFailed(#[source] deadpool_postgres::tokio_postgres::Error),
    #[error("{0}")]
    DbIntegrityError(String),
    #[error(transparent)]
    IsFromGroupAdminError(#[from] IsFromGroupAdminError),
    #[error("failed to send message: {0}")]
    SendMessageFailed(#[from] frankenstein::Error),
    #[error("failed to start transaction: {0}")]
    StartTransactionFailed(#[source] deadpool_postgres::tokio_postgres::Error),
    #[error("failed to query tournament: {0}")]
    QueryTournamentsFailed(#[source] deadpool_postgres::tokio_postgres::Error),
    #[error("failed to update matchups: {0}")]
    UpdateMatchupsFailed(#[source] deadpool_postgres::tokio_postgres::Error),
    #[error("failed to update tournaments: {0}")]
    UpdateTournamentsFailed(#[source] deadpool_postgres::tokio_postgres::Error),
}

async fn handle_abort(message: &Message) -> Result<(), AbortError> {
    if !is_in_group(message) {
        return Ok(());
    }
    if !is_from_group_admin(message).await? {
        reply_not_from_group_admin(message)
            .await
            .map_err(AbortError::SendMessageFailed)?;
        return Ok(());
    }

    let api = API.wait();
    let chat_id = message.chat.id;

    let mut db = DB.wait().lock().await;
    let t = db
        .transaction()
        .await
        .map_err(AbortError::StartTransactionFailed)?;
    let row = t
        .query_opt(
            r#"
            SELECT "id" FROM "tournaments"
            WHERE "chat_id" = $1 AND "state" IN ('submitting', 'voting')
            "#,
            &[&chat_id],
        )
        .await
        .map_err(AbortError::QueryTournamentsFailed)?;

    let tournament_id = match row {
        Some(row) => row.get::<_, String>("id"),
        None => {
            api.send_message(
                &SendMessageParams::builder()
                    .chat_id(message.chat.id)
                    .text(format!(
                        "There is no tournament running {confused}",
                        confused = Kaomoji::CONFUSED,
                    ))
                    .reply_to_message_id(message.message_id)
                    .build(),
            )
            .await?;
            return Ok(());
        }
    };

    let count = t
        .execute(
            r#"UPDATE "tournaments" SET "state" = $1 WHERE "id" = $2"#,
            &[&TournamentState::Aborted, &tournament_id],
        )
        .await
        .map_err(AbortError::UpdateTournamentsFailed)?;
    if count != 1 {
        return Err(AbortError::DbIntegrityError(format!(
            "expected to update 1 tournament, updated {count} rows",
        )));
    }
    let count = t
        .execute(
            r#"UPDATE "matchups" SET "state" = 'aborted' WHERE "tournament_id" = $1 AND "state" = 'started'"#,
            &[
                &tournament_id,
            ],
        )
        .await
        .map_err(AbortError::UpdateMatchupsFailed)?;
    if count > 1 {
        return Err(AbortError::DbIntegrityError(format!(
            "expected to update 0 or 1 matchups, updated {count} rows",
        )));
    }
    t.commit()
        .await
        .map_err(AbortError::CommitTransactionFailed)?;
    api.send_message(
        &SendMessageParams::builder()
            .chat_id(message.chat.id)
            .text(format!(
                "I have stopped the tournament {sad}",
                sad = Kaomoji::SAD,
            ))
            .reply_to_message_id(message.message_id)
            .build(),
    )
    .await?;

    if let Err(err) = update_chat_commands(message.chat.id, None).await {
        eprintln!("failed to update chat commands: {err}");
    }

    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum HelpError {
    #[error(transparent)]
    IsFromGroupAdminError(#[from] IsFromGroupAdminError),
    #[error("failed to send message: {0}")]
    SendMessageFailed(#[from] frankenstein::Error),
    #[error("failed to query database: {0}")]
    QueryTournamentsFailed(#[from] deadpool_postgres::tokio_postgres::Error),
}

async fn handle_help(message: &Message) -> Result<(), HelpError> {
    let api = API.wait();

    let mut help_text_lines = vec![
        "The GIFdome aims to find the ultimate GIF by process of elimination.".to_string(),
        "".to_string(),
    ];
    if !is_in_group(message) {
        help_text_lines.push(format!(
            "Invite me to a group to start a tournament {wink}",
            wink = Kaomoji::WINK,
        ));
    } else {
        let db = DB.wait().lock().await;
        let row = db
            .query_opt(
                r#"SELECT "state" FROM "tournaments" WHERE "chat_id" = $1 AND "state" IN ($2, $3)"#,
                &[
                    &message.chat.id,
                    &TournamentState::Submitting,
                    &TournamentState::Voting,
                ],
            )
            .await?;

        let is_from_group_admin = is_from_group_admin(message).await?;

        match row.map(|row| row.get::<_, TournamentState>("state")) {
            Some(TournamentState::Submitting) => {
                help_text_lines.push(
                    "The tournament is currently in submission phase. \
                    To submit a GIF, just send one to the group."
                        .to_string(),
                );
                help_text_lines.push(
                    "You can cast your vote on an already submitted GIF by sending it again; \
                    forwarding a GIF sent by someone else also works."
                        .to_string(),
                );

                if is_from_group_admin {
                    let config = CONFIG.wait();
                    help_text_lines.push("".to_string());
                    help_text_lines.push("Available commands:".to_string());
                    help_text_lines.push(
                        "• /startvoting - close submissions and start the voting phase. \
                        After the command, specify:"
                            .to_string(),
                    );
                    help_text_lines.push(format!(
                        "  • minimumvotes=<number between 1 and {u8_max}>",
                        u8_max = u8::MAX,
                    ));
                    help_text_lines.push(format!(
                        "  • rounds=<number between 1 and {max_rounds}>",
                        max_rounds = config.tournament.max_rounds,
                    ));
                    help_text_lines.push("• /abort - abort the current tournament".to_string());
                }
            }
            Some(TournamentState::Voting) => {
                help_text_lines.push(
                    "The tournament is currently in voting phase. \
                    See the pinned message for the current poll."
                        .to_string(),
                );

                if is_from_group_admin {
                    help_text_lines.push("".to_string());
                    help_text_lines.push("Available commands:".to_string());
                    help_text_lines.push("• /abort - abort the current tournament".to_string());
                }
            }
            Some(_) | None => {
                help_text_lines.push("There is currently no tournament running.".to_string());

                if is_from_group_admin {
                    help_text_lines.push("".to_string());
                    help_text_lines.push("Available commands:".to_string());
                    help_text_lines.push("• /start - start the tournament".to_string());
                }
            }
        }
    }

    api.send_message(
        &SendMessageParams::builder()
            .chat_id(message.chat.id)
            .text(help_text_lines.join("\n"))
            .reply_to_message_id(message.message_id)
            .build(),
    )
    .await?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum StartError {
    #[error("failed to commit transaction: {0}")]
    CommitTransactionFailed(#[source] deadpool_postgres::tokio_postgres::Error),
    #[error("{0}")]
    DbIntegrityError(String),
    #[error("failed to insert chat: {0}")]
    InsertChatFailed(#[source] deadpool_postgres::tokio_postgres::Error),
    #[error("failed to insert tournament: {0}")]
    InsertTournamentFailed(#[source] deadpool_postgres::tokio_postgres::Error),
    #[error(transparent)]
    IsFromGroupAdminError(#[from] IsFromGroupAdminError),
    #[error("failed to send message: {0}")]
    SendMessageFailed(#[source] frankenstein::Error),
    #[error("failed to set commands for chat admins: {0}")]
    SetCommandsFailed(#[source] frankenstein::Error),
    #[error("failed to start transaction: {0}")]
    StartTransactionFailed(#[source] deadpool_postgres::tokio_postgres::Error),
    #[error("failed to query tournaments: {0}")]
    QueryTournamentsFailed(#[source] deadpool_postgres::tokio_postgres::Error),
}

async fn handle_start(message: &Message) -> Result<(), StartError> {
    let api = API.wait();
    let chat_type: ChatGroupType = match message.chat.type_field.try_into() {
        Ok(chat_type) => chat_type,
        Err(_) => {
            api.send_message(
                &SendMessageParams::builder()
                    .chat_id(message.chat.id)
                    .text(format!(
                        "Invite me to a group to start a tournament {wink}",
                        wink = Kaomoji::WINK,
                    ))
                    .reply_to_message_id(message.message_id)
                    .build(),
            )
            .await
            .map_err(StartError::SendMessageFailed)?;
            return Ok(());
        }
    };
    if !is_from_group_admin(message).await? {
        reply_not_from_group_admin(message)
            .await
            .map_err(StartError::SendMessageFailed)?;
        return Ok(());
    }

    let mut db = DB.wait().lock().await;
    let t = db
        .transaction()
        .await
        .map_err(StartError::StartTransactionFailed)?;

    let row = t
        .query_opt(
            r#"SELECT "id" FROM "tournaments" WHERE "chat_id" = $1 AND "state" IN ($2, $3)"#,
            &[
                &message.chat.id,
                &TournamentState::Submitting,
                &TournamentState::Voting,
            ],
        )
        .await
        .map_err(StartError::QueryTournamentsFailed)?;

    if row.is_some() {
        _ = api
            .send_message(
                &SendMessageParams::builder()
                    .chat_id(message.chat.id)
                    .text(format!(
                        "There is already a tournament running {confused}",
                        confused = Kaomoji::CONFUSED,
                    ))
                    .reply_to_message_id(message.message_id)
                    .build(),
            )
            .await;
        return Ok(());
    }

    let count = t
        .execute(
            r#"
            INSERT INTO "chats" ("id", "type", "title", "username")
            VALUES ($1, $2, $3, $4)
            ON CONFLICT ("id") DO UPDATE SET "type" = $2, "title" = $3, "username" = $4
            "#,
            &[
                &message.chat.id,
                &chat_type,
                &message.chat.title,
                &message.chat.username,
            ],
        )
        .await
        .map_err(StartError::InsertChatFailed)?;
    if count != 1 {
        return Err(StartError::DbIntegrityError(format!(
            "expected to upsert one chat, upserted {count} rows",
        )));
    }

    let config = CONFIG.wait();
    let tournament_id = generate_token(config.tournament.id_length);
    let count = t
        .execute(
            r#"
            INSERT INTO "tournaments" ("id", "chat_id", "state", "created_at")
            VALUES ($1, $2, $3, $4)
            "#,
            &[
                &tournament_id,
                &message.chat.id,
                &TournamentState::Submitting,
                &Utc::now(),
            ],
        )
        .await
        .map_err(StartError::InsertTournamentFailed)?;
    match count {
        0 => {
            return Err(StartError::DbIntegrityError(format!(
                "expected to insert 1 tournament, inserted {count} rows",
            )))
        }
        1 => {}
        _ => {
            return Err(StartError::DbIntegrityError(format!(
                "expected to insert 1 tournament, inserted {count} rows",
            )))
        }
    }
    t.commit()
        .await
        .map_err(StartError::CommitTransactionFailed)?;

    update_chat_commands(message.chat.id, Some(TournamentState::Submitting))
        .await
        .map_err(StartError::SetCommandsFailed)?;

    let new_message = api
        .send_message(
            &SendMessageParams::builder()
                .chat_id(message.chat.id)
                .text(
                    vec![
                        &format!(
                            "The GIFdome has started! Send me your best GIFs! {excited}",
                            excited = Kaomoji::EXCITED,
                        ),
                        "",
                        "To submit a GIF, just send one to the group. \
                         You can cast your vote on an already submitted GIF by sending it again; \
                         forwarding a GIF sent by someone else also works.",
                    ]
                    .join("\n"),
                )
                .reply_to_message_id(message.message_id)
                .build(),
        )
        .await
        .map_err(StartError::SendMessageFailed)?
        .result;

    if let Err(err) = api
        .pin_chat_message(
            &PinChatMessageParams::builder()
                .chat_id(message.chat.id)
                .message_id(new_message.message_id)
                .disable_notification(true)
                .build(),
        )
        .await
    {
        eprintln!("failed to pin message: {err}");
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum StartVotingError {
    #[error("failed to commit transaction: {0}")]
    CommitTransactionFailed(#[source] deadpool_postgres::tokio_postgres::Error),
    #[error("failed to create bracket: {0}")]
    CreateBracketError(#[from] CreateBracketError),
    #[error("db integrity error: {0}")]
    DbIntegrityError(String),
    #[error(transparent)]
    IsFromGroupAdminError(#[from] IsFromGroupAdminError),
    #[error("message has no text")]
    NoTextInMessage,
    #[error("failed to query tournament: {0}")]
    QueryTournamentsFailed(#[source] deadpool_postgres::tokio_postgres::Error),
    #[error("failed to send message: {0}")]
    SendMessageFailed(#[source] frankenstein::Error),
    #[error("failed to send poll: {0}")]
    SendPollError(#[from] SendPollError),
    #[error("failed to start transaction: {0}")]
    StartTransactionFailed(#[source] deadpool_postgres::tokio_postgres::Error),
    #[error("failed to update first matchup of the tournament: {0}")]
    UpdateFirstMatchupFailed(#[source] deadpool_postgres::tokio_postgres::Error),
    #[error("failed to update tournament: {0}")]
    UpdateTournamentFailed(#[source] deadpool_postgres::tokio_postgres::Error),
    #[error("unexpected error: {0}")]
    UnexpectedRegexError(#[from] regex::Error),
}

async fn handle_startvoting(message: &Message) -> Result<(), StartVotingError> {
    if !is_in_group(message) {
        return Ok(());
    }
    if !is_from_group_admin(message).await? {
        reply_not_from_group_admin(message)
            .await
            .map_err(StartVotingError::SendMessageFailed)?;
        return Ok(());
    }

    struct ParameterValues {
        as_i16: i16,
        as_u32: u32,
    }

    fn parse_params_from_message(message_text: &str) -> Option<(ParameterValues, ParameterValues)> {
        let re1 = Regex::new(
            r"^\s*/startvoting(@\w+)?\s+minimumvotes=(?P<minvotes>[0-9]+)\s+rounds=(?P<rounds>[0-9]+)\s*$",
        )
        .unwrap();
        let re2 = Regex::new(
            r"^\s*/startvoting(@\w+)?\s+rounds=(?P<rounds>[0-9]+)\s+minimumvotes=(?P<minvotes>[0-9]+)\s*$",
        )
        .unwrap();

        let captures = match re1.captures(message_text).or(re2.captures(message_text)) {
            Some(captures) => captures,
            None => return None,
        };
        let min_votes = match captures.name("minvotes") {
            Some(min_votes) => min_votes.as_str(),
            None => return None,
        };
        let rounds = match captures.name("rounds") {
            Some(rounds) => rounds.as_str(),
            None => return None,
        };

        let config = CONFIG.wait();
        Some((
            ParameterValues {
                as_i16: match min_votes.parse::<i16>() {
                    Ok(value) => {
                        if value < 1 || value > u8::MAX.into() {
                            return None;
                        }
                        value
                    }
                    Err(_) => return None,
                },
                as_u32: match min_votes.parse() {
                    Ok(value) => value,
                    Err(_) => return None,
                },
            },
            ParameterValues {
                as_i16: match rounds.parse::<i16>() {
                    Ok(value) => {
                        if value < 1 || value > config.tournament.max_rounds.into() {
                            return None;
                        }
                        value
                    }
                    Err(_) => return None,
                },
                as_u32: match rounds.parse() {
                    Ok(value) => value,
                    Err(_) => return None,
                },
            },
        ))
    }

    let message_text = match message.text.as_ref().or(message.caption.as_ref()) {
        Some(text) => text,
        None => return Err(StartVotingError::NoTextInMessage),
    };

    let api = API.wait();

    let (min_votes, rounds) = match parse_params_from_message(message_text) {
        Some((min_votes, rounds)) => (min_votes, rounds),
        None => {
            api.send_message(
                &SendMessageParams::builder()
                    .chat_id(message.chat.id)
                    .text("Invalid parameters; see /help for command usage.")
                    .reply_to_message_id(message.message_id)
                    .build(),
            )
            .await
            .map_err(StartVotingError::SendMessageFailed)?;
            return Ok(());
        }
    };

    let mut db = DB.wait().lock().await;
    let t = db
        .transaction()
        .await
        .map_err(StartVotingError::StartTransactionFailed)?;

    let tournament = match t
        .query_opt(
            r#"SELECT "id" FROM "tournaments" WHERE "chat_id" = $1 AND "state" = $2"#,
            &[&message.chat.id, &TournamentState::Submitting],
        )
        .await
        .map_err(StartVotingError::QueryTournamentsFailed)?
    {
        Some(tournament) => tournament,
        None => {
            api.send_message(
                &SendMessageParams::builder()
                    .chat_id(message.chat.id)
                    .text("The tournament must be in submission phase to start voting.")
                    .reply_to_message_id(message.message_id)
                    .build(),
            )
            .await
            .map_err(StartVotingError::SendMessageFailed)?;
            return Ok(());
        }
    };
    let tournament_id = tournament.get("id");

    let count = t.execute(
        r#"UPDATE "tournaments" SET "state" = $1, "min_votes" = $2, "rounds" = $3 WHERE "id" = $4"#,
        &[
            &TournamentState::Voting,
            &min_votes.as_i16,
            &rounds.as_i16,
            &tournament_id,
        ],
    )
    .await
    .map_err(StartVotingError::UpdateTournamentFailed)?;
    if count != 1 {
        return Err(StartVotingError::DbIntegrityError(format!(
            "expected to update one tournament, updated {count} rows",
        )));
    }

    let rounds = rounds.as_u32;

    if let Err(err) = create_bracket(&t, tournament_id, rounds).await {
        match err {
            CreateBracketError::NotEnoughSubmissions(submission_count, min_submissions) => {
                let rounds_str = match rounds {
                    1 => "a single round".to_string(),
                    rounds => format!("{rounds} rounds"),
                };
                api.send_message(
                    &SendMessageParams::builder()
                        .chat_id(message.chat.id)
                        .text(match submission_count {
                            0 => format!(
                                "There are no submissions. At least {min_submissions} \
                                 are needed for {rounds_str}. {confused}",
                                confused = Kaomoji::CONFUSED,
                            ),
                            1 => format!(
                                "There is only one submission. At least {min_submissions} \
                                 are needed for {rounds_str}. {confused}",
                                confused = Kaomoji::CONFUSED,
                            ),
                            _ => format!(
                                "There are only {submission_count} submissions. At least {min_submissions} \
                                 are needed for {rounds_str}. {confused}",
                                confused = Kaomoji::CONFUSED,
                            ),
                        })
                        .reply_to_message_id(message.message_id)
                        .build(),
                )
                .await
                .map_err(StartVotingError::SendMessageFailed)?;
                return Ok(());
            }
            _ => return Err(err.into()),
        }
    }

    let (poll_id, message_id) = send_poll(&t, message.chat.id, tournament_id, 0).await?;

    t.execute(
        r#"
        UPDATE "matchups" SET
            "poll_id" = $1,
            "message_id" = $2,
            "state" = 'started',
            "animation_a_votes" = 0,
            "animation_b_votes" = 0,
            "started_at" = $3
        WHERE "tournament_id" = $4 AND "index" = 0
        "#,
        &[&poll_id, &message_id, &Utc::now(), &tournament_id],
    )
    .await
    .map_err(StartVotingError::UpdateFirstMatchupFailed)?;

    t.commit()
        .await
        .map_err(StartVotingError::CommitTransactionFailed)?;

    if let Err(err) = update_chat_commands(message.chat.id, Some(TournamentState::Voting)).await {
        eprintln!("failed to update chat commands: {err}");
    }

    Ok(())
}
