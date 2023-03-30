mod gist;

use lazy_static::lazy_static;
use thiserror::Error;
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
    #[error("missing credentials")]
    NoCredentials,
}

lazy_static! {
    static ref GIST_ORIGIN: Origin = "https://gist.github.com/".parse::<Url>().unwrap().origin();
    static ref GIST_RAW_ORIGIN: Origin = "https://gist.githubusercontent.com/"
        .parse::<Url>()
        .unwrap()
        .origin();
}

async fn find_loader(url: &Url) -> Result<WebModule> {
    let origin = url.origin();
    if origin == *GIST_ORIGIN || origin == *GIST_RAW_ORIGIN {
        gist::load_gist_from_url(url).await
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
