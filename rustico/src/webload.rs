use itertools::Itertools;
use lazy_static::lazy_static;
use thiserror::Error;
use tracing::info;
use url::{Origin, Url};

use crate::service::Result;

#[derive(Error, Debug)]
pub enum InvalidUrl {
    #[error("url cannot be parsed")]
    ParseError,
    #[error("rejected origin")]
    RejectedOrigin,
    #[error("url cannot contain username or password")]
    CredentialsNotAllowed,
    #[error("invalid path")]
    InvalidPath,
}

#[derive(Error, Debug)]
pub enum WebError {
    #[error("temporary failure: {0}")]
    TemporaryFailure(#[source] reqwest::Error),
    #[error("web client error: {0}")]
    ReqwestError(#[source] reqwest::Error),
    #[error("not a webassembly module")]
    NotWasm,
    #[error("file too large")]
    TooLarge,
}

lazy_static! {
    static ref GIST_ORIGIN: Origin = "https://gist.github.com/".parse::<Url>().unwrap().origin();
    static ref GIST_RAW_ORIGIN: Origin = "https://gist.githubusercontent.com/"
        .parse::<Url>()
        .unwrap()
        .origin();
}

fn is_hex_string(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Parsed Gist url
struct Gist<'a> {
    user: &'a str,
    gist_id: &'a str,
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

        match &segments[..] {
            // raw gist url:
            // /<user>/<gist_id>/raw/<commit>/<filepath>
            &[user, gist_id, "raw", commit, file_path] => {
                if !is_hex_string(gist_id) || !is_hex_string(commit) || file_path.is_empty() {
                    Err(InvalidUrl::InvalidPath.into())
                } else {
                    Ok(Self {
                        user,
                        gist_id,
                        commit: Some(commit),
                        file_path: Some(file_path),
                        fragment: None,
                    })
                }
            }

            // gist.github.com url:
            // /<user>/<gist_id>
            // /<user>/<gist_id>#file-<filename-with-dashes>
            &[user, gist_id] => {
                if !is_hex_string(gist_id) {
                    Err(InvalidUrl::InvalidPath.into())
                } else {
                    Ok(Self {
                        user,
                        gist_id,
                        commit: None,
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

    fn commit(&self) -> Option<&'a str> {
        self.commit
    }

    fn file_path(&self) -> Option<&'a str> {
        self.file_path
    }

    fn file_name_hint(&self) -> Option<GistFileNameHint<'a>> {
        self.fragment?.strip_prefix("file-").map(GistFileNameHint)
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
        if let Ok((k, v)) = files.iter().filter(|(k, _)| hint.matches(&k)).exactly_one() {
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
        // TODO fetch raw file instead
        let raw_url = file.get("raw_url")?.as_str()?.to_string();
        return Some(GistGuessResult::MustFetch {
            user,
            name,
            raw_url,
        });
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
async fn load_gist_from_url(url: &Url) -> Result<WebModule> {
    debug_assert_eq!(url.scheme(), "https");
    debug_assert_eq!(url.host(), Some(url::Host::Domain("gist.github.com")));

    let gist = Gist::new(url)?;
    let api_url = gist.build_api_url();
    let client = reqwest::Client::new();
    let request = client
        .request(reqwest::Method::GET, api_url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        // .bearer_auth(token)  // TODO
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
                // .bearer_auth(token)  // TODO
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

async fn load_gist_from_raw_url(url: &Url) -> Result<WebModule> {
    debug_assert_eq!(url.scheme(), "https");
    debug_assert_eq!(
        url.host(),
        Some(url::Host::Domain("gist.githubusercontent.com"))
    );

    let segments: Vec<_> = url
        .path_segments()
        .ok_or(InvalidUrl::InvalidPath)?
        .collect();

    // /sorcio/477bb75059341c4dfaef1b0c0849677f/raw/dd1250c85e31ca7541a04f70ed8d77c586bc0377/math.wat
    // /<user>/<gist_id>/raw/<commit>/<filepath>

    if segments.len() != 5 || segments[2] != "raw" {
        return Err(InvalidUrl::InvalidPath.into());
    }

    let user = segments[0];
    let gist_id = segments[1];
    let commit = segments[3];
    let file_name = segments[4];

    info!("load_gist_from_raw_url() url={url} user={user} gist_id={gist_id} commit={commit} file_name={file_name}");

    // just to be extremely careful about sanitizing, we rebuild the url
    let content_path = format!("{user}/{gist_id}/raw/{commit}/{file_name}");
    let content_url = Url::parse(&GIST_RAW_ORIGIN.unicode_serialization())
        .unwrap()
        .join(&content_path)
        .unwrap();

    info!("load_gist_from_raw_url() content_url={content_url}");

    let r = reqwest::get(content_url)
        .await
        .map_err(WebError::TemporaryFailure)?
        .error_for_status()
        .map_err(WebError::TemporaryFailure)?;

    let content = r.bytes().await.map_err(WebError::TemporaryFailure)?;

    Ok(WebModule::new(
        Domain::Github,
        user.to_string(),
        file_name.to_string(),
        content,
    ))
}

async fn find_loader(url: &Url) -> Result<WebModule> {
    let origin = url.origin();
    // WEB_LOADERS
    //     .get(&origin)
    //     .ok_or(InvalidUrl::RejectedOrigin.into())
    //     .copied()
    if origin == *GIST_ORIGIN {
        load_gist_from_url(url).await
    } else if origin == *GIST_RAW_ORIGIN {
        load_gist_from_raw_url(url).await
    } else {
        Err(InvalidUrl::RejectedOrigin.into())
    }
}

/// Domain defines the domain for the user, in case one day we want to have a
/// more complex namespacing scheme, or code authentication. E.g.
/// `Domain::Github` indicates that the user (in `WebModule`) is a GitHub user.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Domain {
    Github,
    #[allow(dead_code)]
    Other(&'static str),
}

#[derive(Debug)]
pub(crate) struct WebModule {
    domain: Domain,
    user: String,
    name: String,
    content: Vec<u8>,
}

impl WebModule {
    fn new<B>(domain: Domain, user: String, name: String, content: B) -> Self
    where
        B: Into<Vec<u8>>,
    {
        Self {
            domain,
            user,
            name,
            content: content.into(),
        }
    }

    pub(crate) fn domain(&self) -> Domain {
        self.domain
    }

    pub(crate) fn user(&self) -> &str {
        &self.user
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn content(&self) -> &[u8] {
        &self.content
    }
}

pub(crate) async fn load_module_from_url(url: Url) -> Result<WebModule> {
    let module = find_loader(&url).await?;
    Ok(module)
}
