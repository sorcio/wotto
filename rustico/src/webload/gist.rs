use super::{Domain, InvalidUrl, WebError, WebModule};
use crate::service::Result;
use tracing::warn;
use url::Url;

fn is_hex_string(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Parsed Gist url
struct Gist<'a> {
    user: &'a str,
    gist_id: &'a str,
    blob: Option<&'a str>,
    commit: Option<&'a str>,
    file_path: Option<&'a str>,
    fragment: Option<&'a str>,
}

impl<'a> Gist<'a> {
    fn new(url: &'a Url) -> Result<Self> {
        Self::parse(url)
    }

    fn parse(url: &'a Url) -> Result<Self> {
        let segments: Vec<_> = url.path().trim_matches('/').splitn(5, '/').collect();

        match segments[..] {
            // raw gist url:
            // /<user>/<gist_id>/raw/<blob>/<filepath>
            [user, gist_id, "raw", blob, file_path] => {
                if !is_hex_string(gist_id) || !is_hex_string(blob) || file_path.is_empty() {
                    Err(InvalidUrl::InvalidPath.into())
                } else {
                    Ok(Self {
                        user,
                        gist_id,
                        blob: Some(blob),
                        commit: None,
                        file_path: Some(file_path),
                        fragment: None,
                    })
                }
            }

            // gist.github.com url:
            // /<user>/<gist_id>
            // /<user>/<gist_id>#file-<filename-with-dashes>
            [user, gist_id] => {
                if !is_hex_string(gist_id) {
                    Err(InvalidUrl::InvalidPath.into())
                } else {
                    Ok(Self {
                        user,
                        gist_id,
                        blob: None,
                        commit: None,
                        file_path: None,
                        fragment: url.fragment(),
                    })
                }
            }

            // gist.github.com url with revision:
            // /<user>/<gist_id>/<commit>
            // /<user>/<gist_id>/<commit>#file-<filename-with-dashes>
            [user, gist_id, commit] => {
                if !is_hex_string(gist_id) || !is_hex_string(commit) {
                    Err(InvalidUrl::InvalidPath.into())
                } else {
                    Ok(Self {
                        user,
                        gist_id,
                        blob: None,
                        commit: Some(commit),
                        file_path: None,
                        fragment: url.fragment(),
                    })
                }
            }

            _ => Err(InvalidUrl::InvalidPath.into()),
        }
    }

    fn build_api_url(&self) -> String {
        // see https://docs.github.com/en/rest/gists/gists?apiVersion=2022-11-28#get-a-gist
        if let Some(commit) = self.commit() {
            format!("https://api.github.com/gists/{}/{commit}", self.gist_id())
        } else {
            format!("https://api.github.com/gists/{}", self.gist_id())
        }
    }

    fn build_raw_url(&self) -> Option<String> {
        if let Self {
            user,
            gist_id,
            blob: Some(blob),
            file_path: Some(file_path),
            ..
        } = self
        {
            Some(format!(
                "https://gist.githubusercontent.com/{user}/{gist_id}/raw/{blob}/{file_path}"
            ))
        } else {
            None
        }
    }

    fn user(&self) -> &'a str {
        self.user
    }

    fn gist_id(&self) -> &'a str {
        self.gist_id
    }

    fn blob(&self) -> Option<&'a str> {
        self.blob
    }

    fn commit(&self) -> Option<&'a str> {
        self.commit
    }

    fn file_path(&self) -> Option<&'a str> {
        self.file_path
    }

    fn file_name_hint(&self) -> Option<GistFileNameHint<'a>> {
        self.fragment?.strip_prefix("file-").map(GistFileNameHint)
    }

    fn eq_with_blob(&self, url: &'_ Url) -> bool {
        if let Ok(other) = Gist::parse(url) {
            self.user() == other.user()
                && self.gist_id() == other.gist_id()
                && self.blob().is_some()
                && self.blob() == other.blob()
        } else {
            false
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct GistFileNameHint<'a>(&'a str);

impl GistFileNameHint<'_> {
    fn matches(self, other: &str) -> bool {
        if self.0.len() == other.len() {
            self.0.chars().zip(other.chars()).all(|(h, c)| {
                match (h.to_ascii_lowercase(), c.to_ascii_lowercase()) {
                    ('-', '.') => true,
                    (h, c) => h == c,
                }
            })
        } else {
            false
        }
    }
}

fn guess_gist_file_name<'a>(
    files: &'a serde_json::Map<String, serde_json::Value>,
    gist: &Gist,
) -> Option<(String, &'a serde_json::Map<String, serde_json::Value>)> {
    use itertools::Itertools;

    if files.len() == 1 {
        // only one file in the gist (common case)
        return files
            .iter()
            .next()
            .map(|(k, v)| Some((k.to_string(), v.as_object()?)))?;
    }
    if let Some(hint) = gist.file_name_hint() {
        // there are multiple files but we have a hint from the fragment
        // if it's a unique match, we use the hint. otherwise we go on
        if let Ok((k, v)) = files.iter().filter(|(k, _)| hint.matches(k)).exactly_one() {
            return Some((k.to_string(), v.as_object()?));
        }
    }
    // there are multiple files and the hint didn't match a specific one. so we
    // check if there is a unique file with a .wasm extension, or a unique one
    // with a .wat extension, or a unique one with language = "WebAssembly".
    if let Ok((k, v)) = files
        .iter()
        .filter(|(k, _)| k.ends_with(".wasm"))
        .exactly_one()
    {
        return Some((k.to_string(), v.as_object()?));
    }
    if let Ok((k, v)) = files
        .iter()
        .filter(|(k, _)| k.ends_with(".wat"))
        .exactly_one()
    {
        return Some((k.to_string(), v.as_object()?));
    }
    if let Ok((k, v)) = files
        .iter()
        .filter(|(_, v)| {
            v.as_object()
                .and_then(|o| o.get("language"))
                .and_then(|l| l.as_str())
                .map(|s| s == "WebAssembly")
                .unwrap_or(false)
        })
        .exactly_one()
    {
        return Some((k.to_string(), v.as_object()?));
    }
    None
}

enum GistGuessResult {
    Found(WebModule),
    MustFetch {
        user: String,
        name: String,
        raw_url: String,
    },
    NotFound,
}

impl From<Option<GistGuessResult>> for GistGuessResult {
    fn from(value: Option<GistGuessResult>) -> Self {
        match value {
            Some(g) => g,
            None => Self::NotFound,
        }
    }
}

fn extract_gist_from_json(json: serde_json::Value, gist: Gist) -> Option<GistGuessResult> {
    let files = json.as_object()?.get("files")?.as_object()?;
    let (name, file) = if let Some(file_name) = gist.file_path() {
        // if a file name is specified, it must be valid
        (file_name.to_string(), files.get(file_name)?.as_object()?)
    } else {
        // must guess the file name
        guess_gist_file_name(files, &gist)?
    };

    let user = gist.user().to_string();
    let content = if file.get("truncated")?.as_bool()? {
        let raw_url = file.get("raw_url")?.as_str()?.to_string();
        if gist.eq_with_blob(&raw_url.parse().ok()?) {
            return Some(GistGuessResult::MustFetch {
                user,
                name,
                raw_url,
            });
        } else {
            // TODO decide whether we want to validate the raw url (against
            // the gist commit history or something)
            warn!("partially supported case: raw gist with non-current revision");
            return Some(GistGuessResult::MustFetch {
                user,
                name,
                raw_url: gist.build_raw_url()?,
            });
        }
    } else {
        file.get("content")?.as_str()?
    };

    Some(GistGuessResult::Found(WebModule::new(
        Domain::Github,
        user,
        name,
        content,
    )))
}

fn github_basic_auth() -> Result<(String, String)> {
    let text = std::fs::read_to_string("github.token").map_err(|_| WebError::NoCredentials)?;
    let lines: Vec<_> = text.split_ascii_whitespace().take(2).collect();
    match lines[..] {
        [username, password] => Ok((username.to_string(), password.to_string())),
        _ => Err(WebError::NoCredentials.into()),
    }
}

pub(crate) async fn load_gist_from_url(url: &Url) -> Result<WebModule> {
    debug_assert_eq!(url.scheme(), "https");
    debug_assert!(matches!(
        url.host(),
        Some(url::Host::Domain(
            "gist.github.com" | "gist.githubusercontent.com"
        ))
    ));

    let gist = Gist::new(url)?;
    let api_url = gist.build_api_url();
    let client = reqwest::ClientBuilder::new()
        .user_agent("https://github.com/sorcio/rusto")
        .https_only(true)
        .build()
        .map_err(WebError::ReqwestError)?;
    let (username, password) = github_basic_auth()?;
    let request = client
        .request(reqwest::Method::GET, api_url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .basic_auth(&username, Some(&password))
        .build()
        .map_err(WebError::ReqwestError)?;
    let response = client
        .execute(request)
        .await
        .map_err(WebError::TemporaryFailure)?
        .error_for_status()
        .map_err(WebError::TemporaryFailure)?;

    let json = response
        .json::<serde_json::Value>()
        .await
        .map_err(WebError::ReqwestError)?;

    match extract_gist_from_json(json, gist).into() {
        GistGuessResult::Found(wm) => Ok(wm),
        GistGuessResult::MustFetch {
            user,
            name,
            raw_url,
        } => {
            let raw_req = client
                .request(reqwest::Method::GET, raw_url)
                .basic_auth(&username, Some(&password))
                .build()
                .map_err(WebError::ReqwestError)?;
            let content = client
                .execute(raw_req)
                .await
                .map_err(WebError::TemporaryFailure)?
                .error_for_status()
                .map_err(WebError::TemporaryFailure)?
                .bytes()
                .await
                .map_err(WebError::TemporaryFailure)?;
            Ok(WebModule::new(Domain::Github, user, name, content))
        }
        GistGuessResult::NotFound => Err(WebError::NotWasm.into()),
    }
}
