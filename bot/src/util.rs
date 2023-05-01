use std::convert::Infallible;

use frankenstein::{AsyncTelegramApi, Message, SendMessageParams, SetMyCommandsParams, BotCommand, BotCommandScope, BotCommandScopeChatAdministrators, DeleteMyCommandsParams};
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use tokio::task::JoinHandle;

use crate::{webhook::WebhookListenerError, API, db::TournamentState, server::ServerListenerError};

pub struct Kaomoji;
impl Kaomoji {
    pub const CONFUSED: &str = "\u{256e}(\u{30fb}_\u{30fb})\u{256d}";
    pub const EXCITED: &str = "(\u{2267}\u{25e1}\u{2266})";
    pub const HAPPY: &str = "(^\u{25bd}^)/";
    pub const FRUSTRATED: &str = "\u{ff61}\u{ff9f}\u{ff65} (>\u{fe4f}<) \u{ff65}\u{ff9f}\u{ff61}";
    pub const SAD: &str = "(\u{f3}\u{fe4f}\u{f2}\u{ff61})";
    pub const SHOCKED: &str = "\u{ff3c}(\u{3007}\u{ff4f})\u{ff0f}";
    pub const WINK: &str = "(^_<)\u{301c}\u{2606}";
}

#[derive(Debug, thiserror::Error)]
pub enum ThreadError {
    #[error("thread join error: {0}")]
    JoinError(#[from] tokio::task::JoinError),
    #[error("webhook listener failed: {0}")]
    WebhookListenerError(#[from] WebhookListenerError),
    #[error("server failed: {0}")]
    ServerListenerError(#[from] ServerListenerError),
}

impl From<Infallible> for ThreadError {
    fn from(_: Infallible) -> Self {
        unreachable!()
    }
}

pub async fn flatten_handle<T, E: Into<ThreadError>>(
    handle: JoinHandle<Result<T, E>>,
) -> Result<T, ThreadError> {
    match handle.await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(err)) => Err(err.into()),
        Err(err) => Err(err.into()),
    }
}

pub fn generate_token(length: u16) -> String {
    thread_rng()
        .sample_iter(&Alphanumeric)
        .take(length.into())
        .map(char::from)
        .collect()
}

pub async fn update_chat_commands(
    chat_id: i64,
    tournament_state: Option<TournamentState>,
) -> Result<(), frankenstein::Error> {
    let api = API.wait();

    match tournament_state {
        Some(TournamentState::Submitting) => {
            api.set_my_commands(
                &SetMyCommandsParams::builder()
                    .commands(vec![
                        BotCommand::builder()
                            .command("startvoting")
                            .description("Start the voting phase")
                            .build(),
                        BotCommand::builder()
                            .command("abort")
                            .description("Stop the tournament")
                            .build(),
                        BotCommand::builder()
                            .command("help")
                            .description("Get help")
                            .build(),
                    ])
                    .scope(BotCommandScope::ChatAdministrators(
                        BotCommandScopeChatAdministrators::builder()
                            .chat_id(chat_id)
                            .build(),
                    ))
                    .build(),
            )
            .await?;
        }
        Some(TournamentState::Voting) => {
            api.set_my_commands(
                &SetMyCommandsParams::builder()
                    .commands(vec![
                        BotCommand::builder()
                            .command("abort")
                            .description("Stop the tournament")
                            .build(),
                            BotCommand::builder()
                            .command("help")
                            .description("Get help")
                            .build(),
                    ])
                    .scope(BotCommandScope::ChatAdministrators(
                        BotCommandScopeChatAdministrators::builder()
                            .chat_id(chat_id)
                            .build(),
                    ))
                    .build(),
            )
            .await?;
        },
        Some(_) | None => {
            api.delete_my_commands(
                &DeleteMyCommandsParams::builder()
                    .scope(BotCommandScope::ChatAdministrators(
                        BotCommandScopeChatAdministrators::builder()
                            .chat_id(chat_id)
                            .build(),
                    ))
                    .build(),
            )
            .await?;
        }
    }
    Ok(())
}

pub async fn unexpected_error_reply(message: &Message) {
    let api = API.wait();
    let chat_id = message.chat.id;

    if let Err(err) = api
        .send_message(
            &SendMessageParams::builder()
                .chat_id(chat_id)
                .text(format!(
                    "I ran into an unexpected error {frustrated}",
                    frustrated = Kaomoji::FRUSTRATED,
                ))
                .reply_to_message_id(message.message_id)
                .build(),
        )
        .await
    {
        eprintln!("failed to send unexpected error reply to chat {chat_id}: {err}");
    }
}
