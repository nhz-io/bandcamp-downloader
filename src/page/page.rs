use crate::page::traits;
use reqwest::Url;
use serde::de::DeserializeOwned;
use std::marker::PhantomData;

pub struct Page<T>
where
    T: DeserializeOwned,
{
    _page_data: PhantomData<T>,
    url: Url,
}

impl<T> Page<T>
where
    T: DeserializeOwned,
{
    pub fn new(url: impl Into<Url>) -> Self {
        Self {
            _page_data: PhantomData,
            url: url.into(),
        }
    }
}

impl<T> traits::Page<T> for Page<T>
where
    T: DeserializeOwned,
{
    fn get_url(&self) -> &Url {
        &self.url
    }
}