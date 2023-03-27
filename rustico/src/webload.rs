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
}

lazy_static! {
    static ref GIST_ORIGIN: Origin = "https://gist.github.com/".parse::<Url>().unwrap().origin();
    static ref GIST_RAW_ORIGIN: Origin = "https://gist.githubusercontent.com/"
        .parse::<Url>()
        .unwrap()
        .origin();
}

async fn load_gist_from_url(url: &Url) -> Result<WebModule> {
    debug_assert_eq!(url.scheme(), "https");
    debug_assert_eq!(url.host(), Some(url::Host::Domain("gist.github.com")));

    let _segments: Vec<_> = url
        .path_segments()
        .ok_or(InvalidUrl::InvalidPath)?
        .collect();

    // https://gist.github.com/sorcio/477bb75059341c4dfaef1b0c0849677f
    // /<user>/<gist_id>

    // TODO: call API to fetch content and filename
    Err(InvalidUrl::RejectedOrigin.into())
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
