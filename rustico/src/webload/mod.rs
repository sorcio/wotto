mod gist;

use std::any::Any;
use std::collections::HashMap;

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

trait ResolverResult {
    fn domain(&self) -> Domain;
    fn user(&self) -> &str;
    fn name(&self) -> &str;
    fn content(&self) -> Option<&[u8]>;
    fn take_content(&mut self) -> Option<Vec<u8>>;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

pub(crate) struct ResolvedModule {
    loader: Loader,
    url: Url,
    resolved: Box<dyn ResolverResult + Send + Sync>,
}

impl ResolvedModule {
    pub(crate) fn url(&self) -> &Url {
        &self.url
    }

    pub(crate) fn domain(&self) -> Domain {
        self.resolved.domain()
    }

    pub(crate) fn user(&self) -> &str {
        self.resolved.user()
    }

    pub(crate) fn name(&self) -> &str {
        self.resolved.name()
    }

    pub(crate) fn content(&self) -> Option<&[u8]> {
        self.resolved.content()
    }

    pub(crate) async fn ensure_content(&mut self) -> Result<()> {
        self.loader.ensure_content(self).await
    }

    fn downcast<T: ResolverResult + 'static>(&mut self) -> &mut T {
        self.resolved
            .as_any_mut()
            .downcast_mut()
            .expect("downcast should be only called when the concrete type is known")
    }

}

impl core::fmt::Debug for ResolvedModule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedModule")
            .field("loader", &self.loader)
            .field("url", &self.url)
            .field("domain", &self.domain())
            .field("user", &self.user())
            .field("name", &self.name())
            .field("has_content", &self.content().is_some())
            .finish()
    }
}

#[derive(Debug, Clone, Copy)]
enum Loader {
    Gist,
}

impl Loader {
    fn from_url(url: &Url) -> Result<Self> {
        ORIGIN_MAP
            .get(&url.origin())
            .ok_or(InvalidUrl::RejectedOrigin.into())
            .copied()
    }

    async fn resolve(self, url: Url) -> Result<ResolvedModule> {
        let resolved = match self {
            Loader::Gist => gist::resolve_gist(&url).await?,
        };
        Ok(ResolvedModule {
            loader: self,
            url,
            resolved: Box::new(resolved),
        })
    }

    async fn ensure_content(self, module: &mut ResolvedModule) -> Result<()> {
        if module.content().is_some() {
            return Ok(());
        }
        match self {
            Loader::Gist => gist::load_content(module).await,
        }
    }
}

/// Internal (used by Loader)
macro_rules! origin_map {
    {$($url:literal => $target:expr),* $(,)?} => {
        {
            use ::std::collections::HashMap;
            let mut origin_map = HashMap::new();
            $(
                origin_map.insert(
                    $url.parse::<Url>().unwrap().origin(),
                    $target
                );
            )*
            origin_map
        }
    };
}

lazy_static! {
    /// Internal (used by Loader)
    static ref ORIGIN_MAP: HashMap<Origin, Loader> = origin_map!{
        "https://gist.github.com/" => Loader::Gist,
        "https://gist.githubusercontent.com/" => Loader::Gist
    };
}

/// Domain defines the domain for the user, in case one day we want to have a
/// more complex namespacing scheme, or code authentication. E.g.
/// `Domain::Github` indicates that the user (in `WebModule`) is a GitHub user.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Domain {
    Github,
    #[allow(dead_code)]
    Builtin,
    #[allow(dead_code)]
    Other(&'static str),
}

pub(crate) async fn resolve(url: Url) -> Result<ResolvedModule> {
    let loader = Loader::from_url(&url)?;
    loader.resolve(url).await
}
