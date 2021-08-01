#[macro_use]
extern crate log;
#[macro_use]
extern crate anyhow;
#[macro_use]
extern crate serde;
#[macro_use]
extern crate async_trait;

use actix::clock::sleep;
use log::LevelFilter;
use primitives::ChainName;
use std::env;
use std::fs;
use std::time::Duration;

pub type Result<T> = std::result::Result<T, anyhow::Error>;

use actors::api::run_rest_api_server;
use actors::connector::run_connector;
use adapters::run_adapters;
use database::Database;
use notifier::SessionNotifier;

mod actors;
mod adapters;
mod database;
mod display_name;
mod notifier;
mod primitives;
#[cfg(test)]
mod tests;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub log_level: LevelFilter,
    pub db: DatabaseConfig,
    pub instance: InstanceType,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "config")]
pub enum InstanceType {
    AdapterListener(AdapterConfig),
    SessionNotifier(NotifierConfig),
    SingleInstance(SingleInstanceConfig),
}

// TODO: Do all of those need to be pubic fields?
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SingleInstanceConfig {
    pub adapter: AdapterConfig,
    pub notifier: NotifierConfig,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "snake_case")]
pub struct DatabaseConfig {
    pub uri: String,
    pub db_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct NotifierConfig {
    pub api_address: String,
    pub display_name: DisplayNameConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AdapterConfig {
    pub watcher: Vec<WatcherConfig>,
    pub matrix: MatrixConfig,
    pub twitter: TwitterConfig,
    pub email: EmailConfig,
    pub display_name: DisplayNameConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WatcherConfig {
    pub network: ChainName,
    pub endpoint: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DisplayNameConfig {
    pub enabled: bool,
    pub limit: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MatrixConfig {
    pub enabled: bool,
    pub homeserver: String,
    pub username: String,
    pub password: String,
    pub db_path: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TwitterConfig {
    pub enabled: bool,
    pub api_key: String,
    pub api_secret: String,
    pub token: String,
    pub token_secret: String,
    pub request_interval: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
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
    let content = fs::read_to_string("config.yaml")
        .or_else(|_| fs::read_to_string("/etc/registrar/config.yaml"))
        .map_err(|_| {
            anyhow!("Failed to open config at 'config.yaml' or '/etc/registrar/config.yaml'.")
        })?;

    // Parse config file as JSON.
    let config = serde_yaml::from_str::<Config>(&content)
        .map_err(|err| anyhow!("Failed to parse config: {:?}", err))?;

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
            .filter_module("system", config.log_level)
            .init();
    }

    println!("Logger initiated");

    Ok(config)
}

async fn config_adapter_listener(db_config: DatabaseConfig, config: AdapterConfig) -> Result<()> {
    let db = Database::new(&db_config.uri, &db_config.db_name).await?;

    // TODO: Pretty all the clones?
    let watchers = config.watcher.clone();
    run_adapters(config.clone(), db.clone()).await?;
    run_connector(db, watchers, config.display_name).await
}

async fn config_session_notifier(
    db_config: DatabaseConfig,
    not_config: NotifierConfig,
) -> Result<()> {
    let db = Database::new(&db_config.uri, &db_config.db_name).await?;
    let lookup = run_rest_api_server(not_config, db.clone()).await?;

    // TODO: Should be executed in `run_rest_api_server`
    actix::spawn(async move { SessionNotifier::new(db, lookup).run_blocking().await });

    Ok(())
}

// TODO: Check for database connectivity.
pub async fn run() -> Result<()> {
    let root = init_env()?;
    let (db_config, instance) = (root.db, root.instance);

    match instance {
        InstanceType::AdapterListener(config) => {
            info!("Starting adapter listener instance");
            config_adapter_listener(db_config, config).await?;
        }
        InstanceType::SessionNotifier(config) => {
            info!("Starting session notifier instance");
            config_session_notifier(db_config, config).await?;
        }
        InstanceType::SingleInstance(config) => {
            info!("Starting adapter listener and session notifier instances");
            let (adapter_config, notifier_config) = (config.adapter, config.notifier);

            let t1_db_config = db_config.clone();
            let t2_db_config = db_config;

            config_adapter_listener(t1_db_config, adapter_config).await?;
            config_session_notifier(t2_db_config, notifier_config).await?;
        }
    }

    info!("Setup completed");

    loop {
        sleep(Duration::from_secs(u64::MAX)).await;
    }
}
