//! Résolution des fils de review GitHub.
//!
//! La résolution de fil n'existe pas en REST. GitHub ne l'expose qu'en
//! GraphQL, donc ni le MCP GitHub ni un simple appel REST ne suffisent :
//! c'est le seul endroit du runner qui parle GraphQL.
//!
//! Le token n'apparaît jamais dans argv ni dans les journaux : il n'est
//! utilisé que dans l'en-tête `Authorization` de la requête HTTP.

use anyhow::{Context, Result, bail};
use serde_json::json;

const GITHUB_GRAPHQL_URL: &str = "https://api.github.com/graphql";

/// La mutation GraphQL de résolution, paramétrée.
///
/// Une seule mutation par appel : résoudre en lot masquerait des fils non
/// traités.
///
/// L'identifiant du fil est une VARIABLE (`$id`), jamais interpolé dans le
/// littéral : il vient de l'argv que l'agent a construit, donc d'une source
/// influencée par le contenu d'un ticket ou d'une review. Interpolé, une
/// valeur portant `"` et `}` refermait la mutation et en ajoutait d'autres,
/// exécutées avec le token d'installation, fusion comprise.
pub const RESOLVE_MUTATION: &str =
    "mutation($id: ID!) { resolveReviewThread(input: {threadId: $id}) { thread { isResolved } } }";

/// Un identifiant de node GraphQL GitHub est un identifiant opaque en
/// base64url (`PRRT_kwDO...`). Tout ce qui sort de cet alphabet est refusé
/// avant l'appel : la validation ne remplace pas le paramétrage ci-dessus,
/// elle s'y ajoute (une valeur absurde doit échouer ici, pas chez GitHub).
#[must_use]
pub fn is_valid_thread_id(thread_id: &str) -> bool {
    !thread_id.is_empty()
        && thread_id.len() <= 256
        && thread_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '='))
}

/// Poste la mutation de résolution sur l'API GraphQL de GitHub, authentifiée
/// par le token d'installation. Toute erreur GraphQL remonte telle quelle,
/// sans jamais exposer le token dans le message.
pub async fn resolve_thread(token: &str, thread_id: &str) -> Result<()> {
    if !is_valid_thread_id(thread_id) {
        bail!("identifiant de fil de review invalide");
    }

    let client = reqwest::Client::new();
    let body = json!({
        "query": RESOLVE_MUTATION,
        "variables": { "id": thread_id },
    });

    let resp = client
        .post(GITHUB_GRAPHQL_URL)
        .bearer_auth(token)
        .header("User-Agent", "ferrfleet-runner")
        .json(&body)
        .send()
        .await
        .context("appel a l'API GraphQL de GitHub")?
        .error_for_status()
        .context("reponse non-2xx de l'API GraphQL de GitHub")?;

    let payload: serde_json::Value = resp
        .json()
        .await
        .context("decodage de la reponse GraphQL")?;

    if let Some(errors) = payload.get("errors") {
        bail!("erreur GraphQL en resolvant le fil {thread_id}: {errors}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mutation_targets_one_thread_and_interpolates_nothing() {
        assert!(RESOLVE_MUTATION.contains("resolveReviewThread"));
        // Une seule mutation par appel: resoudre en lot masquerait des fils non traites.
        assert_eq!(RESOLVE_MUTATION.matches("resolveReviewThread").count(), 1);
        // L'identifiant passe par une variable, jamais par le litteral.
        assert!(RESOLVE_MUTATION.contains("$id"));
        assert!(!RESOLVE_MUTATION.contains('"'));
    }

    #[test]
    fn a_thread_id_that_could_close_the_mutation_is_refused() {
        assert!(!is_valid_thread_id(
            "x\"}) { thread { isResolved } } m2: mergePullRequest(input: {pullRequestId: \"y"
        ));
        assert!(!is_valid_thread_id(""));
        assert!(!is_valid_thread_id("PRRT kwDO"));
        assert!(is_valid_thread_id("PRRT_kwDOAbC-123="));
    }
}
