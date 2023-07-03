use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use apt_sources_lists::{SourceEntry, SourceLine, SourcesLists};
use futures::StreamExt;
use oma_console::indicatif::MultiProgress;
use oma_fetch::{DownloadEntry, DownloadError, DownloadResult, DownloadSourceType, OmaFetcher};
use once_cell::sync::Lazy;
use reqwest::ClientBuilder;
use serde::Deserialize;
use url::Url;

use crate::{
    decompress::OmaDecompresser,
    inrelease::{DistFileType, InReleaseParser, InReleaseParserError},
    util::database_filename,
};

#[derive(Deserialize)]
struct MirrorMapItem {
    url: String,
}

static MIRROR: Lazy<PathBuf> =
    Lazy::new(|| PathBuf::from("/usr/share/distro-repository-data/mirrors.yml"));

pub static APT_LIST_DISTS: Lazy<PathBuf> = Lazy::new(|| {
    let p = PathBuf::from("/var/lib/apt/lists");

    if !p.is_dir() {
        let _ = std::fs::create_dir_all(&p);
    }

    p
});

#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    #[error("Invalid URL: {0}")]
    InvaildUrl(String),
    #[error("Can not parse distro repo data: {0}")]
    ParseDistroRepoDataErrpr(String),
    #[error("Can not read file {}: {}", .path, .error)]
    ReadFileError { path: String, error: std::io::Error },
    #[error("Scan sources.list failed: {0}")]
    ScanSourceError(String),
    #[error("Unsupport Protocol: {0}")]
    UnsupportedProtocol(String),
    #[error(transparent)]
    FetcherError(#[from] oma_fetch::DownloadError),
    #[error(transparent)]
    ReqwestError(#[from] reqwest::Error),
    #[error(transparent)]
    TopicsError(#[from] oma_topics::OmaTopicsError),
    #[error("Failed to download InRelease from URL {0}: Remote file not found (HTTP 404).")]
    NoInReleaseFile(String),
    #[error(transparent)]
    InReleaseParserError(#[from] InReleaseParserError),
    #[error(transparent)]
    DpkgArchError(#[from] oma_utils::DpkgArchError),
    #[error(transparent)]
    JoinError(#[from] tokio::task::JoinError),
    #[error(transparent)]
    DecompressError(#[from] crate::decompress::DecompressError),
}

type Result<T> = std::result::Result<T, RefreshError>;

pub(crate) async fn get_url_short_and_branch(url: &str) -> Result<String> {
    let url = Url::parse(url).map_err(|_| RefreshError::InvaildUrl(url.to_string()))?;
    let host = url
        .host_str()
        .ok_or_else(|| RefreshError::InvaildUrl(url.to_string()))?;

    let schema = url.scheme();
    let branch = url
        .path()
        .split('/')
        .nth_back(1)
        .ok_or_else(|| RefreshError::InvaildUrl(url.to_string()))?;
    let url = format!("{schema}://{host}/");

    // MIRROR 文件为 AOSC 独有，为兼容其他 .deb 系统，这里不直接返回错误
    if let Ok(mirror_map_f) = tokio::fs::read(&*MIRROR).await {
        let mirror_map: HashMap<String, MirrorMapItem> = serde_yaml::from_slice(&mirror_map_f)
            .map_err(|_| RefreshError::ParseDistroRepoDataErrpr(MIRROR.display().to_string()))?;

        for (k, v) in mirror_map.iter() {
            let mirror_url =
                Url::parse(&v.url).map_err(|_| RefreshError::InvaildUrl(v.url.to_string()))?;
            let mirror_url_host = mirror_url
                .host_str()
                .ok_or_else(|| RefreshError::InvaildUrl(v.url.to_string()))?;

            let schema = mirror_url.scheme();
            let mirror_url = format!("{schema}://{mirror_url_host}/");

            if mirror_url == url {
                return Ok(format!("{k}:{branch}"));
            }
        }
    }

    Ok(format!("{host}:{branch}"))
}

/// Get /etc/apt/sources.list and /etc/apt/sources.list.d
pub fn get_sources() -> Result<Vec<SourceEntry>> {
    let mut res = Vec::new();
    let list = SourcesLists::scan().map_err(|e| RefreshError::ScanSourceError(e.to_string()))?;

    for file in list.iter() {
        for i in &file.lines {
            if let SourceLine::Entry(entry) = i {
                res.push(entry.to_owned());
            }
        }
    }

    // AOSC OS/Retro 以后也许会支持使用光盘源安装软件包，但目前因为没有实例，所以无法测试
    // 因此 Omakase 暂不支持 cdrom:// 源的安装
    // let cdrom = res
    //     .iter()
    //     .filter(|x| x.url().starts_with("cdrom://"))
    //     .collect::<Vec<_>>();

    // for i in &cdrom {
    //     error!("{}", fl!("unsupport-cdrom", url = i.url()));
    // }

    // if !cdrom.is_empty() {
    //     bail!(fl!("unsupport-some-mirror"));
    // }

    Ok(res)
}

#[derive(Debug, Clone)]
pub struct OmaSourceEntry {
    from: OmaSourceEntryFrom,
    components: Vec<String>,
    url: String,
    _suite: String,
    inrelease_path: String,
    dist_path: String,
    is_flat: bool,
    signed_by: Option<String>,
}

#[derive(PartialEq, Eq, Debug, Clone)]
enum OmaSourceEntryFrom {
    Http,
    Local,
}

pub enum Event {
    Info(String),
}

impl OmaSourceEntry {
    fn new(v: &SourceEntry) -> Result<Self> {
        let from = if v.url().starts_with("http://") || v.url().starts_with("https://") {
            OmaSourceEntryFrom::Http
        } else if v.url().starts_with("file://") {
            OmaSourceEntryFrom::Local
        } else {
            return Err(RefreshError::UnsupportedProtocol(format!("{v:?}")));
        };

        let components = v.components.clone();
        let url = v.url.clone();
        let suite = v.suite.clone();
        let (dist_path, is_flat) = if components.is_empty() && suite == "/" {
            (v.url().to_string(), true)
        } else {
            (v.dist_path(), false)
        };

        let inrelease_path = if is_flat {
            // flat Repo
            format!("{url}/Release")
        } else if !components.is_empty() {
            // Normal Repo
            format!("{dist_path}/InRelease")
        } else {
            return Err(RefreshError::UnsupportedProtocol(format!("{v:?}")));
        };

        let options = v.options.as_deref().unwrap_or_default();

        let options = options.split_whitespace().collect::<Vec<_>>();

        let signed_by = options
            .iter()
            .find(|x| x.strip_prefix("signed-by=").is_some())
            .map(|x| x.strip_prefix("signed-by=").unwrap().to_string());

        Ok(Self {
            from,
            components,
            url,
            _suite: suite,
            is_flat,
            inrelease_path,
            dist_path,
            signed_by,
        })
    }
}

fn hr_sources(sources: &[SourceEntry]) -> Result<Vec<OmaSourceEntry>> {
    let mut res = vec![];
    for i in sources {
        res.push(OmaSourceEntry::new(i)?);
    }

    Ok(res)
}

pub struct OmaRefresh {
    sources: Vec<OmaSourceEntry>,
    limit: Option<usize>,
    arch: String,
    download_dir: PathBuf,
    bar: bool,
}

impl OmaRefresh {
    pub fn scan(limit: Option<usize>) -> Result<Self> {
        let sources = get_sources()?;
        let sources = hr_sources(&sources)?;
        let arch = oma_utils::dpkg_arch()?;

        let download_dir = APT_LIST_DISTS.clone();

        Ok(Self {
            sources,
            limit,
            arch,
            download_dir,
            bar: true,
        })
    }

    pub fn download_dir(&mut self, download_dir: &Path) -> &mut Self {
        self.download_dir = download_dir.to_path_buf();

        self
    }

    pub fn bar(&mut self, bar: bool) -> &mut Self {
        self.bar = bar;

        self
    }

    pub async fn start(self) -> Result<()> {
        update_db(
            self.sources,
            self.limit,
            self.arch,
            self.download_dir,
            self.bar,
        )
        .await
    }
}

// Update database
async fn update_db(
    sourceslist: Vec<OmaSourceEntry>,
    limit: Option<usize>,
    arch: String,
    download_dir: PathBuf,
    bar: bool,
) -> Result<()> {
    let mut tasks = vec![];

    for source_entry in &sourceslist {
        let msg = get_url_short_and_branch(&source_entry.inrelease_path).await?;
        match source_entry.from {
            OmaSourceEntryFrom::Http => {
                let task = DownloadEntry::new(
                    source_entry.inrelease_path.clone(),
                    database_filename(&source_entry.inrelease_path),
                    download_dir.clone(),
                    None,
                    false,
                    DownloadSourceType::Http,
                    Some(format!("{msg} InRelease")),
                );

                tracing::debug!("oma will fetch {} InRelease", source_entry.url);
                tasks.push(task);
            }
            OmaSourceEntryFrom::Local => {
                let task = DownloadEntry::new(
                    source_entry.inrelease_path.clone(),
                    database_filename(&source_entry.inrelease_path),
                    download_dir.clone(),
                    None,
                    false,
                    DownloadSourceType::Http,
                    Some(msg),
                );

                tracing::debug!("oma will fetch {} InRelease", source_entry.url);
                tasks.push(task);
            }
        }
    }

    let res = OmaFetcher::new(None, bar, None, tasks, limit)?
        .start_download()
        .await;

    let mut all_inrelease = vec![];

    for inrelease in res {
        if cfg!(feature = "aosc") {
            match inrelease {
                Ok(i) => {
                    tracing::debug!("{} fetched", &i.filename);
                    all_inrelease.push(i)
                }
                Err(e) => match e {
                    DownloadError::NotFound(url) => {
                        let client = ClientBuilder::new().user_agent("oma").build()?;
                        let (tx, rx) = std::sync::mpsc::channel();

                        let event_worker = tokio::task::spawn_blocking(move || {
                            while let Ok(v) = rx.recv() {
                                tracing::info!("{:?}", v);
                            }
                        });

                        let removed_suites =
                            oma_topics::scan_closed_topic(&client, Some(tx)).await?;

                        event_worker.await?;

                        tracing::debug!("Removed topics: {removed_suites:?}");

                        let suite = url
                            .split('/')
                            .nth_back(1)
                            .ok_or_else(|| RefreshError::InvaildUrl(url.to_string()))?
                            .to_string();

                        if !removed_suites.contains(&suite) {
                            return Err(RefreshError::NoInReleaseFile(url.to_string()));
                        }
                    }
                    _ => return Err(e.into()),
                },
            }
        } else {
            let i = inrelease?;
            tracing::debug!("{} fetched", &i.filename);
            all_inrelease.push(i);
        }
    }

    for inrelease_summary in all_inrelease {
        let ose = sourceslist.get(inrelease_summary.count).unwrap();

        tracing::debug!("Getted Oma source entry: {:?}", ose);

        let download_dir = download_dir.clone();
        let inrelease_path = download_dir.join(&inrelease_summary.filename);

        let inrelease =
            InReleaseParser::new(&inrelease_path, ose.signed_by.as_deref(), &ose.url, &arch)?;

        let checksums = inrelease
            .checksums
            .iter()
            .filter(|x| {
                ose.components
                    .contains(&x.name.split('/').next().unwrap_or(&x.name).to_owned())
            })
            .map(|x| x.to_owned())
            .collect::<Vec<_>>();

        let mut total = 0;
        let handle = if ose.is_flat {
            tracing::debug!("{} is flat repo", ose.url);
            // Flat repo
            let mut handle = vec![];
            for i in &inrelease.checksums {
                if i.file_type == DistFileType::PackageList {
                    tracing::debug!("oma will download package list: {}", i.name);
                    handle.push(i);
                    total += i.size;
                }
            }

            handle
        } else {
            let mut handle = vec![];
            for i in &checksums {
                match i.file_type {
                    DistFileType::BinaryContents => {
                        tracing::debug!("oma will download Binary Contents: {}", i.name);
                        handle.push(i);
                        total += i.size;
                    }
                    DistFileType::Contents | DistFileType::PackageList => {
                        if arch == "mips64r6el" {
                            tracing::debug!("oma will download Package List/Contetns: {}", i.name);
                            handle.push(i);
                            total += i.size;
                        }
                    }
                    DistFileType::CompressContents | DistFileType::CompressPackageList => {
                        if arch != "mips64r6el" {
                            tracing::debug!(
                                "oma will download compress Package List/compress Contetns: {}",
                                i.name
                            );

                            handle.push(i);
                            total += i.size;
                        }
                    }
                    _ => continue,
                }
            }

            handle
        };

        let mut tasks = vec![];

        for c in handle {
            let mut p_not_compress = Path::new(&c.name).to_path_buf();
            p_not_compress.set_extension("");
            let not_compress_filename_before = p_not_compress.to_string_lossy().to_string();

            let source_index = sourceslist.get(inrelease_summary.count).unwrap();
            let not_compress_filename = database_filename(&format!(
                "{}/{}",
                source_index.dist_path, not_compress_filename_before
            ));

            let typ = match c.file_type {
                DistFileType::CompressContents => "Contents",
                DistFileType::CompressPackageList | DistFileType::PackageList => "Package List",
                DistFileType::BinaryContents => "BinContents",
                _ => unreachable!(),
            };

            let msg = get_url_short_and_branch(&source_index.inrelease_path).await?;

            match source_index.from {
                OmaSourceEntryFrom::Http => {
                    let dist_url = if !ose.is_flat {
                        source_index.dist_path.clone()
                    } else {
                        format!("{}/{}", source_index.dist_path, not_compress_filename)
                    };

                    let file_path = format!("{}/{}", dist_url, c.name);

                    let task = DownloadEntry::new(
                        file_path.clone(),
                        database_filename(&file_path),
                        download_dir.clone(),
                        Some(c.checksum.clone()),
                        false,
                        DownloadSourceType::Http,
                        Some(format!("{msg} {typ}")),
                    );

                    tracing::debug!("oma will download http source database: {file_path}");

                    tasks.push(task);
                }
                OmaSourceEntryFrom::Local => {
                    let p = if !ose.is_flat {
                        source_index.dist_path.clone()
                    } else {
                        format!("{}/{}", source_index.dist_path, not_compress_filename)
                    };

                    tracing::debug!("oma will download local source database: {p} {}", c.name);

                    let task = DownloadEntry::new(
                        p.clone(),
                        database_filename(&p),
                        download_dir.clone(),
                        Some(c.checksum.clone()),
                        false,
                        DownloadSourceType::Local,
                        Some(format!("{msg} {typ}")),
                    );

                    tasks.push(task);
                }
            }
        }

        let res = OmaFetcher::new(None, bar, Some(total), tasks, limit)?
            .start_download()
            .await;

        let res = res.into_iter().collect::<DownloadResult<Vec<_>>>()?;

        // 解压
        let mut tasks = vec![];
        let len = res.len();
        let mb = MultiProgress::new();
        for (i, c) in res.into_iter().enumerate() {
            let download_dir = download_dir.clone();
            let decompresser = OmaDecompresser::new(download_dir.join(c.filename.clone()));
            let mbc = mb.clone();
            
            let f = tokio::task::spawn_blocking(move || {
                decompresser.decompress(bar, i, len, &download_dir, c.context.as_ref().unwrap(), mbc)
            });
            tasks.push(f);
        }

        let stream = futures::stream::iter(tasks).buffer_unordered(limit.unwrap_or(4));
        let res = stream.collect::<Vec<_>>().await;
        for i in res {
            i??;
        }
    }

    Ok(())
}