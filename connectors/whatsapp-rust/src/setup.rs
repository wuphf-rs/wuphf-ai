//! `wuphf-connector-whatsapp setup`: the config file and the env file its
//! options name, edited on a setup screen without the connector running, and
//! the account linked there (see `link`).

use std::path::PathBuf;

use tokio::runtime::Handle;

use crate::config::{self, Config};
use crate::link::{self, Mode};

const LINK: &str = "Link this connector's account here, so the connector starts linked, then fetch \
     its chats into the chat directory: the core's and misc's setup offer them by name. Uses the \
     saved config; the connector must not be running.";
const REFRESH: &str = "Fetch the linked account's chats into the chat directory again (new \
     groups, renamed chats). The connector must not be running.";

/// Runs the setup screen. Called from the connector's runtime: the actions
/// block on it from setup's worker thread.
pub fn run(config: PathBuf, env_file: PathBuf) -> anyhow::Result<()> {
    let runtime = Handle::current();
    let refresh_runtime = runtime.clone();
    let setup = botconf_tui::Setup::new(
        config::description(),
        config,
        env_file,
        // What the connector checks when it starts: the file, then its core
        // token, read from the env file being written before the process
        // environment.
        Box::new(|candidate| {
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
    )
    .action("Link account", LINK, move |screen, candidate| {
        let config = Config::parse(candidate.config).map_err(|error| error.to_string())?;
        link::run(&config, &runtime, screen, Mode::Link)
    })
    .action("Refresh chats", REFRESH, move |screen, candidate| {
        let config = Config::parse(candidate.config).map_err(|error| error.to_string())?;
        link::run(&config, &refresh_runtime, screen, Mode::Refresh)
    });
    botconf_tui::run(setup)?;
    Ok(())
}
