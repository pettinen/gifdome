use std::{collections::HashSet, path::PathBuf};

use deadpool_postgres::Config as DbConfig;
use serde::Deserialize;

#[derive(thiserror::Error, Debug)]
pub enum ConfigError {
    #[error("could not read file: {0}")]
    FileReadError(#[from] std::io::Error),
    #[error("invalid values: {0}")]
    InvalidConfig(#[from] ConfigValidationError),
    #[error("could not parse as TOML: {0}")]
    ParseError(#[from] toml::de::Error),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnimationConfigInput {
    allowed_mime_types: Vec<String>,
    max_duration_secs: u16,
    max_size_bytes: u64,
    save_dir: String,
    temp_filename_bits: u16,
    temp_save_dir: String,
    thumbnail_fingerprint_file: String,
    thumbnail_fingerprint_threshold: String,
    thumbnail_save_dir: String,
    vspipe_working_dir: String,
}

#[derive(Clone, Debug)]
pub struct AnimationConfig {
    pub allowed_mime_types: HashSet<String>,
    pub max_duration_secs: u16,
    pub max_size_bytes: u64,
    pub save_dir: PathBuf,
    pub temp_filename_length: u16,
    pub temp_save_dir: PathBuf,
    pub thumbnail_fingerprint_file: PathBuf,
    pub thumbnail_fingerprint_threshold: String,
    pub thumbnail_save_dir: PathBuf,
    pub vspipe_working_dir: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BotConfigInput {
    token: String,
}

#[derive(Clone, Debug)]
pub struct BotConfig {
    pub token: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DbConfigInput {
    user: String,
    password: String,
    dbname: String,
    application_name: Option<String>,
    host: Option<String>,
    port: Option<u16>,
}

impl DbConfigInput {
    fn as_db_config(self: &DbConfigInput) -> DbConfig {
        DbConfig {
            user: Some(self.user.clone()),
            password: Some(self.password.clone()),
            dbname: Some(self.dbname.clone()),
            application_name: self.application_name.clone(),
            host: self.host.clone(),
            port: self.port,
            ..DbConfig::default()
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DevConfigInput {
    debug: bool,
    init_db: Option<DbConfigInput>,
    testing: bool,
}

#[derive(Clone, Debug, Default)]
pub struct DevConfig {
    pub debug: bool,
    pub init_db: Option<DbConfig>,
    pub testing: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PollConfigInput {
    option_a_text: String,
    option_b_text: String,
}

#[derive(Clone, Debug)]
pub struct PollConfig {
    pub option_a_text: String,
    pub option_b_text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SchedulerConfigInput {
    job_interval_secs: u16,
    job_timeout_secs: u16,
    poll_interval_millis: u16,
}

#[derive(Clone, Debug)]
pub struct SchedulerConfig {
    pub job_interval_secs: u32,
    pub job_timeout_secs: u64,
    pub poll_interval_millis: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerConfigInput {
    socket_path: String,
    socket_permissions: u32,
}

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub socket_path: String,
    pub socket_permissions: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TournamentConfigInput {
    id_bits: u16,
    max_rounds: u8,
    round_lengths_secs: Vec<u16>,
}

#[derive(Clone, Debug)]
pub struct TournamentConfig {
    pub id_length: u16,
    pub max_rounds: u8,
    pub round_lengths_secs: Vec<u16>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WebhookConfigInput {
    secret: String,
    socket_path: String,
    socket_permissions: u32,
    url: String,
}

#[derive(Clone, Debug)]
pub struct WebhookConfig {
    pub secret: String,
    pub socket_path: String,
    pub socket_permissions: u32,
    pub url: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigInput {
    animation: AnimationConfigInput,
    bot: BotConfigInput,
    db: DbConfigInput,
    dev: Option<DevConfigInput>,
    poll: PollConfigInput,
    scheduler: SchedulerConfigInput,
    server: ServerConfigInput,
    tournament: TournamentConfigInput,
    webhook: WebhookConfigInput,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub animation: AnimationConfig,
    pub bot: BotConfig,
    pub dev: DevConfig,
    pub db: DbConfig,
    pub poll: PollConfig,
    pub scheduler: SchedulerConfig,
    pub server: ServerConfig,
    pub tournament: TournamentConfig,
    pub webhook: WebhookConfig,
}

fn alphanum_token_length(bits: u16) -> u16 {
    const ALPHABET_SIZE: f64 = 62.0; // [0-9A-Za-z]
    let result = (Into::<f64>::into(bits) / ALPHABET_SIZE.log2()).ceil();
    assert!(result <= u16::MAX.into());
    result as u16
}

impl Config {
    fn new(input: ConfigInput) -> Self {
        Self {
            animation: AnimationConfig {
                allowed_mime_types: input.animation.allowed_mime_types.into_iter().collect(),
                max_duration_secs: input.animation.max_duration_secs,
                max_size_bytes: input.animation.max_size_bytes,
                save_dir: input.animation.save_dir.into(),
                temp_filename_length: alphanum_token_length(input.animation.temp_filename_bits),
                temp_save_dir: input.animation.temp_save_dir.into(),
                thumbnail_fingerprint_file: input.animation.thumbnail_fingerprint_file.into(),
                thumbnail_fingerprint_threshold: input.animation.thumbnail_fingerprint_threshold,
                thumbnail_save_dir: input.animation.thumbnail_save_dir.into(),
                vspipe_working_dir: input.animation.vspipe_working_dir.into(),
            },
            bot: BotConfig {
                token: input.bot.token,
            },
            db: input.db.as_db_config(),
            dev: if let Some(dev_config) = &input.dev {
                DevConfig {
                    debug: dev_config.debug,
                    init_db: dev_config
                        .init_db
                        .as_ref()
                        .map(|config| config.as_db_config()),
                    testing: dev_config.testing,
                }
            } else {
                DevConfig::default()
            },
            poll: PollConfig {
                option_a_text: input.poll.option_a_text,
                option_b_text: input.poll.option_b_text,
            },
            scheduler: SchedulerConfig {
                job_interval_secs: input.scheduler.job_interval_secs.into(),
                job_timeout_secs: input.scheduler.job_timeout_secs.into(),
                poll_interval_millis: input.scheduler.poll_interval_millis.into(),
            },
            server: ServerConfig {
                socket_path: input.server.socket_path,
                socket_permissions: input.server.socket_permissions,
            },
            tournament: TournamentConfig {
                id_length: alphanum_token_length(input.tournament.id_bits),
                max_rounds: input.tournament.max_rounds,
                round_lengths_secs: input.tournament.round_lengths_secs,
            },
            webhook: WebhookConfig {
                secret: input.webhook.secret,
                socket_path: input.webhook.socket_path,
                socket_permissions: input.webhook.socket_permissions,
                url: input.webhook.url,
            },
        }
    }

    pub fn from_file(path: &str) -> Result<Self, ConfigError> {
        let path = std::fs::read_to_string(path)?;
        let input = toml::from_str(&path)?;
        let config = Self::new(input);
        validate_config(&config)?;
        Ok(config)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigValidationError {
    #[error("{0} cannot be empty")]
    EmptyValue(&'static str),
    #[error("round lengths must be specified for each round (as many as max_rounds)")]
    InvalidRoundLengths,
    #[error("allow at least one MIME type")]
    NoAllowedMimeTypes,
    #[error("poll options must be different")]
    PollOptionsEqual,
}
pub fn validate_config(config: &Config) -> Result<(), ConfigValidationError> {
    if config.animation.allowed_mime_types.is_empty() {
        return Err(ConfigValidationError::NoAllowedMimeTypes);
    }
    if config.bot.token.is_empty() {
        return Err(ConfigValidationError::EmptyValue("bot.token"));
    }
    if config.poll.option_a_text == config.poll.option_b_text {
        return Err(ConfigValidationError::PollOptionsEqual);
    }
    if config.tournament.round_lengths_secs.len() != config.tournament.max_rounds as usize {
        return Err(ConfigValidationError::InvalidRoundLengths);
    }
    if config.webhook.secret.is_empty() {
        return Err(ConfigValidationError::EmptyValue("webhook.secret"));
    }
    if config.webhook.url.is_empty() {
        return Err(ConfigValidationError::EmptyValue("webhook.url"));
    }
    Ok(())
}
