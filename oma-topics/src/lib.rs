use std::path::PathBuf;

use apt_sources_lists::{SourceLine, SourcesLists};
use indexmap::IndexMap;
use oma_console::debug;
use once_cell::sync::Lazy;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

static ATM_STATE: Lazy<PathBuf> = Lazy::new(|| {
    let p = PathBuf::from("/var/lib/atm/state");

    let top = p.parent().unwrap();

    if !top.exists() {
        let _ = std::fs::create_dir_all(top);
    }

    if !p.exists() {
        let _ = std::fs::File::create(&p);
    }

    p
});

static APT_GEN_LIST: Lazy<PathBuf> = Lazy::new(|| {
    let p = PathBuf::from("/var/lib/apt/gen/status.json");

    let top = p.parent().unwrap();

    if !top.exists() {
        let _ = std::fs::create_dir_all(top);
    }

    if !p.exists() {
        let _ = std::fs::File::create(&p);
    }

    p
});

pub type Result<T> = std::result::Result<T, OmaTopicsError>;

#[derive(Debug, thiserror::Error)]
pub enum OmaTopicsError {
    #[error(transparent)]
    SerdeError(#[from] serde_json::error::Error),
    #[error(transparent)]
    IOError(#[from] std::io::Error),
    #[error("Can not find topic: {0}")]
    CanNotFindTopic(String),
    #[error("Failed to enable topic: {0}")]
    FailedToEnableTopic(String),
    #[error(transparent)]
    ReqwestError(#[from] reqwest::Error),
    #[error(transparent)]
    SoutceListError(#[from] apt_sources_lists::SourceError),
}

#[derive(Deserialize)]
struct GenList {
    mirror: IndexMap<String, String>,
}

async fn enabled_mirror() -> Result<Vec<String>> {
    let s = tokio::fs::read_to_string(&*APT_GEN_LIST).await?;
    let gen_list: GenList = serde_json::from_str(&s)?;

    let urls = gen_list
        .mirror
        .values()
        .map(|x| x.to_owned())
        .collect::<Vec<_>>();

    Ok(urls)
}

const TOPICS_JSON: &str = "manifest/topics.json";

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
pub struct Topic {
    pub name: String,
    description: Option<String>,
    date: u64,
    #[serde(skip_serializing)]
    arch: Option<Vec<String>>,
    pub packages: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum TopicsEvent {
    Info(String),
}

#[derive(Debug)]
pub struct TopicManager {
    pub enabled: Vec<Topic>,
    pub all: Vec<Topic>,
}

impl TryFrom<&str> for TopicManager {
    type Error = OmaTopicsError;

    fn try_from(value: &str) -> Result<Self> {
        Ok(Self {
            enabled: serde_json::from_str(value)?,
            all: vec![],
        })
    }
}

impl TopicManager {
    pub async fn new() -> Result<Self> {
        let f = tokio::fs::read_to_string(&*ATM_STATE).await?;

        Ok(Self {
            enabled: serde_json::from_str(&f).unwrap_or(vec![]),
            all: vec![],
        })
    }

    async fn refresh(&mut self, client: &Client) -> Result<Vec<Topic>> {
        let urls = enabled_mirror()
            .await?
            .iter()
            .map(|x| {
                if x.ends_with('/') {
                    format!("{}debs/{TOPICS_JSON}", x)
                } else {
                    format!("{}/debs/{TOPICS_JSON}", x)
                }
            })
            .collect::<Vec<_>>();

        let all = refresh_innter(client, urls).await?;

        self.all = all.clone();

        Ok(all)
    }

    pub async fn add(
        &mut self,
        client: &Client,
        topic: &str,
        dry_run: bool,
        arch: &str,
    ) -> Result<()> {
        debug!("oma will opt_in: {}", topic);

        if dry_run {
            return Ok(());
        }

        let all = if self.all.is_empty() {
            self.refresh(client).await?
        } else {
            self.all.clone()
        };

        debug!("all topic: {all:?}");

        let index = all.iter().find(|x| {
            x.name.to_ascii_lowercase() == topic.to_ascii_lowercase()
                && x.arch
                    .as_ref()
                    .map(|x| x.contains(&arch.to_string()) || x.contains(&"all".to_string()))
                    == Some(true)
        });

        let enabled_names = self.enabled.iter().map(|x| &x.name).collect::<Vec<_>>();

        debug!("Enabled: {enabled_names:?}");

        if let Some(index) = index {
            if !enabled_names.contains(&&index.name) {
                self.enabled.push(index.clone());
            }

            return Ok(());
        }

        debug!("index: {index:?} does not exist");

        Err(OmaTopicsError::CanNotFindTopic(topic.to_owned()))
    }

    pub fn remove(&mut self, topic: &str, dry_run: bool) -> Result<Vec<String>> {
        let index = self
            .enabled
            .iter()
            .position(|x| x.name.to_ascii_lowercase() == topic.to_ascii_lowercase());

        if dry_run {
            debug!("oma will opt_out: {}", topic);
            return Ok(self.enabled[index.unwrap()].packages.clone());
        }

        if let Some(index) = index {
            let d = self.enabled.remove(index);
            let pkgs = d.packages;
            return Ok(pkgs);
        }

        Err(OmaTopicsError::FailedToEnableTopic(topic.to_string()))
    }

    pub async fn write_enabled(&self, dry_run: bool) -> Result<()> {
        if dry_run {
            return Ok(());
        }

        let mut f = tokio::fs::File::create("/etc/apt/sources.list.d/atm.list").await?;
        let mirrors = enabled_mirror().await?;

        // f.write_all(format!("{}\n", fl!("do-not-edit-topic-sources-list")).as_bytes())?;

        for i in &self.enabled {
            f.write_all(format!("# Topic `{}`\n", i.name).as_bytes())
                .await?;
            for j in &mirrors {
                f.write_all(
                    format!(
                        "deb {}debs {} main\n",
                        if j.ends_with('/') {
                            j.to_owned()
                        } else {
                            format!("{j}/")
                        },
                        i.name
                    )
                    .as_bytes(),
                )
                .await?;
            }
        }

        let s = serde_json::to_vec(&self.enabled)?;

        tokio::fs::write(&*ATM_STATE, s).await?;

        Ok(())
    }
}

async fn refresh_innter(client: &Client, urls: Vec<String>) -> Result<Vec<Topic>> {
    let mut json = vec![];

    let mut tasks = vec![];

    for url in urls {
        let v = client.get(url).send();
        tasks.push(v);
    }

    let res = futures::future::try_join_all(tasks).await?;

    let mut tasks = vec![];

    for i in res {
        tasks.push(i.error_for_status()?.json::<Vec<Topic>>());
    }

    let res = futures::future::try_join_all(tasks).await?;

    for i in res {
        let f = i
            .into_iter()
            .filter(|x| !json.contains(x))
            .collect::<Vec<_>>();

        json.extend(f);
    }

    Ok(json)
}

pub async fn list(tm: &mut TopicManager, client: &Client) -> Result<Vec<String>> {
    let all = tm.refresh(client).await?;

    let ft = all
        .iter()
        .map(|x| {
            let mut s = x.name.clone();
            if let Some(d) = &x.description {
                s += &format!(" ({d})");
            }
            s
        })
        .collect::<Vec<_>>();

    Ok(ft)
}

pub async fn scan_closed_topic(client: &Client) -> Result<Vec<String>> {
    let mut atm_sources = vec![];
    let s = SourcesLists::new_from_paths(vec!["/etc/apt/sources.list.d/atm.list"].iter())?;

    for file in s.iter() {
        for i in &file.lines {
            if let SourceLine::Entry(entry) = i {
                atm_sources.push(entry.to_owned());
            }
        }
    }

    let mut tm = TopicManager::new().await?;

    let all = tm.refresh(client).await?;

    let mut res = vec![];

    for i in atm_sources {
        let suite = i.suite;
        let suite_clone = suite.clone();

        if all.iter().all(|x| x.name != suite) {
            rm_topic(&suite, false).await?;
        }

        res.push(suite_clone);
    }

    Ok(res)
}

pub async fn rm_topic(name: &str, dry_run: bool) -> Result<()> {
    if dry_run {
        return Ok(());
    }

    let mut tm = TopicManager::new().await?;
    let mut enabled = tm.enabled;

    let index = enabled
        .iter()
        .position(|x| x.name == name)
        .ok_or_else(|| OmaTopicsError::CanNotFindTopic(name.to_string()))?;

    enabled.remove(index);

    tm.enabled = enabled;

    tm.write_enabled(dry_run).await?;

    Ok(())
}
