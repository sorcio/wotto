use std::borrow::Borrow;
use std::fmt::Display;
use std::path::{Path, PathBuf};

use crate::service::Result;
use crate::webload::{Domain, ResolvedModule};
use crate::Error;

#[derive(Debug)]
pub(crate) struct CanonicalName<'a>(&'a str);

impl<'a> TryFrom<&'a Path> for CanonicalName<'a> {
    type Error = Error;

    fn try_from(value: &'a Path) -> std::result::Result<Self, Self::Error> {
        Ok(CanonicalName(
            value
                .file_stem()
                .ok_or(Error::InvalidModuleName)?
                .to_str()
                .ok_or(Error::InvalidModuleName)?,
        ))
    }
}

impl<'a> TryFrom<&'a str> for CanonicalName<'a> {
    type Error = <Self as TryFrom<&'a Path>>::Error;

    fn try_from(value: &'a str) -> std::result::Result<Self, Self::Error> {
        Self::try_from(Path::new(value))
    }
}

impl<'a> TryFrom<&'a PathBuf> for CanonicalName<'a> {
    type Error = <Self as TryFrom<&'a Path>>::Error;

    fn try_from(value: &'a PathBuf) -> std::result::Result<Self, Self::Error> {
        Self::try_from(Path::new(value))
    }
}

impl Display for CanonicalName<'_> {
    #[inline]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Identifier for a webmodule
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
#[repr(transparent)]
pub(crate) struct FullyQualifiedNameBuf {
    fqn: String,
}

impl FullyQualifiedNameBuf {
    pub(crate) fn new(domain: Domain, canonical_name: CanonicalName, user: &str) -> Self {
        let fqn = match domain {
            Domain::Github => format!("{user}/{canonical_name}"),
            Domain::Builtin => format!("{canonical_name}"),
            Domain::Other(domain) => format!("{user}@{domain}/{canonical_name}"),
        };
        Self { fqn }
    }

    pub(crate) fn for_module(module: &ResolvedModule) -> Result<Self> {
        Ok(Self::new(
            module.domain(),
            module.name().try_into()?,
            module.user(),
        ))
    }
}

impl core::ops::Deref for FullyQualifiedNameBuf {
    type Target = FullyQualifiedName;

    #[inline]
    fn deref(&self) -> &Self::Target {
        unsafe { FullyQualifiedName::from_str_unchecked(&self.fqn) }
    }
}

impl Display for FullyQualifiedNameBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.fqn)
    }
}

impl Borrow<FullyQualifiedName> for FullyQualifiedNameBuf {
    fn borrow(&self) -> &FullyQualifiedName {
        self
    }
}

#[derive(Debug, Hash, PartialEq, Eq)]
#[repr(transparent)]
pub(crate) struct FullyQualifiedName {
    fqn: str,
}

impl FullyQualifiedName {
    pub(crate) fn from_str(s: &str) -> Result<&Self> {
        let _domain = if let Some((ns, _name)) = s.rsplit_once('/') {
            if let Some((_, _domain_name)) = ns.split_once('@') {
                // String matches format for "Other" domain but currently none exists
                return Err(Error::InvalidModuleName);
            } else if !ns.is_empty() {
                // By default, all users are Github users
                Domain::Github
            } else {
                return Err(Error::InvalidModuleName);
            }
        } else {
            Domain::Builtin
        };
        Ok(unsafe { Self::from_str_unchecked(s) })
    }

    unsafe fn from_str_unchecked(s: &str) -> &Self {
        // Safety: FullyQualifiedNameBorrow is repr(transparent) with str
        unsafe { std::mem::transmute(s) }
    }
}

impl ToOwned for FullyQualifiedName {
    type Owned = FullyQualifiedNameBuf;

    fn to_owned(&self) -> Self::Owned {
        FullyQualifiedNameBuf {
            fqn: self.fqn.to_owned(),
        }
    }
}

impl Display for FullyQualifiedName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.fqn)
    }
}

impl PartialEq<str> for FullyQualifiedName {
    #[inline]
    fn eq(&self, other: &str) -> bool {
        PartialEq::eq(&self.fqn, other)
    }
}

impl PartialEq<String> for FullyQualifiedName {
    #[inline]
    fn eq(&self, other: &String) -> bool {
        PartialEq::eq(&self.fqn, other)
    }
}
