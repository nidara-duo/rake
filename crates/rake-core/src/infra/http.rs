use std::path::Path;
use std::time::Instant;

use async_trait::async_trait;
use flume::Sender;
use futures_util::StreamExt;
use rake_domain::package::PackageIdent;
use tokio::io::AsyncWriteExt;

use crate::Result;
use crate::event::{DownloadProgress, Event};

#[async_trait]
pub trait HttpClient: Send + Sync {
    async fn content_length(&self, url: &str) -> Result<Option<u64>>;

    async fn download(
        &self,
        url: &str,
        dest: &Path,
        ident: PackageIdent,
        progress_tx: Option<Sender<Event>>,
    ) -> Result<()>;
}

#[derive(Clone)]
pub struct ReqwestClient {
    inner: reqwest::Client,
}

impl ReqwestClient {
    pub fn new(proxy: Option<&str>, user_agent: Option<&str>) -> Result<Self> {
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(10))
            .tcp_keepalive(std::time::Duration::from_secs(30));

        if let Some(ua) = user_agent {
            builder = builder.user_agent(ua);
        }

        if let Some(proxy_url) = proxy {
            let p = reqwest::Proxy::all(proxy_url)
                .map_err(|e| crate::Error::HttpConfig(e.to_string()))?;
            builder = builder.proxy(p);
        }

        let inner = builder
            .build()
            .map_err(|e| crate::Error::HttpConfig(e.to_string()))?;

        Ok(Self { inner })
    }
}

#[async_trait]
impl HttpClient for ReqwestClient {
    async fn content_length(&self, url: &str) -> Result<Option<u64>> {
        // 1) Быстрый путь: HEAD. Работает для простых статических файловых
        //    серверов, но НЕ работает для многих реальных хостингов (см. ниже).
        if let Ok(resp) = self.inner.head(url).send().await
            && resp.status().is_success()
            && let Some(len) = resp.content_length()
            && len > 0
        {
            return Ok(Some(len));
        }

        // 2) Фолбэк: HEAD либо вернул ошибку/редирект-без-Content-Length, либо
        //    сервер вообще не умеет в HEAD как надо. Частый практический случай:
        //    GitHub Releases assets редиректят на подписанный CDN URL, чья
        //    подпись считается для метода GET — на HEAD такой URL отвечает 403
        //    и без Content-Length. Поэтому делаем настоящий GET, читаем заголовок
        //    из первого пришедшего ответа и СРАЗУ дропаем Response, не читая тело.
        //    reqwest/hyper при дропе Response просто закрывают соединение — файл
        //    целиком НЕ скачивается. Не пытайся "прочитать тело для надёжности" —
        //    это именно то, чего мы хотим избежать (иначе calculate_total_download_size
        //    станет полноценным двойным скачиванием всех пакетов).
        match self.inner.get(url).send().await {
            Ok(resp) if resp.status().is_success() => {
                Ok(resp.content_length().filter(|&len| len > 0))
            }
            _ => Ok(None),
        }
    }

    async fn download(
        &self,
        url: &str,
        dest: &Path,
        ident: PackageIdent,
        progress_tx: Option<Sender<Event>>,
    ) -> Result<()> {
        let resp = self.inner.get(url).send().await?;
        let total = resp.content_length().unwrap_or(0);
        let mut stream = resp.bytes_stream();

        let mut file = tokio::fs::File::create(dest).await?;

        let mut downloaded = 0u64;
        let mut last_report = Instant::now();
        let throttle_dur = std::time::Duration::from_millis(100);

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            file.write_all(&chunk).await?;
            downloaded += chunk.len() as u64;

            if let Some(ref tx) = progress_tx
                && last_report.elapsed() >= throttle_dur
            {
                let _ = tx.try_send(Event::DownloadProgress(DownloadProgress {
                    ident: ident.clone(),
                    url: url.to_owned(),
                    filename: dest
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    total_bytes: total,
                    downloaded_bytes: downloaded,
                }));
                last_report = Instant::now();
            }
        }

        file.flush().await?;

        if let Some(tx) = progress_tx {
            let _ = tx.try_send(Event::DownloadProgress(DownloadProgress {
                ident,
                url: url.to_owned(),
                filename: dest
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                total_bytes: total,
                downloaded_bytes: downloaded,
            }));
        }

        Ok(())
    }
}
