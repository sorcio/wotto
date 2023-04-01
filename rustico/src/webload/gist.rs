use super::{Domain, InvalidUrl, ResolvedModule, ResolverResult, WebError};
use crate::service::Result;
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

fn extract_gist_from_json(json: serde_json::Value, gist: Gist) -> Option<GistResolvedModule> {
    let files = json.as_object()?.get("files")?.as_object()?;
    let (name, file) = if let Some(file_name) = gist.file_path() {
        // if a file name is specified, it must be valid
        (file_name.to_string(), files.get(file_name)?.as_object()?)
    } else {
        // must guess the file name
        guess_gist_file_name(files, &gist)?
    };

    // Find content already in the json document, if not truncated. We might
    // disregard this later, so let's not make a copy yet.
    let have_content = !file
        .get("truncated")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let content = if have_content {
        file.get("content").and_then(|c| c.as_str())
    } else {
        None
    };

    // We know the file exists, but does it match the requested revision? We
    // have three cases:
    // 1) no specific revision was requested, so we must assume latest
    // 2) a specific revision (commit sha) was requested; the api call was
    //    already made for the correct revision, so we can assume that the file
    //    we see here is the file we need
    // 3) a raw url was given, which contains a blob sha, but not a commit sha;
    //    we have the option to validate that the url matches the info on gh,
    //    but that would require us to either make multiple api calls until we
    //    find a file matching the blob sha, or use git directly to fetch the
    //    right object (afaik the gists api doesn't have a way to fetch by
    //    blob). since neither is implemented (maybe todo?) what we do now is
    //    just to trust the info given in the raw url.

    let raw_url: Url = file.get("raw_url")?.as_str()?.parse().ok()?;

    if let Some(blob) = gist.blob() {
        // case (3) above, let's trust the raw url given by the user; if the
        // file happens to be already the latest we might be in luck because
        // perhaps we have the content
        debug_assert!(gist.file_path().is_some());

        let blob = blob.to_string();
        if gist.eq_with_blob(&raw_url) {
            // ok, let's use the json content (if any)
            Some(GistResolvedModule::new(
                gist,
                name,
                blob,
                content.map(|s| s.bytes().collect()),
            ))
        } else {
            // we disregard the json entirely
            let file_path = gist
                .file_path()
                .expect("raw url Gists should always be created with a file_path")
                .to_string();
            Some(GistResolvedModule::new(gist, file_path, blob, None))
        }
    } else {
        // either case 1 or 2, which are handled the same way
        let parsed = Gist::new(&raw_url).ok()?;
        let blob = parsed.blob()?.to_string();
        Some(GistResolvedModule::new(
            gist,
            name,
            blob,
            content.map(|s| s.bytes().collect()),
        ))
    }
}

fn github_basic_auth() -> Result<(String, String)> {
    let text = std::fs::read_to_string("github.token").map_err(|_| WebError::NoCredentials)?;
    let lines: Vec<_> = text.split_ascii_whitespace().take(2).collect();
    match lines[..] {
        [username, password] => Ok((username.to_string(), password.to_string())),
        _ => Err(WebError::NoCredentials.into()),
    }
}

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::ClientBuilder::new()
        .user_agent("https://github.com/sorcio/rusto")
        .https_only(true)
        .build()
        .map_err(WebError::ReqwestError)?)
}

pub(super) async fn resolve_gist(url: &Url) -> Result<impl ResolverResult> {
    debug_assert_eq!(url.scheme(), "https");
    debug_assert!(matches!(
        url.host(),
        Some(url::Host::Domain(
            "gist.github.com" | "gist.githubusercontent.com"
        ))
    ));

    let gist = Gist::new(url)?;
    let api_url = gist.build_api_url();
    let client = client()?;
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

    extract_gist_from_json(json, gist).ok_or(WebError::NotWasm.into())
}

pub(crate) async fn load_content(module: &mut ResolvedModule) -> Result<()> {
    if module.content().is_some() {
        return Ok(());
    }
    let resolver_result = module.downcast::<GistResolvedModule>();
    let fetch_url = resolver_result.build_raw_url();

    let client = reqwest::ClientBuilder::new()
        .user_agent("https://github.com/sorcio/rusto")
        .https_only(true)
        .build()
        .map_err(WebError::ReqwestError)?;
    let (username, password) = github_basic_auth()?;
    let request = client
        .request(reqwest::Method::GET, fetch_url)
        .header("Accept", "application/vnd.github.raw")
        .basic_auth(&username, Some(&password))
        .build()
        .map_err(WebError::ReqwestError)?;
    let response = client
        .execute(request)
        .await
        .map_err(WebError::TemporaryFailure)?
        .error_for_status()
        .map_err(WebError::TemporaryFailure)?;
    let content = response.bytes().await.map_err(WebError::TemporaryFailure)?;
    resolver_result.set_content(content);
    Ok(())
}

struct GistResolvedModule {
    user: String,
    gist_id: String,
    file_path: String,
    blob: String,
    content: Option<Vec<u8>>,
}

impl GistResolvedModule {
    fn new(gist: Gist, file_path: String, blob: String, content: Option<Vec<u8>>) -> Self {
        Self {
            user: gist.user().to_string(),
            gist_id: gist.gist_id().to_string(),
            file_path,
            blob,
            content,
        }
    }

    fn build_raw_url(&self) -> String {
        let Self {
            user,
            gist_id,
            blob,
            file_path,
            ..
        } = self;
        format!("https://gist.githubusercontent.com/{user}/{gist_id}/raw/{blob}/{file_path}")
    }

    fn set_content<B: Into<Vec<u8>>>(&mut self, content: B) {
        assert!(self.content.is_none(), "set_content() requires that content is None");
        self.content = Some(content.into());
    }
}

impl ResolverResult for GistResolvedModule {
    fn domain(&self) -> Domain {
        Domain::Github
    }

    fn user(&self) -> &str {
        &self.user
    }

    fn name(&self) -> &str {
        &self.file_path
    }

    fn content(&self) -> Option<&[u8]> {
        self.content.as_deref()
    }

    fn take_content(&mut self) -> Option<Vec<u8>> {
        self.content.take()
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
