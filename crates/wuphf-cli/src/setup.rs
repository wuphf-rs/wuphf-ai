//! `wuphf-server setup`: the core's config file and the env file its options
//! name, edited on a setup screen without the core running.

use anyhow::Result;
use std::path::PathBuf;

pub fn run(config: PathBuf, env_file: PathBuf) -> Result<()> {
    botconf_tui::run(botconf_tui::Setup {
        description: wuphf_settings::description(),
        config,
        env_file,
        // The core's own checks, the tokens read from the env file being
        // written before the process environment.
        validate: Box::new(|candidate| {
            let env = candidate.env.clone();
            wuphf_settings::check(candidate.config, move |name| {
                env.get(name).cloned().or_else(|| std::env::var(name).ok())
            })
        }),
    })?;
    Ok(())
}
