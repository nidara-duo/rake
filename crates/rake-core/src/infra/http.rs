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

    /// Fetch a small text resource, such as a release manifest or a checksum file.
    async fn get_text(&self, url: &str) -> Result<String>;

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
    async fn get_text(&self, url: &str) -> Result<String> {
        let resp = self.inner.get(url).send().await?;

        if !resp.status().is_success() {
            return Err(crate::Error::Download(format!(
                "GET {url} returned {}",
                resp.status()
            )));
        }

        resp.text()
            .await
            .map_err(|e| crate::Error::Download(format!("read {url}: {e}")))
    }

    async fn content_length(&self, url: &str) -> Result<Option<u64>> {
        // 1) Fast path: HEAD. Works for simple static file servers, but does NOT work
        //    for many real hosts (see below).
        if let Ok(resp) = self.inner.head(url).send().await
            && resp.status().is_success()
            && let Some(len) = resp.content_length()
            && len > 0
        {
            return Ok(Some(len));
        }

        // 2) Fallback: HEAD either errored, or redirected without a Content-Length, or
        //    the server does not handle HEAD properly at all. A common real case:
        //    GitHub Releases assets redirect to a signed CDN URL whose signature is
        //    computed for the GET method — such a URL answers HEAD with 403 and no
        //    Content-Length. So issue a real GET, read the headers off the first
        //    response that arrives, and immediately drop the Response without reading
        //    the body. reqwest/hyper simply close the connection when a Response is
        //    dropped, so the file is NOT downloaded. Do not be tempted to "read the
        //    body for reliability" — that is precisely what we are avoiding here
        //    (otherwise calculate_download_size becomes a full second download
        //    of every package).
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

        // Checked before the file is created. Without this the body of a 404 — an HTML
        // error page — was written to the cache under the real archive's name and the
        // call still returned success, so the failure only surfaced later as a corrupt
        // download. A dead mirror is routine, not exceptional.
        if !resp.status().is_success() {
            return Err(crate::Error::Download(format!(
                "GET {url} returned {}",
                resp.status()
            )));
        }

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

            // Throttled events are droppable on purpose: each supersedes the last, and
            // the consumer redraws from the newest. The final one is not droppable — see
            // below.
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
            // Sent with `send_async` rather than `try_send`. The bus is bounded (256), so
            // a full channel used to swallow this event — and it is the one carrying
            // `downloaded == total`, which is exactly what the progress display waits for.
            // A dropped final event left the bar spinning forever. Awaiting is safe here:
            // it happens once per file, after the body has been written.
            let _ = tx
                .send_async(Event::DownloadProgress(DownloadProgress {
                    ident,
                    url: url.to_owned(),
                    filename: dest
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    total_bytes: total,
                    downloaded_bytes: downloaded,
                }))
                .await;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flume::bounded;

    fn client() -> ReqwestClient {
        ReqwestClient::new(None, Some("Rake/test")).unwrap()
    }

    fn ident() -> PackageIdent {
        PackageIdent::new("main", "demo")
    }

    /// Serve one canned response and hand back its URL. Loopback only, no network.
    ///
    /// The request head is read before answering. Skipping that leaves the client's
    /// request sitting unread in the socket buffer, and closing on it makes the kernel
    /// send an RST — which the client reports as "connection aborted" rather than as the
    /// status this helper was written to produce.
    async fn serve_once(response: &'static str) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let body = response.to_owned();
        let handle = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 4096];
                let _ = socket.read(&mut buf).await;
                let _ = socket.write_all(body.as_bytes()).await;
                let _ = socket.flush().await;
                let _ = socket.shutdown().await;
            }
        });
        (format!("http://{addr}/file.zip"), handle)
    }

    const OK_BODY: &str = "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello";

    const NOT_FOUND_BODY: &str =
        "HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\nConnection: close\r\n\r\nnot found";

    /// A dead mirror must not become a cached file. Before the status check this wrote
    /// the error page into the cache under the real archive's name and returned `Ok`, so
    /// the corruption surfaced much later as a broken archive.
    #[tokio::test]
    async fn a_404_is_an_error_and_leaves_no_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("demo.zip");
        let (url, server) = serve_once(NOT_FOUND_BODY).await;

        let result = client().download(&url, &dest, ident(), None).await;
        server.await.unwrap();

        let err = result.unwrap_err().to_string();
        assert!(err.contains("404"), "got: {err}");
        assert!(
            !dest.exists(),
            "no file may be created for a failed response"
        );
    }

    #[tokio::test]
    async fn a_200_writes_the_body() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("demo.zip");
        let (url, server) = serve_once(OK_BODY).await;

        client().download(&url, &dest, ident(), None).await.unwrap();
        server.await.unwrap();

        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "hello");
    }

    /// The final progress event is what tells the display the download finished. The bus is
    /// bounded, and `try_send` used to drop this one when it was full — leaving the bar
    /// spinning forever on a download that had actually succeeded.
    ///
    /// The consumer runs concurrently here, which is the point: the sender now *waits* for
    /// room, so a test that fills the channel and only drains it after `download` returns
    /// deadlocks. That is the intended behaviour, so the test has to consume rather than
    /// assert on a stalled send.
    #[tokio::test]
    async fn the_final_progress_event_survives_backpressure() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("demo.zip");
        let (url, server) = serve_once(OK_BODY).await;

        // Capacity 1, pre-filled: a `try_send` at this moment would be dropped.
        let (tx, rx) = bounded::<Event>(1);
        tx.try_send(Event::DownloadDone).unwrap();

        let consumer = tokio::spawn(async move {
            let mut seen = Vec::new();
            // Two events are expected: the placeholder and the awaited final one.
            while seen.len() < 2 {
                match rx.recv_async().await {
                    Ok(ev) => seen.push(ev),
                    Err(_) => break,
                }
            }
            seen
        });

        client()
            .download(&url, &dest, ident(), Some(tx))
            .await
            .unwrap();
        server.await.unwrap();
        let seen = consumer.await.unwrap();

        let final_event = seen
            .iter()
            .rev()
            .find_map(|e| match e {
                Event::DownloadProgress(p) => Some(p),
                _ => None,
            })
            .expect("the final progress event must be delivered, not dropped");
        assert_eq!(final_event.downloaded_bytes, final_event.total_bytes);
        assert_eq!(final_event.downloaded_bytes, 5);
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "hello");
    }

    /// A no-op channel and a missing one are both fine: progress is optional.
    #[tokio::test]
    async fn downloading_without_a_progress_channel_works() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("demo.zip");
        let (url, server) = serve_once(OK_BODY).await;

        let (tx, rx) = bounded::<Event>(4);
        drop(rx);
        client()
            .download(&url, &dest, ident(), Some(tx))
            .await
            .unwrap();
        server.await.unwrap();

        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "hello");
    }

    /// `get_text` already refused non-success; this pins that it still does, since the
    /// two paths drifting apart is what let `download` miss the check.
    #[tokio::test]
    async fn get_text_refuses_a_404() {
        let (url, server) = serve_once(NOT_FOUND_BODY).await;
        let result = client().get_text(&url).await;
        server.await.unwrap();
        assert!(result.unwrap_err().to_string().contains("404"));
    }

    /// A server that does not implement HEAD, or answers without a Content-Length, must
    /// not be reported as "size unknown" — the GET fallback exists for exactly that.
    ///
    /// There is deliberately no companion test for the HEAD fast path. Reproducing a correct
    /// HEAD response needs a server that omits the body, and getting that wrong with a
    /// hand-rolled one tests reqwest's framing rather than anything in this file.
    #[tokio::test]
    async fn content_length_falls_back_when_head_is_refused() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            // First connection answers HEAD with a 405, the second with a real GET.
            for (index, response) in [
                "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                OK_BODY,
            ]
            .into_iter()
            .enumerate()
            {
                if let Ok((mut socket, _)) = listener.accept().await {
                    // Drain the request head so the client is not still writing.
                    let mut buf = [0u8; 1024];
                    let _ = socket.read(&mut buf).await;
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.flush().await;
                    let _ = index;
                }
            }
        });

        let len = client()
            .content_length(&format!("http://{addr}/file.zip"))
            .await
            .unwrap();
        handle.await.unwrap();

        assert_eq!(
            len,
            Some(5),
            "the GET fallback must recover the size when HEAD is refused"
        );
    }
}
