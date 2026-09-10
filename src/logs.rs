//! Bounded collection of compose logs useful for investigating OSTree failures.

use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use reqwest::{StatusCode, blocking::Client, redirect::Policy};
use scraper::{Html, Selector};
use tempfile::Builder;
use url::Url;

const RAW_HIDE_ROOT: &str = "https://kojipkgs.fedoraproject.org/compose/rawhide/";
const MAX_INDEX_BYTES: u64 = 1024 * 1024;
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_FILES: usize = 256;
const MAX_ARCHES: usize = 16;
const MAX_VARIANTS: usize = 128;
const MAX_JOBS: usize = 256;
const MAX_REQUESTS: usize = 512;
const MAX_RESPONSE_BYTES: u64 = 96 * 1024 * 1024;
const MAX_ELAPSED: Duration = Duration::from_secs(120);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const GLOBAL_FILES: &[&str] = &[
    "pungi.global.log",
    "config-dump.global.log",
    "deliverables.json",
];
const JOB_FILES: &[&str] = &[
    "create-ostree-repo.log",
    "runroot.log",
    "image-builder.log",
    "create-image.log",
    "lorax.log",
    "koji.log",
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Args {
    pub(crate) compose: Option<String>,
}

#[derive(Debug)]
pub(crate) struct Failure {
    pub(crate) path: PathBuf,
    pub(crate) message: String,
}

struct Collector {
    client: Client,
    base: Url,
    output: PathBuf,
    files: usize,
    bytes: u64,
    saved: BTreeSet<PathBuf>,
    budget: Budget,
}

struct Budget {
    requests: usize,
    response_bytes: u64,
    max_requests: usize,
    max_response_bytes: u64,
    deadline: Instant,
}

impl Budget {
    fn production() -> Self {
        Self::new(MAX_REQUESTS, MAX_RESPONSE_BYTES, MAX_ELAPSED)
    }

    fn new(max_requests: usize, max_response_bytes: u64, elapsed: Duration) -> Self {
        Self {
            requests: 0,
            response_bytes: 0,
            max_requests,
            max_response_bytes,
            deadline: Instant::now() + elapsed,
        }
    }

    fn before_request(&mut self) -> Result<(), String> {
        self.check_deadline()?;
        if self.requests >= self.max_requests {
            return Err(format!("request limit ({}) reached", self.max_requests));
        }
        self.requests += 1;
        Ok(())
    }

    fn check_deadline(&self) -> Result<(), String> {
        if Instant::now() >= self.deadline {
            Err("overall collection deadline reached".into())
        } else {
            Ok(())
        }
    }

    fn request_timeout(&self) -> Result<Duration, String> {
        self.check_deadline()?;
        Ok(REQUEST_TIMEOUT.min(self.deadline.saturating_duration_since(Instant::now())))
    }

    fn remaining(&self) -> Result<u64, String> {
        self.max_response_bytes
            .checked_sub(self.response_bytes)
            .ok_or_else(|| "aggregate response byte limit reached".into())
    }

    fn consume(&mut self, bytes: u64) -> Result<(), String> {
        self.response_bytes = self.response_bytes.saturating_add(bytes);
        if self.response_bytes > self.max_response_bytes {
            return Err(format!(
                "aggregate response byte limit ({}) exceeded",
                self.max_response_bytes
            ));
        }
        Ok(())
    }
}

type CollectError = (Option<String>, String, usize, u64);

pub(crate) fn run(args: Args) -> Result<(), Failure> {
    run_from_base(args, RAW_HIDE_ROOT).map(|(compose, path, files, bytes)| {
        tracing::info!(
            audit_event = "logs_finished",
            compose_id = %compose,
            path = %path.display(),
            files,
            bytes,
            "compose logs retained"
        );
    })
}

fn run_from_base(args: Args, raw_root: &str) -> Result<(String, PathBuf, usize, u64), Failure> {
    let output = Builder::new()
        .prefix("ostree-missing-refs-logs-")
        .tempdir()
        .map_err(|error| Failure {
            path: PathBuf::new(),
            message: format!("cannot create persistent temporary directory: {error}"),
        })?
        .keep();
    run_into(args, raw_root, output)
}

fn run_into(
    args: Args,
    raw_root: &str,
    output: PathBuf,
) -> Result<(String, PathBuf, usize, u64), Failure> {
    let result = collect(args, raw_root, &output);
    match result {
        Ok((compose, files, bytes)) => {
            write_status(&output, &compose, "complete", files, bytes, None).map_err(|error| {
                Failure {
                    path: output.clone(),
                    message: format!(
                        "cannot write capture STATUS after complete collection: {error}"
                    ),
                }
            })?;
            Ok((compose, output, files, bytes))
        }
        Err((compose, message, files, bytes)) => {
            let status_error = write_status(
                &output,
                compose.as_deref().unwrap_or("unknown"),
                "partial",
                files,
                bytes,
                Some(&message),
            );
            Err(Failure {
                path: output,
                message: match status_error {
                    Ok(()) => message,
                    Err(error) => {
                        format!("{message}; additionally cannot write capture STATUS: {error}")
                    }
                },
            })
        }
    }
}

fn collect(
    args: Args,
    raw_root: &str,
    output: &Path,
) -> Result<(String, usize, u64), CollectError> {
    let base = Url::parse(raw_root).map_err(|error| {
        (
            None,
            format!("invalid configured compose URL: {error}"),
            0,
            0,
        )
    })?;
    let client = Client::builder()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|error| (None, format!("cannot create HTTP client: {error}"), 0, 0))?;
    let mut budget = Budget::production();
    let compose = match args.compose {
        Some(compose) if valid_compose(&compose) => compose,
        Some(_) => {
            return Err((
                None,
                "invalid compose ID; expected Fedora-Rawhide-YYYYMMDD.n.N".into(),
                0,
                0,
            ));
        }
        None => {
            let latest = base
                .join("latest-Fedora-Rawhide/COMPOSE_ID")
                .map_err(|error| (None, error.to_string(), 0, 0))?;
            let body = fetch(&client, &base, &latest, MAX_INDEX_BYTES, &mut budget)
                .map_err(|error| (None, format!("cannot pin latest compose: {error}"), 0, 0))?;
            let value = String::from_utf8(body)
                .map_err(|_| (None, "latest COMPOSE_ID is not UTF-8".into(), 0, 0))?;
            let value = value.trim().to_owned();
            if !valid_compose(&value) {
                return Err((None, "latest COMPOSE_ID has an invalid value".into(), 0, 0));
            }
            value
        }
    };
    fs::write(output.join("COMPOSE_ID"), format!("{compose}\n")).map_err(|error| {
        (
            Some(compose.clone()),
            format!("cannot write COMPOSE_ID: {error}"),
            0,
            0,
        )
    })?;
    let compose_base = base
        .join(&format!("{compose}/"))
        .map_err(|error| (Some(compose.clone()), error.to_string(), 0, 0))?;
    let mut collector = Collector {
        client,
        base,
        output: output.to_owned(),
        files: 0,
        bytes: 0,
        saved: BTreeSet::new(),
        budget,
    };
    collector.collect(&compose_base).map_err(|error| {
        (
            Some(compose.clone()),
            error,
            collector.files,
            collector.bytes,
        )
    })?;
    Ok((compose, collector.files, collector.bytes))
}

impl Collector {
    fn collect(&mut self, compose: &Url) -> Result<(), String> {
        let logs = compose.join("logs/").map_err(|error| error.to_string())?;
        let root = self.index(&logs)?;
        if root.contains("global") {
            for file in GLOBAL_FILES {
                self.download(
                    &logs
                        .join(&format!("global/{file}"))
                        .map_err(|error| error.to_string())?,
                    Path::new("logs/global").join(file),
                )?;
            }
        }
        for arch in bounded_names(
            root.into_iter()
                .filter(|name| name != "global" && safe_name(name)),
            MAX_ARCHES,
            "architecture directory",
        )? {
            let arch_url = logs
                .join(&format!("{arch}/"))
                .map_err(|error| error.to_string())?;
            for variant in bounded_names(
                self.index(&arch_url)?
                    .into_iter()
                    .filter(|name| safe_name(name)),
                MAX_VARIANTS,
                "variant directory",
            )? {
                let variant_url = arch_url
                    .join(&format!("{variant}/"))
                    .map_err(|error| error.to_string())?;
                for job in bounded_names(
                    self.index(&variant_url)?
                        .into_iter()
                        .filter(|name| job_name(name)),
                    MAX_JOBS,
                    "OSTree/image-builder job directory",
                )? {
                    let job_url = variant_url
                        .join(&format!("{job}/"))
                        .map_err(|error| error.to_string())?;
                    let available = self.entries(&job_url)?;
                    for file in JOB_FILES.iter().filter(|file| available.contains(**file)) {
                        self.download(
                            &job_url.join(file).map_err(|error| error.to_string())?,
                            Path::new("logs")
                                .join(&arch)
                                .join(&variant)
                                .join(&job)
                                .join(file),
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    fn index(&mut self, url: &Url) -> Result<BTreeSet<String>, String> {
        let body = fetch(
            &self.client,
            &self.base,
            url,
            MAX_INDEX_BYTES,
            &mut self.budget,
        )?;
        let html = String::from_utf8(body).map_err(|_| format!("index is not UTF-8: {url}"))?;
        let selector = Selector::parse("a[href]").expect("static selector");
        Ok(Html::parse_document(&html)
            .select(&selector)
            .filter_map(|link| link.value().attr("href"))
            .filter_map(|href| href.strip_suffix('/'))
            .filter(|name| safe_name(name))
            .map(ToOwned::to_owned)
            .collect())
    }

    fn entries(&mut self, url: &Url) -> Result<BTreeSet<String>, String> {
        let body = fetch(
            &self.client,
            &self.base,
            url,
            MAX_INDEX_BYTES,
            &mut self.budget,
        )?;
        let html = String::from_utf8(body).map_err(|_| format!("index is not UTF-8: {url}"))?;
        let selector = Selector::parse("a[href]").expect("static selector");
        Ok(Html::parse_document(&html)
            .select(&selector)
            .filter_map(|link| link.value().attr("href"))
            .filter_map(|href| href.strip_suffix('/').or(Some(href)))
            .filter(|name| safe_name(name))
            .map(ToOwned::to_owned)
            .collect())
    }

    fn download(&mut self, url: &Url, relative: PathBuf) -> Result<(), String> {
        if !safe_relative_path(&relative) {
            return Err(format!(
                "refusing unsafe output path: {}",
                relative.display()
            ));
        }
        if self.files >= MAX_FILES {
            return Err(format!("file limit ({MAX_FILES}) reached"));
        }
        if !self.saved.insert(relative.clone()) {
            return Err(format!("duplicate remote log path: {}", relative.display()));
        }
        let remaining = MAX_TOTAL_BYTES.saturating_sub(self.bytes);
        if remaining == 0 {
            return Err(format!("total byte limit ({MAX_TOTAL_BYTES}) reached"));
        }
        let bytes = fetch(
            &self.client,
            &self.base,
            url,
            MAX_FILE_BYTES.min(remaining),
            &mut self.budget,
        )?;
        let target = self.output.join(relative);
        fs::create_dir_all(target.parent().expect("relative file has a parent"))
            .map_err(|error| error.to_string())?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|error| format!("cannot create {}: {error}", target.display()))?;
        file.write_all(&bytes)
            .map_err(|error| format!("cannot write {}: {error}", target.display()))?;
        self.files += 1;
        self.bytes += bytes.len() as u64;
        Ok(())
    }
}

fn fetch(
    client: &Client,
    origin: &Url,
    url: &Url,
    limit: u64,
    budget: &mut Budget,
) -> Result<Vec<u8>, String> {
    if url.origin() != origin.origin() {
        return Err(format!("refusing off-origin URL: {url}"));
    }
    let remaining = budget.remaining()?;
    if remaining == 0 {
        return Err("aggregate response byte limit reached".into());
    }
    let timeout = budget.request_timeout()?;
    budget.before_request()?;
    let response = client
        .get(url.clone())
        .timeout(timeout)
        .send()
        .map_err(|error| match budget.check_deadline() {
            Err(_) => format!("overall collection deadline reached while requesting {url}"),
            Ok(()) => format!("GET {url}: {error}"),
        })?;
    if response.status() != StatusCode::OK {
        return Err(format!(
            "GET {url}: HTTP {} (compose may be missing or no longer retained)",
            response.status()
        ));
    }
    if response.content_length().is_some_and(|size| size > limit) {
        return Err(format!("GET {url}: response exceeds {limit}-byte limit"));
    }
    if response
        .content_length()
        .is_some_and(|size| size > remaining)
    {
        return Err(format!(
            "GET {url}: response exceeds aggregate response byte limit"
        ));
    }
    let mut body = Vec::new();
    response
        .take(limit.min(remaining) + 1)
        .read_to_end(&mut body)
        .map_err(|error| match budget.check_deadline() {
            Err(_) => format!("overall collection deadline reached while reading {url}"),
            Ok(()) => format!("read {url}: {error}"),
        })?;
    budget.consume(body.len() as u64)?;
    budget.check_deadline()?;
    if body.len() as u64 > limit {
        return Err(format!("GET {url}: response exceeds {limit}-byte limit"));
    }
    if body.len() as u64 > remaining {
        return Err(format!(
            "GET {url}: response exceeds aggregate response byte limit"
        ));
    }
    Ok(body)
}

fn bounded_names(
    names: impl Iterator<Item = String>,
    limit: usize,
    kind: &str,
) -> Result<Vec<String>, String> {
    let values: Vec<_> = names.collect();
    if values.len() > limit {
        return Err(format!("{kind} limit ({limit}) exceeded"));
    }
    Ok(values)
}

fn safe_name(name: &str) -> bool {
    name != "."
        && name != ".."
        && !name.is_empty()
        && name.len() <= 96
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn safe_relative_path(path: &Path) -> bool {
    path.is_relative()
        && path.components().all(|component| matches!(component, std::path::Component::Normal(name) if safe_name(&name.to_string_lossy())))
}

fn job_name(name: &str) -> bool {
    let Some((kind, id)) = name.rsplit_once('-') else {
        return false;
    };
    matches!(kind, "ostree" | "image-builder")
        && !id.is_empty()
        && id.bytes().all(|byte| byte.is_ascii_digit())
}

fn valid_compose(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("Fedora-Rawhide-") else {
        return false;
    };
    let Some((date, number)) = rest.split_once(".n.") else {
        return false;
    };
    date.len() == 8
        && date.bytes().all(|byte| byte.is_ascii_digit())
        && !number.is_empty()
        && number.bytes().all(|byte| byte.is_ascii_digit())
}

fn write_status(
    path: &Path,
    compose: &str,
    status: &str,
    files: usize,
    bytes: u64,
    error: Option<&str>,
) -> Result<(), std::io::Error> {
    let mut value = format!(
        "status={status}\ncompose_id={compose}\nfiles={files}\nbytes={bytes}\nsource={RAW_HIDE_ROOT}\n"
    );
    if let Some(error) = error {
        value.push_str(&format!("error={error}\n"));
    }
    fs::write(path.join("STATUS"), value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::BTreeMap,
        io::{BufRead, BufReader, ErrorKind},
        net::TcpListener,
        thread,
    };

    fn server(routes: BTreeMap<&'static str, (u16, &'static str)>, requests: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut served = 0;
            while served < requests && Instant::now() < deadline {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(error) => panic!("accept test request: {error}"),
                };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                let path = request.split_whitespace().nth(1).unwrap();
                let (status, body) = routes.get(path).copied().unwrap_or((404, "missing"));
                write!(stream, "HTTP/1.1 {status} test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                served += 1;
            }
        });
        format!("http://{address}/compose/rawhide/")
    }

    fn delayed_server(delay: Duration) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            thread::sleep(delay);
            let _ = write!(
                stream,
                "HTTP/1.1 200 test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
        });
        format!("http://{address}/compose/rawhide/")
    }

    #[test]
    fn validates_compose_ids_and_safe_links() {
        assert!(valid_compose("Fedora-Rawhide-20260826.n.0"));
        assert!(!valid_compose("Fedora-Rawhide-20260826.n.0/../../x"));
        assert!(safe_name("x86_64"));
        assert!(!safe_name("."));
        assert!(!safe_name(".."));
        assert!(!safe_name("../evil"));
        assert!(!safe_name("https://evil.invalid"));
        assert!(!safe_relative_path(Path::new("logs/../escape")));
        assert!(job_name("ostree-123"));
        assert!(job_name("image-builder-456"));
        assert!(!job_name("ostree-../../x"));
    }

    #[test]
    fn collects_pinned_relevant_logs_without_following_malicious_links() {
        let mut routes = BTreeMap::new();
        routes.insert(
            "/compose/rawhide/latest-Fedora-Rawhide/COMPOSE_ID",
            (200, "Fedora-Rawhide-20260826.n.0\n"),
        );
        routes.insert("/compose/rawhide/Fedora-Rawhide-20260826.n.0/logs/", (200, "<a href=\"global/\">global</a><a href=\"x86_64/\">x</a><a href=\"../\">parent</a><a href=\"../escape/\">bad</a>"));
        routes.insert(
            "/compose/rawhide/Fedora-Rawhide-20260826.n.0/logs/global/pungi.global.log",
            (200, "pungi"),
        );
        routes.insert(
            "/compose/rawhide/Fedora-Rawhide-20260826.n.0/logs/global/config-dump.global.log",
            (200, "config"),
        );
        routes.insert(
            "/compose/rawhide/Fedora-Rawhide-20260826.n.0/logs/global/deliverables.json",
            (200, "{}"),
        );
        routes.insert(
            "/compose/rawhide/Fedora-Rawhide-20260826.n.0/logs/x86_64/",
            (
                200,
                "<a href=\"Silverblue/\">variant</a><a href=\"../\">parent</a>",
            ),
        );
        routes.insert(
            "/compose/rawhide/Fedora-Rawhide-20260826.n.0/logs/x86_64/Silverblue/",
            (
                200,
                "<a href=\"ostree-12/\">job</a><a href=\"random-1/\">ignore</a><a href=\"../\">parent</a>",
            ),
        );
        routes.insert("/compose/rawhide/Fedora-Rawhide-20260826.n.0/logs/x86_64/Silverblue/ostree-12/", (200, "<a href=\"create-ostree-repo.log\">repo</a><a href=\"runroot.log\">run</a><a href=\"secret.log\">no</a><a href=\"../\">parent</a>"));
        routes.insert("/compose/rawhide/Fedora-Rawhide-20260826.n.0/logs/x86_64/Silverblue/ostree-12/create-ostree-repo.log", (200, "ostree"));
        routes.insert("/compose/rawhide/Fedora-Rawhide-20260826.n.0/logs/x86_64/Silverblue/ostree-12/runroot.log", (200, "runroot"));
        let base = server(routes, 10);
        let (compose, path, files, _) = run_from_base(Args::default(), &base).unwrap();
        assert_eq!(compose, "Fedora-Rawhide-20260826.n.0");
        assert_eq!(files, 5);
        assert_eq!(
            fs::read_to_string(
                path.join("logs/x86_64/Silverblue/ostree-12/create-ostree-repo.log")
            )
            .unwrap(),
            "ostree"
        );
        assert!(
            !path
                .join("logs/x86_64/Silverblue/ostree-12/secret.log")
                .exists()
        );
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn rejects_non_success_and_oversized_responses() {
        let mut routes = BTreeMap::new();
        routes.insert("/compose/rawhide/missing", (404, "gone"));
        routes.insert("/compose/rawhide/large", (200, "abcdef"));
        let base = server(routes, 2);
        let origin = Url::parse(&base).unwrap();
        let client = Client::builder().redirect(Policy::none()).build().unwrap();
        let mut budget = Budget::new(2, 16, Duration::from_secs(1));
        assert!(
            fetch(
                &client,
                &origin,
                &origin.join("missing").unwrap(),
                8,
                &mut budget
            )
            .unwrap_err()
            .contains("HTTP 404")
        );
        assert!(
            fetch(
                &client,
                &origin,
                &origin.join("large").unwrap(),
                3,
                &mut budget
            )
            .unwrap_err()
            .contains("exceeds")
        );
    }

    #[test]
    fn enforces_the_total_byte_budget_before_a_download() {
        let temp = tempfile::tempdir().unwrap();
        let base = Url::parse("http://127.0.0.1:9/compose/rawhide/").unwrap();
        let mut collector = Collector {
            client: Client::builder().build().unwrap(),
            base: base.clone(),
            output: temp.path().to_owned(),
            files: 0,
            bytes: MAX_TOTAL_BYTES,
            saved: BTreeSet::new(),
            budget: Budget::production(),
        };
        assert!(
            collector
                .download(
                    &base.join("one.log").unwrap(),
                    PathBuf::from("logs/one.log")
                )
                .unwrap_err()
                .contains("total byte limit")
        );
    }

    #[test]
    fn bounds_directory_lists_instead_of_silently_truncating() {
        assert!(
            bounded_names(["one", "two"].into_iter().map(str::to_owned), 1, "test")
                .unwrap_err()
                .contains("test limit")
        );
    }

    #[test]
    fn request_budget_stops_empty_index_crawls_before_an_excess_request() {
        let mut routes = BTreeMap::new();
        routes.insert(
            "/compose/rawhide/Fedora-Rawhide-20260826.n.0/logs/",
            (200, "<a href=\"a/\">a</a><a href=\"b/\">b</a>"),
        );
        routes.insert(
            "/compose/rawhide/Fedora-Rawhide-20260826.n.0/logs/a/",
            (200, ""),
        );
        let base = server(routes, 2);
        let base_url = Url::parse(&base).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let mut collector = Collector {
            client: Client::builder().build().unwrap(),
            base: base_url.clone(),
            output: temp.path().to_owned(),
            files: 0,
            bytes: 0,
            saved: BTreeSet::new(),
            budget: Budget::new(2, 1024, Duration::from_secs(1)),
        };
        let compose = base_url.join("Fedora-Rawhide-20260826.n.0/").unwrap();
        assert!(
            collector
                .collect(&compose)
                .unwrap_err()
                .contains("request limit")
        );
        assert_eq!(collector.budget.requests, 2);
    }

    #[test]
    fn aggregate_response_budget_stops_before_a_request() {
        let origin = Url::parse("http://127.0.0.1:9/compose/rawhide/").unwrap();
        let client = Client::builder().build().unwrap();
        let mut budget = Budget::new(1, 0, Duration::from_secs(1));
        assert!(
            fetch(
                &client,
                &origin,
                &origin.join("never").unwrap(),
                8,
                &mut budget
            )
            .unwrap_err()
            .contains("aggregate response byte limit")
        );
        assert_eq!(budget.requests, 0);
    }

    #[test]
    fn overall_deadline_limits_a_delayed_index_request() {
        let base = Url::parse(&delayed_server(Duration::from_millis(100))).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let mut collector = Collector {
            client: Client::builder().build().unwrap(),
            base: base.clone(),
            output: temp.path().to_owned(),
            files: 0,
            bytes: 0,
            saved: BTreeSet::new(),
            budget: Budget::new(1, 1024, Duration::from_millis(10)),
        };
        let compose = base.join("Fedora-Rawhide-20260826.n.0/").unwrap();
        assert!(
            collector
                .collect(&compose)
                .unwrap_err()
                .contains("overall collection deadline reached")
        );
    }

    #[test]
    fn status_write_failures_are_reported_after_successful_collection() {
        let mut routes = BTreeMap::new();
        routes.insert(
            "/compose/rawhide/Fedora-Rawhide-20260826.n.0/logs/",
            (200, ""),
        );
        let base = server(routes, 1);
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("STATUS")).unwrap();
        let failure = run_into(
            Args {
                compose: Some("Fedora-Rawhide-20260826.n.0".into()),
            },
            &base,
            temp.path().to_owned(),
        )
        .unwrap_err();
        assert!(
            failure
                .message
                .contains("cannot write capture STATUS after complete collection")
        );
    }

    #[test]
    fn primary_and_status_write_failures_are_both_reported() {
        let mut routes = BTreeMap::new();
        routes.insert(
            "/compose/rawhide/Fedora-Rawhide-20260826.n.0/logs/",
            (404, "missing"),
        );
        let base = server(routes, 1);
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("STATUS")).unwrap();
        let failure = run_into(
            Args {
                compose: Some("Fedora-Rawhide-20260826.n.0".into()),
            },
            &base,
            temp.path().to_owned(),
        )
        .unwrap_err();
        assert!(failure.message.contains("HTTP 404"));
        assert!(
            failure
                .message
                .contains("additionally cannot write capture STATUS")
        );
    }
}
