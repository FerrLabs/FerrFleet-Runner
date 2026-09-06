//! Prépare le dépôt de travail avant de lancer le CLI Claude Code.
//!
//! Clone le dépôt, bascule sur la branche du ticket (nouvelle ou existante)
//! et laisse le push à l'agent lui-même.
//!
//! Le token GitHub ne doit apparaître ni dans un argument de commande (visible
//! dans la table des process et dans `/proc/<pid>/cmdline` pendant toute la
//! durée du sous-processus), ni dans `.git/config`, ni dans les journaux, ni
//! dans l'environnement d'aucun processus qu'hérite l'agent. Il n'est donc
//! jamais mis dans l'URL du remote, ni dans une variable d'environnement
//! héritée : `git` le demande via `GIT_ASKPASS`, un script qui va lui-même le
//! chercher aupres de l'API (`GET /runs/{id}/github-token`, authentifie par
//! `FERRFLEET_RUN_TOKEN`) a chaque invocation, plutot que de le relayer
//! depuis une variable posee a l'avance. Le token ne transite donc jamais par
//! l'environnement du sous-processus `claude` ni d'aucun processus lance par
//! l'agent : seul le chemin (non secret) du script y figure.
//!
//! Ce canal d'authentification (`GitCredentials`) survit à la préparation :
//! il est rendu à l'appelant, qui le garde vivant pour toute la durée du run
//! et le passe au sous-processus `claude`. Sans lui, l'agent ne peut pas
//! pousser sa branche, alors que c'est précisément ce que son prompt lui
//! demande de faire. Le script est supprimé par son `Drop`, donc à la fin du
//! run comme en cas d'échec de la préparation.

use anyhow::{Context, Result, bail};
use ferrfleet_shared::Checkout;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;
use tracing::info;
use uuid::Uuid;

/// Script temporaire pointé par `GIT_ASKPASS` : a chaque invocation par
/// `git`, il recupere lui-meme un token aupres de l'API
/// (`FERRFLEET_API_URL`/`FERRFLEET_RUN_ID`/`FERRFLEET_RUN_TOKEN`, deja
/// presentes dans l'environnement du pod et donc heritees par ce script) et
/// le restitue a `git` sur sa sortie standard. Situé hors du dépôt
/// (répertoire temporaire du système), permissions restreintes au
/// propriétaire, et supprimé par son `Drop`, donc même quand `prepare`
/// échoue en cours de route.
struct AskpassScript {
    path: PathBuf,
}

impl AskpassScript {
    fn create() -> Result<Self> {
        let path = std::env::temp_dir().join(format!("ferrfleet-askpass-{}.sh", Uuid::new_v4()));
        // `-sf` : silencieux et echec net (statut non nul, rien sur stdout)
        // sur une reponse HTTP non-2xx. `sh` POSIX n'a pas `pipefail` : un
        // `curl | sed` sous `set -e` retient le statut de `sed`, qui reussit
        // trivialement meme sur une entree vide, donc un `curl` en echec
        // aurait quand meme laisse le script sortir en succes avec une
        // sortie vide. La reponse est donc capturee dans une variable AVANT
        // le `sed`, avec un `|| exit 1` explicite sur le `curl` lui-meme :
        // c'est ce qui fait echouer le script (et donc `git`) proprement sur
        // une API injoignable ou une reponse non-2xx, pas `-f` seul. Le
        // token n'est jamais un argument de `curl` : il est lu depuis la
        // reponse JSON, seul `FERRFLEET_RUN_TOKEN` (deja dans l'environnement
        // herite) sert d'identifiant pour l'appel.
        let script = "#!/bin/sh\nset -e\nresponse=$(curl -sf -H \"Authorization: Bearer ${FERRFLEET_RUN_TOKEN}\" \"${FERRFLEET_API_URL}/runs/${FERRFLEET_RUN_ID}/github-token\") || exit 1\nprintf '%s' \"$response\" | sed -n 's/.*\"token\"[[:space:]]*:[[:space:]]*\"\\([^\"]*\\)\".*/\\1/p'\n";
        std::fs::write(&path, script).context("ecriture du script GIT_ASKPASS")?;
        let mut perms = std::fs::metadata(&path)
            .context("lecture des permissions du script GIT_ASKPASS")?
            .permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(&path, perms)
            .context("restriction des permissions du script GIT_ASKPASS")?;
        Ok(Self { path })
    }
}

impl Drop for AskpassScript {
    fn drop(&mut self) {
        // Best effort : un `Drop` ne peut pas remonter d'erreur, et c'est le
        // dernier filet avant que le fichier ne traîne sur le disque.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Le canal d'authentification `git` d'un run : uniquement le script
/// `GIT_ASKPASS`, qui va lui-même chercher un token aupres de l'API a chaque
/// invocation.
///
/// Vit aussi longtemps que le run, et pas seulement le temps de la
/// préparation : `git push` est fait par l'agent, pas par le runner, donc le
/// sous-processus `claude` doit hériter des mêmes variables que les commandes
/// `git` d'ici. Aucun token n'est jamais place dans l'environnement d'aucun
/// processus par ce canal : seul le chemin du script `GIT_ASKPASS` (non
/// secret) y va.
///
/// Portee de la garantie : ceci ferme la fuite passive, un `env` accidentel
/// ou déclenché par une injection de prompt dans le contenu d'un ticket ne
/// recrache plus de token. Ca ne retire rien a l'agent : `FERRFLEET_RUN_TOKEN`
/// reste herite par le sous-processus `claude` (necessaire a d'autres appels
/// API du runner), et un agent deliberement malveillant peut toujours
/// appeler `GET /runs/{id}/github-token` lui-meme avec ce jeton. Cet acces
/// delibere reste borne par le fait que le token mint est deja scope au seul
/// depot du run.
pub struct GitCredentials {
    askpass: AskpassScript,
}

impl GitCredentials {
    fn create() -> Result<Self> {
        Ok(Self {
            askpass: AskpassScript::create()?,
        })
    }

    /// Donne à `cmd` de quoi s'authentifier auprès de GitHub sans jamais
    /// écrire ni token ni argument secret dans son environnement.
    pub fn apply(&self, cmd: &mut Command) {
        cmd.env("GIT_ASKPASS", &self.askpass.path)
            // Empêche tout repli interactif qui contournerait GIT_ASKPASS :
            // sans credentials valides, la commande échoue proprement plutôt
            // que d'attendre une saisie qui ne viendra jamais.
            .env("GIT_TERMINAL_PROMPT", "0");
    }
}

/// Clone `checkout.repo` dans `workdir` et bascule sur la branche demandée.
///
/// Rend le canal d'authentification `git` du run, que l'appelant doit garder
/// vivant : il porte le `GIT_ASKPASS` dont l'agent a besoin pour pousser.
///
/// Toute erreur nomme l'étape en échec, pour que le run s'arrête avant le
/// lancement du CLI plutôt que de laisser l'agent travailler dans un dépôt
/// à moitié prêt.
pub async fn prepare(checkout: &Checkout, workdir: &Path) -> Result<GitCredentials> {
    validate_branch_name(&checkout.branch)
        .context("etape 'validation du nom de branche' de la preparation du depot")?;

    // Créé avant le clone. En cas d'échec de la préparation, il sort de
    // portée ici et son `Drop` supprime le script ; en cas de succès, il est
    // rendu à l'appelant, qui le garde vivant pour la durée du run.
    let credentials = GitCredentials::create()
        .context("etape 'preparation du canal d'authentification' de la preparation du depot")?;

    clone(checkout, workdir, &credentials)
        .await
        .context("etape 'clone' de la preparation du depot")?;

    configure_identity(workdir)
        .await
        .context("etape 'configuration de l'identite git' de la preparation du depot")?;

    checkout_branch(checkout, workdir, &credentials)
        .await
        .context("etape 'checkout de la branche' de la preparation du depot")?;

    Ok(credentials)
}

async fn clone(checkout: &Checkout, workdir: &Path, credentials: &GitCredentials) -> Result<()> {
    // Aucun token dans l'URL : seul le nom d'utilisateur "x-access-token"
    // (fixe, non secret) y figure. `git` demande le mot de passe via
    // `GIT_ASKPASS`.
    let url = format!("https://x-access-token@github.com/{}.git", checkout.repo);

    let mut cmd = Command::new("git");
    cmd.current_dir(workdir)
        // Empêche git de retomber sur un credential.helper héritant de
        // l'environnement (keyring, cache, etc.) qui pourrait persister le
        // token en dehors de ce process.
        .arg("-c")
        .arg("credential.helper=")
        .arg("clone")
        .arg("--depth")
        .arg("50");

    if let Some(base) = &checkout.base_branch {
        cmd.arg("--branch").arg(base);
    }

    cmd.arg(&url).arg(".");
    credentials.apply(&mut cmd);

    info!(repo = %checkout.repo, base_branch = ?checkout.base_branch, "clonage du depot");
    run_git(cmd, "git clone").await
}

async fn configure_identity(workdir: &Path) -> Result<()> {
    let mut name_cmd = Command::new("git");
    name_cmd
        .current_dir(workdir)
        .arg("config")
        .arg("user.name")
        .arg("ferrfleet[bot]");
    run_git(name_cmd, "git config user.name").await?;

    let mut email_cmd = Command::new("git");
    email_cmd
        .current_dir(workdir)
        .arg("config")
        .arg("user.email")
        .arg("ferrfleet[bot]@users.noreply.github.com");
    run_git(email_cmd, "git config user.email").await
}

async fn checkout_branch(
    checkout: &Checkout,
    workdir: &Path,
    credentials: &GitCredentials,
) -> Result<()> {
    if checkout.existing {
        info!(branch = %checkout.branch, "reprise de la branche existante");

        // `--` termine l'analyse des options : un nom de branche commençant
        // par `-` ne peut plus être pris pour un flag de `git fetch`.
        let mut fetch_cmd = Command::new("git");
        fetch_cmd
            .current_dir(workdir)
            .arg("fetch")
            .arg("origin")
            .arg("--")
            .arg(&checkout.branch);
        credentials.apply(&mut fetch_cmd);
        run_git(fetch_cmd, "git fetch de la branche existante").await?;

        // Le clone met en place le refspec de suivi par défaut d'origin, donc
        // `refs/remotes/origin/<branche>` existe déjà après le fetch
        // ci-dessus. On le référence en forme pleinement qualifiée plutôt
        // qu'en DWIM `git checkout <branche>` : la chaîne "refs/remotes/..."
        // ne peut jamais commencer par `-`, donc ce point de départ n'est
        // jamais ambigu avec une option, quel que soit le contenu de
        // `branche`. L'argument de `-b` n'a pas ce problème non plus : `git`
        // consomme sans condition le jeton qui suit un flag qui attend une
        // valeur.
        let mut checkout_cmd = Command::new("git");
        checkout_cmd
            .current_dir(workdir)
            .arg("checkout")
            .arg("-b")
            .arg(&checkout.branch)
            .arg("--track")
            .arg(format!("refs/remotes/origin/{}", checkout.branch));
        run_git(checkout_cmd, "git checkout de la branche existante").await
    } else {
        info!(branch = %checkout.branch, "creation de la branche de travail");
        let mut cmd = Command::new("git");
        cmd.current_dir(workdir)
            .arg("checkout")
            .arg("-b")
            .arg(&checkout.branch);
        run_git(cmd, "git checkout -b").await
    }
}

/// Refuse en amont tout nom de branche qui commencerait par `-` : passé tel
/// quel à `git`, un tel nom serait pris pour une option plutôt que pour un
/// argument positionnel. La branche vient aujourd'hui de `branch_name`
/// (partagé), qui ne produit jamais ce préfixe, et demain de l'API GitHub
/// (tâche suivante, branche d'une PR existante) : cette validation ne doit
/// pas reposer sur la discipline de l'appelant, elle s'applique ici, à la
/// source de toute commande git.
fn validate_branch_name(branch: &str) -> Result<()> {
    if branch.is_empty() {
        bail!("nom de branche vide");
    }
    if branch.starts_with('-') {
        bail!(
            "nom de branche invalide : commence par '-' (confondu avec une option git) : {branch}"
        );
    }
    Ok(())
}

/// Lance une commande git et fait échouer avec un message qui nomme
/// l'étape. Le token n'apparaît plus dans aucun argument depuis le passage
/// à `GIT_ASKPASS` ; on continue néanmoins à ne jamais faire remonter
/// stdout/stderr bruts dans le message d'erreur, uniquement le code de
/// sortie.
async fn run_git(mut cmd: Command, step: &str) -> Result<()> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let output = cmd
        .output()
        .await
        .with_context(|| format!("lancement de {step}"))?;
    if !output.status.success() {
        bail!(
            "{step} a echoue avec le statut {}",
            output.status.code().unwrap_or(-1)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{AskpassScript, GitCredentials, validate_branch_name};
    use std::os::unix::fs::PermissionsExt;
    use uuid::Uuid;

    #[test]
    fn validate_branch_name_rejects_leading_dash() {
        assert!(validate_branch_name("-force").is_err());
    }

    #[test]
    fn validate_branch_name_rejects_empty() {
        assert!(validate_branch_name("").is_err());
    }

    #[test]
    fn validate_branch_name_accepts_normal_names() {
        assert!(validate_branch_name("ferrfleet/ticket-ft-142").is_ok());
    }

    /// Le canal d'authentification doit survivre a `prepare` : sans lui,
    /// `claude` ne recoit pas `GIT_ASKPASS` et l'agent ne peut pas pousser sa
    /// branche. Aucun token ne doit jamais apparaitre dans l'environnement
    /// pose par `apply` : c'est le script `GIT_ASKPASS` lui-meme qui va le
    /// chercher aupres de l'API a chaque invocation.
    #[test]
    fn credentials_carry_only_the_askpass_path_to_a_command() {
        let credentials = GitCredentials::create().expect("creation du canal");
        let mut cmd = tokio::process::Command::new("true");
        credentials.apply(&mut cmd);
        let env: std::collections::HashMap<_, _> = cmd
            .as_std()
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_str()?.to_owned(), v?.to_str()?.to_owned())))
            .collect();
        assert_eq!(
            env.get("GIT_TERMINAL_PROMPT").map(String::as_str),
            Some("0")
        );
        assert!(env.contains_key("GIT_ASKPASS"));
        assert!(
            !env.keys().any(|k| k.to_uppercase().contains("TOKEN")),
            "aucune variable d'environnement posee par apply() ne doit porter de token"
        );
    }

    #[test]
    fn askpass_script_is_removed_on_drop() {
        let script = AskpassScript::create().expect("creation du script askpass");
        let path = script.path.clone();
        assert!(path.exists(), "le script doit exister apres creation");
        drop(script);
        assert!(!path.exists(), "le script doit disparaitre apres Drop");
    }

    /// Regression pour l'absence de `pipefail` en `sh` POSIX (revue PR #689) :
    /// un `curl | sed` sous `set -e` retenait le statut de `sed`, qui reussit
    /// trivialement meme sur une entree vide. Ce test place un faux `curl` en
    /// tete de `PATH` qui echoue comme le ferait une API injoignable ou une
    /// reponse non-2xx, et verifie que le script askpass sort en erreur SANS
    /// rien imprimer, plutot que de rendre un statut 0 et un mot de passe
    /// vide a `git`.
    #[test]
    fn askpass_script_fails_closed_when_curl_fails() {
        let script = AskpassScript::create().expect("creation du script askpass");

        let fake_bin_dir =
            std::env::temp_dir().join(format!("ferrfleet-fake-bin-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&fake_bin_dir).expect("creation du repertoire du faux curl");
        let fake_curl = fake_bin_dir.join("curl");
        std::fs::write(&fake_curl, "#!/bin/sh\nexit 22\n").expect("ecriture du faux curl");
        let mut perms = std::fs::metadata(&fake_curl)
            .expect("lecture des permissions du faux curl")
            .permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(&fake_curl, perms).expect("permissions du faux curl");

        let real_path = std::env::var("PATH").unwrap_or_default();
        let fake_path = format!("{}:{real_path}", fake_bin_dir.display());

        let output = std::process::Command::new(&script.path)
            .env("PATH", fake_path)
            .env("FERRFLEET_API_URL", "http://127.0.0.1:1")
            .env("FERRFLEET_RUN_ID", "test-run")
            .env("FERRFLEET_RUN_TOKEN", "t0ken")
            .output()
            .expect("lancement du script askpass avec le faux curl");

        let _ = std::fs::remove_dir_all(&fake_bin_dir);

        assert!(
            !output.status.success(),
            "le script doit echouer quand curl echoue, pas rendre un statut 0"
        );
        assert!(
            output.stdout.is_empty(),
            "le script ne doit rien imprimer (donc pas de mot de passe vide) quand curl echoue"
        );
    }
}
