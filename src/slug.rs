use std::{fmt, ops::Deref};

use arrayvec::ArrayString;
use rand::prelude::*;
use serde::Deserialize;

pub const LENGTH: usize = 10;

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct Slug(ArrayString<LENGTH>);

impl Slug {
    pub fn from_rng(rng: &mut impl RngCore) -> Self {
        let dist = rand::distributions::Alphanumeric
            .sample_iter(rng)
            .take(LENGTH);
        let mut this = ArrayString::new();
        for ch in dist {
            this.push(ch as char);
        }
        Self(this)
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        <ArrayString<LENGTH>>::as_str(&self.0)
    }
}

impl<'a> TryFrom<&'a str> for Slug {
    type Error = arrayvec::CapacityError<&'a str>;

    #[inline]
    fn try_from(str: &'a str) -> Result<Self, Self::Error> {
        ArrayString::from(str).map(Self)
    }
}

impl Deref for Slug {
    type Target = str;

    #[inline]
    fn deref(&self) -> &Self::Target {
        <ArrayString<LENGTH>>::deref(&self.0)
    }
}

impl fmt::Display for Slug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<'de> Deserialize<'de> for Slug {
    fn deserialize<D>(de: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        ArrayString::deserialize(de).map(Slug)
    }
}

