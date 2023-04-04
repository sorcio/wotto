use std::path::{Path, PathBuf};

use tracing::trace;
use url::Url;

use super::{ResolvedModule, ResolverResult};
use crate::service::Result;
use crate::webload::{InvalidUrl, WebError};

const BUILTIN_DIR: &str = "builtin";

pub(super) async fn resolve_module(url: &Url) -> Result<Box<dyn ResolverResult + Send + Sync>> {
    debug_assert_eq!(url.scheme(), "builtin");

    let path = Path::new(url.path());
    if !path
        .components()
        .all(|c| matches!(c, std::path::Component::Normal(_)))
    {
        return Err(InvalidUrl::InvalidPath.into());
    }
    debug_assert!(path.is_relative());
    if path.extension().is_some() {
        return Err(InvalidUrl::InvalidPath.into());
    }
    let extensionless_path = Path::new(BUILTIN_DIR).join(path);
    let extensions = ["wasm", "wat"];

    // Note: we check if the file exists here, but we are not reading the
    // content yet and the file might not exist later when we load it.
    // Resolving does not guarantee that load is successful. Also note that
    // restricting to regular files (`is_file() == true`) leaves out some cases
    // like named pipes, which might be okay for our purposes.
    let mut good_path = None;
    let mut errors: Vec<(PathBuf, WebError)> = Vec::new();
    for ext in extensions {
        let full_path = extensionless_path.with_extension(ext);
        match tokio::fs::metadata(&full_path).await {
            Ok(metadata) => {
                trace!(path = %full_path.display(), ?metadata, "found builtin candidate");
                if metadata.is_file() {
                    good_path = Some(full_path);
                    break;
                } else {
                    errors.push((full_path, WebError::NotWasm))
                }
            }
            Err(error) => errors.push((full_path, WebError::IoError(error))),
        }
    }
    if let Some(full_path) = good_path {
        Ok(Box::new(BuiltinModule::new(full_path)))
    } else {
        Err(WebError::Multiple(errors).into())
    }
}

pub(super) async fn load_content(module: &mut ResolvedModule) -> Result<()> {
    if module.content().is_some() {
        return Ok(());
    }
    let resolver_result = module.downcast::<BuiltinModule>();
    resolver_result.read().await.map_err(WebError::IoError)?;
    Ok(())
}

struct BuiltinModule {
    full_path: PathBuf,
    content: Option<Vec<u8>>,
}

impl BuiltinModule {
    fn new(full_path: PathBuf) -> Self {
        Self {
            full_path,
            content: None,
        }
    }

    async fn read(&mut self) -> std::io::Result<()> {
        trace!(path = %self.full_path.display(), "attempting to load builtin");
        let content = tokio::fs::read(&self.full_path).await?;
        self.content = Some(content);
        Ok(())
    }
}

impl ResolverResult for BuiltinModule {
    fn domain(&self) -> super::Domain {
        super::Domain::Builtin
    }

    fn user(&self) -> &str {
        ""
    }

    fn name(&self) -> &str {
        self.full_path
            .file_stem()
            .expect("path should already be validated")
            .to_str()
            .expect("path should already be valid utf-8")
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
