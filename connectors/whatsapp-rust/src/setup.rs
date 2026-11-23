//! `wuphf-connector-whatsapp setup`: the config file and the env file its
//! options name, edited on a setup screen without the connector running.

use std::path::PathBuf;

use crate::config::{self, Config};

pub fn run(config: PathBuf, env_file: PathBuf) -> anyhow::Result<()> {
    botconf_tui::run(botconf_tui::Setup {
        description: config::description(),
        config,
        env_file,
        // What the connector checks when it starts: the file, then its core
        // token, read from the env file being written before the process
        // environment.
        validate: Box::new(|candidate| {
            let config = Config::parse(candidate.config).map_err(|error| error.to_string())?;
            let token = candidate
                .env
                .get(&config.core_token_env)
                .cloned()
                .or_else(|| std::env::var(&config.core_token_env).ok());
            match token {
                Some(token) if !token.is_empty() => Ok(()),
                _ => Err(format!(
                    "{} (core_token_env) is unset or empty: set it or generate it (g)",
                    config.core_token_env
                )),
            }
        }),
    })?;
    Ok(())
}
