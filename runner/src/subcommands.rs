use crate::config::Env;
use crate::review_threads;
use crate::sender::EventSender;
use anyhow::{Context, Result, bail};
use tracing::info;

fn read_result_argument(arg: &str) -> Result<serde_json::Value> {
    let raw = match arg.strip_prefix('@') {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("reading the result from {path}"))?,
        None => arg.to_owned(),
    };
    let value: serde_json::Value =
        serde_json::from_str(raw.trim()).context("the result is not valid JSON")?;
    if !value.is_object() {
        bail!(
            "the result must be a JSON object, so whatever reads it next can find its fields by name"
        );
    }
    Ok(value)
}

pub(crate) async fn result(args: &[String]) -> Result<()> {
    let arg = args
        .get(2)
        .context("usage: ferrfleet-runner result '<json>' | @<file>")?;
    let result = read_result_argument(arg)?;

    let env = Env::from_env().context("loading environment")?;
    let sender = EventSender::new(env);
    sender
        .report_result(&result)
        .await
        .context("reporting the run result")?;

    info!("result recorded");
    Ok(())
}

/// `ferrfleet-runner resolve-thread <thread_id>`.
///
/// Outil expose a l'agent (liste dans ses tools autorises), pas la
/// politique: c'est le prompt canonique de l'agent (migration 053) qui
/// decide quand resoudre un fil, cette sous-commande se contente d'executer
/// la mutation. Le token GitHub est recupere aupres de l'API via le meme
/// appel que `workspace::prepare` (`EventSender::fetch_github_token`), donc
/// jamais donne directement a l'agent.
pub(crate) async fn resolve_thread(args: &[String]) -> Result<()> {
    let thread_id = args
        .get(2)
        .context("usage: ferrfleet-runner resolve-thread <thread_id>")?;

    let env = Env::from_env().context("loading environment")?;
    let sender = EventSender::new(env);
    let token = sender
        .fetch_github_token()
        .await
        .context("recuperation du token GitHub aupres de l'API")?;

    review_threads::resolve_thread(&token, thread_id)
        .await
        .context("resolution du fil de review")?;

    info!(thread_id = %thread_id, "fil de review resolu");
    Ok(())
}

/// `ferrfleet-runner pull-request <url>`.
///
/// Point de bouclage de la boucle de review: l'agent annonce la PR qu'il
/// vient d'ouvrir, l'API renseigne `ticket_runs.pr_url`. Sans cet appel, une
/// review humaine sur cette PR n'est jamais reconnue comme portant sur une PR
/// de l'agent, et la boucle de review ne demarre jamais. La validation de
/// l'URL et le rattachement au run sont faits cote API, pas ici.
pub(crate) async fn pull_request(args: &[String]) -> Result<()> {
    let url = args
        .get(2)
        .context("usage: ferrfleet-runner pull-request <url>")?;

    let env = Env::from_env().context("loading environment")?;
    let sender = EventSender::new(env);
    sender
        .report_pull_request(url)
        .await
        .context("enregistrement de la PR aupres de l'API")?;

    info!(url = %url, "PR enregistree, boucle de review armee");
    Ok(())
}

#[cfg(test)]
mod result_argument_tests {
    use super::*;

    #[test]
    fn an_inline_object_is_read() {
        let value =
            read_result_argument(r#"{"verdict":"changes_requested","findings":2}"#).unwrap();
        assert_eq!(value["verdict"], "changes_requested");
        assert_eq!(value["findings"], 2);
    }

    #[test]
    fn anything_that_is_not_an_object_is_refused() {
        for arg in [r#""done""#, "[1,2]", "42", "null"] {
            assert!(
                read_result_argument(arg).is_err(),
                "{arg} would leave the next reader with no field to look up"
            );
        }
    }

    #[test]
    fn broken_json_is_refused_before_anything_is_sent() {
        assert!(read_result_argument("{not json").is_err());
    }

    #[test]
    fn a_file_argument_is_read_from_disk() {
        let path =
            std::env::temp_dir().join(format!("ferrfleet-result-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(
            &path,
            "  {\"summary\": \"ok\"}
",
        )
        .expect("writing the fixture");

        let value = read_result_argument(&format!("@{}", path.display())).unwrap();

        assert_eq!(value["summary"], "ok");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_file_says_which_one() {
        let err = read_result_argument("@/nope/missing.json").unwrap_err();
        assert!(format!("{err}").contains("missing.json"));
    }
}
