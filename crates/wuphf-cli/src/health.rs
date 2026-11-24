//! `wuphf-server health` (plan 079): the open health problems. Without
//! `--run` they are read from the core's database, so it works while the
//! core is down; with `--run` the running core probes and checks again first
//! (its `HealthTransport.Run`, with the observer token from
//! `WUPHF_OBSERVER_TOKEN`). Exits non-zero while a problem not ignored is
//! open, for systemd or monitoring.

use anyhow::{Context, Result};
use std::path::Path;
use wuphf_proto::connector as pb;
use wuphf_proto::connector::health_transport_client::HealthTransportClient;
use wuphf_runtime::health::{HealthReport, report_to_wire};
use wuphf_storage::HealthStore;

/// Print the problems and say whether any is open (not ignored).
pub(crate) async fn run(config: &Path, check_now: bool, address: String) -> Result<bool> {
    let report = if check_now {
        let mut client = HealthTransportClient::connect(address.clone())
            .await
            .with_context(|| format!("failed to connect to {address}"))?;
        let mut request = tonic::Request::new(pb::RunHealthRequest {});
        if let Ok(token) = std::env::var("WUPHF_OBSERVER_TOKEN") {
            request.metadata_mut().insert(
                "authorization",
                format!("Bearer {token}")
                    .parse()
                    .context("WUPHF_OBSERVER_TOKEN is not a valid header value")?,
            );
        }
        client
            .run(request)
            .await
            .map_err(|status| anyhow::anyhow!("Run failed: {}", status.message()))?
            .into_inner()
    } else {
        let (storage, _) =
            crate::settings::open(config, wuphf_settings::WuphfSchema::new()).await?;
        report_to_wire(&HealthReport {
            problems: storage.health_problems().await?,
            ignored: storage.health_ignores().await?,
        })
    };
    print!("{}", render(&report));
    Ok(report.problems.iter().any(|problem| !problem.ignored))
}

/// One line per problem: whether it pauses or is ignored, its text and key.
fn render(report: &pb::HealthReport) -> String {
    if report.problems.is_empty() {
        return "healthy: no open problems\n".to_owned();
    }
    let mut out = String::new();
    for problem in &report.problems {
        let marker = match (problem.ignored, problem.pauses) {
            (true, _) => "ignored",
            (false, true) => "PAUSED",
            (false, false) => "warning",
        };
        out.push_str(&format!("{marker}: {}\n  {}\n", problem.text, problem.key));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn problems_render_one_each_with_their_state() {
        let problem = |key: &str, pauses, ignored| pb::HealthProblem {
            key: key.into(),
            text: format!("{key} is wrong."),
            pauses,
            ignored,
            since_ms: 1,
        };
        assert_eq!(
            render(&pb::HealthReport::default()),
            "healthy: no open problems\n"
        );
        let rendered = render(&pb::HealthReport {
            problems: vec![problem("a", true, false), problem("b", false, true)],
        });
        assert_eq!(
            rendered,
            "PAUSED: a is wrong.\n  a\nignored: b is wrong.\n  b\n"
        );
    }
}
