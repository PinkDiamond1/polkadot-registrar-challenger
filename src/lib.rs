#[macro_use]
extern crate log;
#[macro_use]
extern crate thiserror;
#[macro_use]
extern crate anyhow;
#[macro_use]
extern crate serde;
#[macro_use]
extern crate async_trait;

use std::env;
use std::fs;

pub type Result<T> = std::result::Result<T, anyhow::Error>;

mod actors;
mod adapters;
mod database;
mod primitives;
#[cfg(test)]
mod tests;
mod notifier;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub accounts: AccountsConfig,
    pub log_level: log::LevelFilter,
}

#[derive(Debug, Deserialize)]
// TODO: Rename to "Adapter"?
pub struct AccountsConfig {
    matrix: MatrixConfig,
    twitter: TwitterConfig,
    email: EmailConfig,
}

#[derive(Debug, Deserialize)]
pub struct MatrixConfig {
    pub enabled: bool,
    pub homeserver: String,
    pub username: String,
    pub password: String,
    pub db_path: String,
    // Since the Matrix SDK listens to responses in a stream, this value does
    // not require special considerations. But it should be often enough, given
    // that `AdapterListener` fetches the messages from the queue in intervals.
    pub request_interval: u64,
}

#[derive(Debug, Deserialize)]
pub struct TwitterConfig {
    pub enabled: bool,
    pub api_key: String,
    pub api_secret: String,
    pub token: String,
    pub token_secret: String,
    pub request_interval: u64,
}

#[derive(Debug, Deserialize)]
pub struct EmailConfig {
    pub enabled: bool,
    pub smtp_server: String,
    pub imap_server: String,
    pub inbox: String,
    pub user: String,
    pub password: String,
    pub request_interval: u64,
}

fn open_config() -> Result<Config> {
    // Open config file.
    let content = fs::read_to_string("config.json")
        .or_else(|_| fs::read_to_string("/etc/registrar/config.json"))
        .map_err(|_| {
            eprintln!("Failed to open config at 'config.json' or '/etc/registrar/config.json'.");
            std::process::exit(1);
        })
        .unwrap();

    // Parse config file as JSON.
    let config = serde_yaml::from_str::<Config>(&content)
        .map_err(|err| {
            eprintln!("Failed to parse config: {}", err);
            std::process::exit(1);
        })
        .unwrap();

    Ok(config)
}

pub fn init_env() -> Result<Config> {
    let config = open_config()?;

    // Env variables for log level overwrites config.
    if let Ok(_) = env::var("RUST_LOG") {
        println!("Env variable 'RUST_LOG' found, overwriting logging level from config.");
        env_logger::init();
    } else {
        println!("Setting log level to '{}' from config.", config.log_level);
        env_logger::builder()
            .filter_module("registrar", config.log_level)
            .init();
    }

    println!("Logger initiated");

    Ok(config)
}
