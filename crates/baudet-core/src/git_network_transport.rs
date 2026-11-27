use crate::context::GitNetworkDestination;
use git2::{
    AutotagOption, Config, Cred, CredentialType, Error, FetchOptions, ProxyOptions, PushOptions, Reference, Remote,
    RemoteCallbacks, RemoteRedirect, Repository,
};
use std::path::Path;
use std::sync::OnceLock;

#[derive(Clone, serde::Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case")]
pub enum CredentialConfig {
    Https { username: String, token_env: String },
    Ssh { username: String, private_key: std::path::PathBuf, passphrase_env: Option<String> },
}

pub fn run_staged(
    workspace: &Path,
    destination: &GitNetworkDestination,
    credentials: Option<CredentialConfig>,
    remote_name: &str,
    branch: &str,
    push: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    configure_network_timeouts()?;
    let git = workspace.join(".git");
    if !git.is_dir()
        || [
            "commondir",
            "gitdir",
            "config.worktree",
            "info/sparse-checkout",
            "objects/info/alternates",
            "objects/info/http-alternates",
        ]
        .iter()
        .any(|entry| git.join(entry).exists())
    {
        return Err("network Git requires a standalone repository with its own object store".into());
    }
    let config = git.join("config");
    let previous_config = std::fs::read(&config)?;
    // The staging area is private and has been copied without links.
    std::fs::write(
        &config,
        "[core]\nrepositoryformatversion = 0\nfilemode = false\nbare = false\n[http]\nsslVerify = true\n",
    )?;
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let repository = Repository::open(workspace)?;
        if repository.workdir().and_then(|path| path.canonicalize().ok()) != Some(workspace.canonicalize()?)
            || repository.path().canonicalize()? != git.canonicalize()?
        {
            return Err("the repository resolved outside the staging workspace".into());
        }
        if repository.state() != git2::RepositoryState::Clean
            || repository.head()?.name() != Some(branch_ref(branch)?.as_str())
        {
            return Err(
                "network Git requires the approved branch to be checked out and the repository to be idle".into()
            );
        }
        let token = match &credentials {
            Some(CredentialConfig::Https { token_env, .. }) => Some(std::env::var(token_env)?),
            Some(CredentialConfig::Ssh { passphrase_env: Some(name), .. }) => Some(std::env::var(name)?),
            _ => None,
        };
        let authentication = match &credentials {
            None => Authentication::None,
            Some(CredentialConfig::Https { username, .. }) => {
                Authentication::Https { username, token: token.as_deref().ok_or("missing HTTPS token")? }
            }
            Some(CredentialConfig::Ssh { username, private_key, .. }) => {
                if !private_key.is_absolute() {
                    return Err("the configured SSH private key path must be absolute".into());
                }
                Authentication::Ssh { username, private_key, passphrase: token.as_deref() }
            }
        };
        let bare = Repository::open_bare(&git)?;
        if push {
            return Ok(push_branch(&bare, destination, authentication, branch)?);
        }
        if !repository.statuses(None)?.is_empty() {
            return Err("pull requires a clean index and working tree".into());
        }
        let tracking_ref = format!("refs/remotes/{remote_name}/{branch}");
        if !Reference::is_valid_name(&tracking_ref) || bare.find_reference(FETCHED_REF).is_ok() {
            return Err("the repository cannot record this fetched reference".into());
        }
        let current = repository.head()?.target().ok_or("the checked-out branch has no commit")?;
        let fetched = fetch_branch(&bare, destination, authentication, branch)?;
        if current != fetched {
            if !bare.graph_descendant_of(fetched, current)? {
                return Err("pull would not be a fast-forward".into());
            }
            // Reopen after fetch so the working repository sees any new packs.
            let repository = Repository::open(workspace)?;
            let target = repository.find_object(fetched, Some(git2::ObjectType::Commit))?;
            let mut checkout = git2::build::CheckoutBuilder::new();
            checkout.safe().disable_filters(true);
            repository.checkout_tree(&target, Some(&mut checkout))?;
            repository.head()?.set_target(fetched, "baudet: fast-forward pull")?;
        }
        bare.reference(&tracking_ref, fetched, true, "baudet: pull")?;
        let mut fetched_ref = bare.find_reference(FETCHED_REF)?;
        fetched_ref.delete()?;
        Ok(())
    })();
    std::fs::write(config, previous_config)?;
    result
}

fn configure_network_timeouts() -> Result<(), Error> {
    static NETWORK: OnceLock<Result<(), String>> = OnceLock::new();
    // Only this transport uses libgit2 sockets. Initialize their global limits
    // before any staged operation connects, even when sessions start together.
    NETWORK
        .get_or_init(|| unsafe {
            git2::opts::set_server_connect_timeout_in_milliseconds(15_000)
                .and_then(|_| git2::opts::set_server_timeout_in_milliseconds(15_000))
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map(|_| ())
        .map_err(|error| Error::from_str(error))
}

pub fn validate_branch(branch: &str) -> Result<(), Error> {
    branch_ref(branch).map(|_| ())
}

/// Selected by the host application for the approved destination, outside
/// the model's tool arguments.
pub enum Authentication<'a> {
    None,
    Https { username: &'a str, token: &'a str },
    Ssh { username: &'a str, private_key: &'a Path, passphrase: Option<&'a str> },
}

const FETCHED_REF: &str = "refs/baudet/remote";

/// Fetch only the approved branch into a staging repository. The caller can
/// inspect the fetched branch before updating the user's working tree.
pub fn fetch_branch(
    staging: &Repository,
    destination: &GitNetworkDestination,
    authentication: Authentication<'_>,
    branch: &str,
) -> Result<git2::Oid, Error> {
    let branch = branch_ref(branch)?;
    let mut remote = staging_remote(staging, destination)?;
    let mut options = FetchOptions::new();
    options
        .remote_callbacks(callbacks(destination, authentication)?)
        .follow_redirects(RemoteRedirect::None)
        .proxy_options(ProxyOptions::new())
        .download_tags(AutotagOption::None)
        .update_fetchhead(false);
    remote.fetch(&[format!("{branch}:{FETCHED_REF}")], Some(&mut options), None)?;
    staging.refname_to_id(FETCHED_REF)
}

/// Push a single branch from a staging repository. The remote may only
/// perform a normal fast-forward update.
pub fn push_branch(
    staging: &Repository,
    destination: &GitNetworkDestination,
    authentication: Authentication<'_>,
    branch: &str,
) -> Result<(), Error> {
    let branch = branch_ref(branch)?;
    let mut remote = staging_remote(staging, destination)?;
    let mut callbacks = callbacks(destination, authentication)?;
    callbacks.push_update_reference(|_, rejection| match rejection {
        Some(_) => Err(Error::from_str("the Git server rejected the branch update")),
        None => Ok(()),
    });
    let mut options = PushOptions::new();
    options.remote_callbacks(callbacks).follow_redirects(RemoteRedirect::None).proxy_options(ProxyOptions::new());
    remote.push(&[format!("{branch}:{branch}")], Some(&mut options))
}

fn branch_ref(branch: &str) -> Result<String, Error> {
    let reference = format!("refs/heads/{branch}");
    if !Reference::is_valid_name(&reference) || branch.starts_with('-') {
        return Err(Error::from_str("invalid Git branch name"));
    }
    Ok(reference)
}

fn staging_remote<'repo>(
    staging: &'repo Repository,
    destination: &GitNetworkDestination,
) -> Result<Remote<'repo>, Error> {
    if !staging.is_bare() {
        return Err(Error::from_str("network Git requires a bare staging repository"));
    }
    let remote = staging.remote_anonymous(&destination.address)?;
    if remote.url() != Some(destination.address.as_str())
        || remote.pushurl().is_some_and(|address| address != destination.address)
    {
        return Err(Error::from_str("Git configuration rewrote the approved destination"));
    }
    Ok(remote)
}

fn callbacks<'a>(
    destination: &'a GitNetworkDestination,
    authentication: Authentication<'a>,
) -> Result<RemoteCallbacks<'a>, Error> {
    if !matches!(
        (destination.url.scheme(), &authentication),
        ("https", Authentication::None | Authentication::Https { .. })
            | ("ssh", Authentication::None | Authentication::Ssh { .. })
    ) {
        return Err(Error::from_str("the approved destination needs matching authentication"));
    }

    let mut issued = false;
    let mut try_agent = cfg!(windows)
        || std::env::var_os("SSH_AUTH_SOCK").is_some_and(|socket| !socket.is_empty());
    let mut default_keys = ["id_ed25519", "id_ecdsa", "id_rsa"].into_iter();
    let mut callbacks = RemoteCallbacks::new();
    // Keep libgit2's TLS certificate and SSH known_hosts checks in force.
    callbacks.credentials(move |url, user, allowed| {
        if url != destination.address || issued {
            return Err(Error::from_str("credentials are unavailable for this Git connection"));
        }
        match &authentication {
            Authentication::None if destination.url.scheme() == "https"
                && allowed.contains(CredentialType::USER_PASS_PLAINTEXT) =>
            {
                issued = true;
                // Use the user's Git configuration, without repository-local helpers.
                Cred::credential_helper(&Config::open_default()?, destination.url.as_str(), user)
            }
            Authentication::None if destination.url.scheme() == "ssh"
                && allowed.contains(CredentialType::SSH_KEY)
                && user == Some(destination.url.username()) =>
            {
                let username = destination.url.username();
                if std::mem::take(&mut try_agent) {
                    if let Ok(credential) = Cred::ssh_key_from_agent(username) {
                        return Ok(credential);
                    }
                }
                if let Some(home) = std::env::home_dir() {
                    let ssh = home.join(".ssh");
                    for name in default_keys.by_ref() {
                        let private_key = ssh.join(name);
                        if private_key.is_file() {
                            return Cred::ssh_key(username, None, &private_key, None);
                        }
                    }
                }
                Err(Error::from_str("no usable SSH agent identity or default SSH private key was found"))
            }
            Authentication::Https { username, token } if allowed.contains(CredentialType::USER_PASS_PLAINTEXT) => {
                issued = true;
                Cred::userpass_plaintext(username, token)
            }
            Authentication::Ssh { username, private_key, passphrase }
                if allowed.contains(CredentialType::SSH_KEY) && user == Some(username) =>
            {
                issued = true;
                Cred::ssh_key(username, None, private_key, *passphrase)
            }
            Authentication::Ssh { username, .. } if allowed.contains(CredentialType::USERNAME) && user.is_none() => {
                Cred::username(username)
            }
            _ => Err(Error::from_str("the Git server requires unsupported authentication")),
        }
    });
    Ok(callbacks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::git_network_destination;

    #[test]
    fn given_an_approved_https_remote_when_staging_config_rewrites_it_then_network_access_is_refused() {
        let project = tempfile::tempdir().unwrap();
        let repo = Repository::init(project.path()).unwrap();
        repo.remote("origin", "https://approved.invalid/team/project.git").unwrap();
        let approved = git_network_destination(project.path(), "origin", false).unwrap();
        let stage = tempfile::tempdir().unwrap();
        let bare = Repository::init_bare(stage.path()).unwrap();
        bare.config()
            .unwrap()
            .set_str("url.https://alternate.invalid/.insteadOf", "https://approved.invalid/")
            .unwrap();

        let result = fetch_branch(&bare, &approved, Authentication::None, "main");
        assert!(result.unwrap_err().message().contains("rewrote the approved destination"));
    }

    #[test]
    fn given_an_ssh_remote_when_https_credentials_are_selected_then_network_access_is_refused() {
        let project = tempfile::tempdir().unwrap();
        let repo = Repository::init(project.path()).unwrap();
        repo.remote("origin", "ssh://git@approved.invalid/team/project.git").unwrap();
        let approved = git_network_destination(project.path(), "origin", true).unwrap();
        let stage = tempfile::tempdir().unwrap();
        let bare = Repository::init_bare(stage.path()).unwrap();
        let credentials = Authentication::Https { username: "fixture", token: "fixture" };

        let error = push_branch(&bare, &approved, credentials, "main").unwrap_err();
        assert!(error.message().contains("matching authentication"));
    }
}
