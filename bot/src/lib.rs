use std::collections::HashSet;

use deadpool_postgres::Object;
use frankenstein::AsyncApi;
use once_cell::sync::{Lazy, OnceCell};
use tokio::sync::Mutex;

use crate::{animation::find_duplicates, config::Config};

pub mod animation;
mod command;
pub mod config;
pub mod db;
pub mod scheduled;
pub mod server;
mod tournament;
pub mod util;
pub mod webhook;

pub static API: OnceCell<AsyncApi> = OnceCell::new();
pub static BOT_USERNAME: OnceCell<Option<String>> = OnceCell::new();
pub static DB: OnceCell<Mutex<Object>> = OnceCell::new();
pub static CONFIG: OnceCell<Config> = OnceCell::new();
pub static POSSIBLE_DUPLICATES: Lazy<Mutex<Vec<HashSet<String>>>> = Lazy::new(|| {
    Mutex::new(match find_duplicates() {
        Ok(duplicates) => duplicates,
        Err(err) => {
            eprintln!("failed to find duplicates: {err}");
            Vec::new()
        }
    })
});
