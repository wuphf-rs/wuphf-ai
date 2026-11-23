//! `wuphf-server update`: every installed binary to its newest release (or
//! back, with `--rollback`), and `serve`'s scheduled check telling the owner
//! when a release is out.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use wuphf_runtime::owner_send::OwnerMessageSender;
use wuphf_storage::Storage;
use wuphf_update::config::UpdatesConfig;
use wuphf_update::notice::Announcer;
use wuphf_update::plan::{Action, Plan};
use wuphf_update::services::{Services, Systemd};
use wuphf_update::{Applied, PlanOptions, Updater, http};

/// The first scheduled check waits this long after start: a core in a restart
/// loop does not ask the forges every time.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(120);

#[derive(clap::Args)]
pub(crate) struct UpdateArguments {
    /// The core's config: its [updates] section says where the binaries are
    /// and where their releases come from (/etc/wuphf/core.toml when
    /// installed as deploy/systemd says).
    #[arg(long, default_value = "config.local.toml")]
    config: PathBuf,
    /// Where the token variables [updates] names come from, then the
    /// environment.
    #[arg(long, default_value = ".env.local")]
    env_file: PathBuf,
    /// Only say what an update would change.
    #[arg(long)]
    check: bool,
    /// Install without asking.
    #[arg(short, long)]
    yes: bool,
    /// Build every source at its release's tag instead of downloading.
    #[arg(long)]
    from_source: bool,
    /// Only this binary (repeatable); one not installed yet is installed.
    #[arg(long, value_name = "BINARY")]
    only: Vec<String>,
    /// Leave the systemd units alone (by default the ones running an updated
    /// binary are restarted).
    #[arg(long)]
    no_restart: bool,
    /// Put back the binaries the last update replaced.
    #[arg(long, conflicts_with_all = ["check", "from_source", "only"])]
    rollback: bool,
}

pub(crate) async fn run(arguments: UpdateArguments) -> Result<()> {
    let config = updates_config(&arguments.config)?;
    let env = crate::backup::env_lookup(&arguments.env_file)?;
    let updater = Updater::new(&config, &env, Arc::new(http::Client::new()?))?;
    let services: Option<&dyn Services> = (!arguments.no_restart).then_some(&Systemd);
    let progress = |step: &str| eprintln!("  {step}");

    if arguments.rollback {
        let replaced = updater.installer().replaced()?;
        if replaced.is_empty() {
            println!("The last update replaced nothing to put back.");
            return Ok(());
        }
        confirm(
            &format!("Put back the previous {}?", replaced.join(", ")),
            arguments.yes,
        )?;
        let applied = updater.rollback(services, &progress).await?;
        print_applied(&applied);
        return finished(&applied);
    }

    let plan = updater
        .plan(&PlanOptions {
            from_source: arguments.from_source,
            only: arguments.only,
        })
        .await?;
    print!("{}", describe(&plan, updater.installer().install_dir()));
    if !plan.has_updates() || arguments.check {
        return Ok(());
    }
    let count = plan
        .sources
        .iter()
        .map(|source| source.updates().count())
        .sum::<usize>();
    confirm(&format!("Install {count} update(s)?"), arguments.yes)?;
    let applied = updater.apply(&plan, services, &progress).await?;
    print_applied(&applied);
    finished(&applied)
}

/// The `[updates]` section of the config at `path` (and the environment's
/// `WUPHF_CORE_UPDATES__*`); its defaults when there is none. Only this
/// section is read: an update needs none of the core's tokens.
fn updates_config(path: &Path) -> Result<UpdatesConfig> {
    let figment = wuphf_settings::base(path)?;
    if figment.find_value("updates").is_err() {
        return Ok(UpdatesConfig::default());
    }
    figment
        .extract_inner("updates")
        .with_context(|| format!("[updates] in {}", path.display()))
}

/// The plan, one line a binary, grouped by source.
fn describe(plan: &Plan, install_dir: &Path) -> String {
    let mut text = format!("Binaries in {}:\n", install_dir.display());
    for source in &plan.sources {
        let release = match &source.latest {
            Ok(Some(release)) => release,
            Ok(None) => {
                text.push_str(&format!("{}: no release yet\n", source.source.name));
                continue;
            }
            Err(error) => {
                text.push_str(&format!(
                    "{}: could not check: {error}\n",
                    source.source.name
                ));
                continue;
            }
        };
        let page = release
            .page
            .as_deref()
            .map(|page| format!(" ({page})"))
            .unwrap_or_default();
        text.push_str(&format!(
            "{} {}{page}\n",
            source.source.name, release.version
        ));
        for part in &source.parts {
            let line = match part.action {
                Action::NotInstalled | Action::NoRelease => continue,
                Action::Update => format!("{} → {}", part.installed, release.version),
                Action::Current => format!("{}, current", part.installed),
                Action::Ahead => format!("{}, newer than the release", part.installed),
            };
            text.push_str(&format!("  {:<22}{line}\n", part.binary));
        }
    }
    if !plan.has_updates() {
        text.push_str("Nothing to update.\n");
    }
    text
}

/// Ask before changing anything, unless `yes`; refuse without a terminal to
/// ask on.
fn confirm(question: &str, yes: bool) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        bail!("{question} Run again with --yes to go ahead without asking.");
    }
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    if matches!(answer.trim(), "y" | "Y" | "yes") {
        Ok(())
    } else {
        bail!("nothing changed")
    }
}

fn print_applied(applied: &Applied) {
    for (binary, from, to) in &applied.installed {
        println!("{binary}: {from} → {to}");
    }
    for unit in &applied.restarted {
        println!("restarted {unit}");
    }
    for (unit, error) in &applied.restart_failures {
        eprintln!("could not restart {unit}: {error}");
    }
}

fn finished(applied: &Applied) -> Result<()> {
    if applied.restart_failures.is_empty() {
        Ok(())
    } else {
        bail!("the binaries are installed, but some units did not restart (see above)")
    }
}

/// Check for new releases every `check_interval_seconds` while the core
/// runs, telling the owner (through the notice targets) of each one once. The
/// check only reads: nothing is installed. Off when the interval is 0 or the
/// section cannot be used (logged).
pub(crate) fn spawn_check<S>(
    config: &UpdatesConfig,
    config_path: &Path,
    sender: OwnerMessageSender<S>,
) where
    S: Storage + Send + Sync + 'static,
{
    if config.check_interval_seconds == 0 {
        return;
    }
    let env = |name: &str| std::env::var(name).ok();
    let updater = match http::Client::new()
        .map_err(anyhow::Error::from)
        .and_then(|client| Ok(Updater::new(config, &env, Arc::new(client))?))
    {
        Ok(updater) => updater,
        Err(error) => {
            tracing::warn!(%error, "update checks are off: [updates] cannot be used");
            return;
        }
    };
    let config_path = std::path::absolute(config_path).unwrap_or_else(|_| config_path.to_owned());
    let mut announcer = Announcer::new(format!(
        "sudo wuphf-server update --config {}",
        config_path.display()
    ));
    let interval = Duration::from_secs(config.check_interval_seconds);
    tokio::spawn(async move {
        tokio::time::sleep(FIRST_CHECK_DELAY).await;
        loop {
            check_once(&updater, &mut announcer, &sender).await;
            tokio::time::sleep(interval).await;
        }
    });
}

async fn check_once<S>(updater: &Updater, announcer: &mut Announcer, sender: &OwnerMessageSender<S>)
where
    S: Storage,
{
    let plan = match updater.plan(&PlanOptions::default()).await {
        Ok(plan) => plan,
        Err(error) => {
            tracing::warn!(%error, "update check failed");
            return;
        }
    };
    for source in &plan.sources {
        if let Err(error) = &source.latest {
            tracing::warn!(source = %source.source.name, %error, "update check failed");
        }
    }
    for text in announcer.news(&plan) {
        tracing::info!(text, "a new release is out; telling the owner");
        if let Err(error) = sender.announce(&text).await {
            tracing::warn!(%error, "could not tell the owner of the new release");
        }
    }
}

#[cfg(test)]
mod tests;
