use std::{convert::Infallible, time::Duration};

use chrono::Utc;
use clap::{Args, Parser, Subcommand};
use clokwerk::{AsyncScheduler, TimeUnits};
use deadpool_postgres::{tokio_postgres::NoTls, Transaction};
use frankenstein::{
    AllowedUpdate, AsyncApi, AsyncTelegramApi, BotCommand, BotCommandScope, SetMyCommandsParams,
    SetWebhookParams,
};

use bot::{
    config::{Config, ConfigError},
    db::{init_db, TournamentState},
    scheduled::run_scheduled_task,
    util::{flatten_handle, update_chat_commands, ThreadError},
    API, BOT_USERNAME, CONFIG, DB,
};
use tokio::{sync::Mutex, task::JoinHandle};

#[derive(Parser)]
struct CliArgs {
    #[clap(short, long, default_value = "config.toml")]
    config: String,
    #[command(subcommand)]
    subcommand: Option<CliSubcommand>,
}

#[derive(Subcommand)]
enum CliSubcommand {
    InitDb(InitDbArgs),
    Run,
}

#[derive(Args)]
struct InitDbArgs {
    #[arg(long)]
    create_user: bool,
    #[arg(long)]
    create_database: bool,
    #[arg(long)]
    drop_existing: bool,
}

#[tokio::main]
async fn main() {
    let args = CliArgs::parse();
    let config = match Config::from_file(&args.config) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("config error: {}", err);
            std::process::exit(1);
        }
    };

    match args.subcommand {
        Some(CliSubcommand::InitDb(init_db_args)) => {
            if let Err(err) = init_db(
                &config,
                init_db_args.create_user,
                init_db_args.create_database,
                init_db_args.drop_existing,
            )
            .await
            {
                eprintln!("failed to initialize database: {}", err);
                std::process::exit(1);
            }
            println!("database initialized");
            return;
        }
        Some(CliSubcommand::Run) | None => {
            if let Err(err) = run(config).await {
                eprintln!("error: {}", err);
                std::process::exit(1);
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum RunError {
    #[error("config error: {0}")]
    ConfigError(#[from] ConfigError),
    #[error("failed to create database connection pool: {0}")]
    DbPoolError(#[from] deadpool_postgres::CreatePoolError),
    #[error("database error: {0}")]
    DbError(#[from] deadpool_postgres::PoolError),
    #[error("tried to set global {0} more than once")]
    GlobalAlreadySet(&'static str),
    #[error("global {0} is unset")]
    GlobalNotSet(&'static str),
    #[error("startup tasks failed: {0}")]
    StartupError(#[from] StartupError),
    #[error("thread failed: {0}")]
    ThreadError(#[from] ThreadError),
}

async fn run(config: Config) -> Result<(), RunError> {
    CONFIG
        .set(config)
        .or(Err(RunError::GlobalAlreadySet("CONFIG")))?;
    let config = CONFIG.get().ok_or(RunError::GlobalNotSet("CONFIG"))?;

    let db_pool = config.db.create_pool(None, NoTls)?;
    let db = db_pool.get().await?;
    DB.set(Mutex::new(db))
        .or(Err(RunError::GlobalAlreadySet("DB")))?;

    if let Err(_) = API.set(AsyncApi::new(&config.bot.token)) {
        eprintln!("failed to set API");
        std::process::exit(1);
    }

    let mut scheduler = AsyncScheduler::with_tz(Utc);
    scheduler
        .every(config.scheduler.job_interval_secs.seconds())
        .run(move || run_on_schedule());
    let scheduler_thread: JoinHandle<Result<(), Infallible>> = tokio::spawn(async move {
        loop {
            scheduler.run_pending().await;
            tokio::time::sleep(Duration::from_millis(config.scheduler.poll_interval_millis)).await;
        }
    });

    let webhook_thread = { tokio::spawn(async move { bot::webhook::listen().await }) };
    let server_thread = { tokio::spawn(async move { bot::server::listen().await }) };

    on_startup().await?;

    let join_result = tokio::try_join!(
        flatten_handle(scheduler_thread),
        flatten_handle(server_thread),
        flatten_handle(webhook_thread),
    );
    join_result?;

    println!("finished");
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum StartupError {
    #[error("API error: {0}")]
    ApiError(#[from] frankenstein::Error),
    #[error("failed to update commands: {0}")]
    SetCommandsError(#[from] SetCommandsError),
}

async fn on_startup() -> Result<(), StartupError> {
    let api = API.wait();
    let config = CONFIG.wait();

    BOT_USERNAME
        .set(api.get_me().await?.result.username)
        .unwrap();

    api.set_webhook(
        &SetWebhookParams::builder()
            .url(config.webhook.url.clone())
            .secret_token(config.webhook.secret.clone())
            .allowed_updates([AllowedUpdate::Message, AllowedUpdate::Poll])
            .build(),
    )
    .await?;

    set_commands().await?;

    Ok(())
}

async fn run_on_schedule() {
    let config = CONFIG.wait();
    if tokio::time::timeout(
        Duration::from_secs(config.scheduler.job_timeout_secs),
        run_scheduled_task(),
    )
    .await
    .is_err()
    {
        eprintln!("scheduled task timed out");
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SetCommandsError {
    #[error("API error: {0}")]
    ApiError(#[from] frankenstein::Error),
    #[error(transparent)]
    DbError(#[from] deadpool_postgres::tokio_postgres::Error),
}

async fn set_commands() -> Result<(), SetCommandsError> {
    let api = API.wait();
    let mut db = DB.wait().lock().await;
    let t = db.transaction().await?;

    let set_global_commands = async {
        api.set_my_commands(
            &SetMyCommandsParams::builder()
                .commands(vec![BotCommand::builder()
                    .command("help")
                    .description("Get help")
                    .build()])
                .build(),
        )
        .await
    };

    let set_global_admin_commands = async {
        api.set_my_commands(
            &SetMyCommandsParams::builder()
                .commands(vec![
                    BotCommand::builder()
                        .command("start")
                        .description("Start the GIFdome")
                        .build(),
                    BotCommand::builder()
                        .command("help")
                        .description("Get help")
                        .build(),
                ])
                .scope(BotCommandScope::AllChatAdministrators)
                .build(),
        )
        .await
    };

    let jobs = t
        .query(r#"SELECT "id" FROM "chats""#, &[])
        .await?
        .into_iter()
        .map(|row| set_chat_commands(&t, row.get("id")));

    let (set_global_commands_res, set_global_admin_commands_res, set_chat_commands_results) = tokio::join!(
        set_global_commands,
        set_global_admin_commands,
        futures::future::join_all(jobs),
    );
    if let Err(set_global_commands_err) = set_global_commands_res {
        return Err(set_global_commands_err.into());
    }
    if let Err(set_global_admin_commands_err) = set_global_admin_commands_res {
        return Err(set_global_admin_commands_err.into());
    }
    for res in set_chat_commands_results {
        if let Err(err) = res {
            return Err(err.into());
        }
    }
    Ok(())
}

async fn set_chat_commands(t: &Transaction<'_>, chat_id: i64) -> Result<(), SetCommandsError> {
    let tournament = t
        .query_opt(
            r#"
            SELECT "state" FROM "tournaments"
            WHERE "chat_id" = $1 AND "state" IN ($2, $3)
            "#,
            &[
                &chat_id,
                &TournamentState::Submitting,
                &TournamentState::Voting,
            ],
        )
        .await?;
    match update_chat_commands(
        chat_id,
        tournament.map(|tournament| tournament.get::<_, TournamentState>("state")),
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(err) => Err(err.into()),
    }
}
