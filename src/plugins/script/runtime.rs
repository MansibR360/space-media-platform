//! A JavaScript runtime for source plugins.
//!
//! SPACE runs plugins written to the Nuvio provider contract, so a provider
//! written for that ecosystem works here unchanged:
//!
//! ```js
//! async function getStreams(tmdbId, mediaType, season, episode) {
//!   const res = await fetch(url)
//!   return [{ name, title, url, quality, headers }]
//! }
//! ```
//!
//! Why this exists: nearly every site in the catalog only offers a watch page,
//! which means leaving the app to play anything. A provider returns the stream
//! url itself, so playback happens in our own player.
//!
//! `fetch` is real but synchronous underneath: the host performs the request
//! and hands back an already-resolved promise. `await` therefore works exactly
//! as a provider expects, without this having to run a full async event loop —
//! only the microtask queue is drained. Requests within one provider run in
//! sequence; providers themselves run concurrently.

use std::time::Duration;

use boa_engine::object::ObjectInitializer;
use boa_engine::object::builtins::JsPromise;
use boa_engine::property::Attribute;
use boa_engine::{
    Context, JsArgs, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction,
    Source, js_string,
};
use serde_json::Value;

use crate::error::{ApiError, ApiResult};

/// A stream a provider returned.
#[derive(Debug, Clone)]
pub struct ScriptStream {
    pub name: String,
    pub title: String,
    pub url: String,
    pub quality: String,
    pub headers: Value,
}

/// Ceiling on one provider run, so a slow or looping script cannot hold a
/// request open.
const RUN_TIMEOUT: Duration = Duration::from_secs(25);
/// Ceiling on a single fetch inside a provider.
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);
/// Refuse absurd response bodies rather than pulling them into the engine.
const MAX_BODY: usize = 8 * 1024 * 1024;

/// The environment a provider expects: `global`, `URLSearchParams`, cheerio
/// and a `require` shim. Evaluated before every provider.
const PRELUDE: &str = include_str!("prelude.js");

/// Per-run host state.
///
/// The engine's native functions must be plain function pointers (boa requires
/// captured state to be `Copy`), and a run owns its blocking thread for its
/// whole life — so the http client and plugin name live here rather than being
/// closed over.
struct HostContext {
    plugin: String,
    handle: tokio::runtime::Handle,
    client: reqwest::Client,
    /// Everything the provider printed, kept so a run that finds nothing can
    /// be explained instead of guessed at.
    log: Vec<String>,
}

thread_local! {
    static HOST: std::cell::RefCell<Option<HostContext>> =
        const { std::cell::RefCell::new(None) };
}

fn with_host<T>(f: impl FnOnce(&HostContext) -> JsResult<T>) -> JsResult<T> {
    HOST.with(|host| {
        let borrowed = host.borrow();
        let host = borrowed
            .as_ref()
            .ok_or_else(|| type_error("plugin host is not set up"))?;
        f(host)
    })
}

fn type_error(message: impl Into<String>) -> JsError {
    JsNativeError::typ().with_message(message.into()).into()
}

/// The host side of `fetch`.
///
/// Runs on a blocking thread (see [`run_provider`]), so blocking on the shared
/// runtime here is safe and keeps the JS side free of an event loop.
fn perform_fetch(
    host: &HostContext,
    url: &str,
    init: Option<&JsObject>,
    context: &mut Context,
) -> JsResult<(u16, String)> {
    let mut method = "GET".to_string();
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut body: Option<String> = None;

    if let Some(init) = init {
        if let Ok(value) = init.get(js_string!("method"), context)
            && let Some(text) = value.as_string()
        {
            method = text.to_std_string_escaped().to_uppercase();
        }
        if let Ok(value) = init.get(js_string!("body"), context)
            && let Some(text) = value.as_string()
        {
            body = Some(text.to_std_string_escaped());
        }
        if let Ok(value) = init.get(js_string!("headers"), context)
            && let Some(object) = value.as_object()
            && let Ok(keys) = object.own_property_keys(context)
        {
            for key in keys {
                let name = key.to_string();
                if let Ok(v) = object.get(key, context)
                    && let Some(text) = v.as_string()
                {
                    headers.push((name, text.to_std_string_escaped()));
                }
            }
        }
    }

    let mut builder = match method.as_str() {
        "POST" => host.client.post(url),
        "PUT" => host.client.put(url),
        "HEAD" => host.client.head(url),
        _ => host.client.get(url),
    };
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    if let Some(body) = body {
        builder = builder.body(body);
    }
    let request = builder.timeout(FETCH_TIMEOUT);

    // The engine is synchronous, so this thread has to wait for the response.
    // `Handle::block_on` refuses to run inside a runtime context — which a
    // blocking task still has — so the request is spawned onto the runtime and
    // this thread waits on a plain channel instead.
    let (tx, rx) = std::sync::mpsc::channel();
    host.handle.spawn(async move {
        let outcome = async {
            let response = request.send().await?;
            let status = response.status().as_u16();
            let bytes = response.bytes().await?;
            let text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_BODY)]).into_owned();
            Ok::<_, reqwest::Error>((status, text))
        }
        .await;
        let _ = tx.send(outcome.map_err(|e| e.to_string()));
    });

    match rx.recv_timeout(FETCH_TIMEOUT + Duration::from_secs(5)) {
        Ok(Ok(pair)) => Ok(pair),
        Ok(Err(err)) => Err(type_error(format!("fetch failed: {err}"))),
        Err(_) => Err(type_error("fetch timed out")),
    }
}

/// `res.text()` — the body travels on the response object itself, so this
/// needs no captured state.
fn response_text(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let body = this
        .as_object()
        .ok_or_else(|| type_error("text() called on a non-response"))?
        .get(js_string!("__body"), context)?;
    Ok(JsPromise::resolve(body, context)?.into())
}

/// `res.json()`.
fn response_json(this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let raw = this
        .as_object()
        .ok_or_else(|| type_error("json() called on a non-response"))?
        .get(js_string!("__body"), context)?
        .as_string()
        .ok_or_else(|| type_error("response has no body"))?
        .to_std_string_escaped();

    let parsed: Value = serde_json::from_str(&raw)
        .map_err(|e| type_error(format!("response is not json: {e}")))?;
    let value = JsValue::from_json(&parsed, context)?;
    Ok(JsPromise::resolve(value, context)?.into())
}

/// Builds the Response a provider gets back from `fetch`.
///
/// The body rides on the object as `__body`, so `text()` and `json()` need no
/// captured state — boa requires native functions to be plain fn pointers.
fn make_response(status: u16, body: String, context: &mut Context) -> JsResult<JsObject> {
    Ok(ObjectInitializer::new(context)
        .property(js_string!("status"), status, Attribute::all())
        .property(
            js_string!("ok"),
            (200..300).contains(&status),
            Attribute::all(),
        )
        .property(
            js_string!("__body"),
            JsString::from(body.as_str()),
            Attribute::all(),
        )
        .function(NativeFunction::from_fn_ptr(response_text), js_string!("text"), 0)
        .function(NativeFunction::from_fn_ptr(response_json), js_string!("json"), 0)
        .build())
}

fn js_fetch(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let url = args
        .get_or_undefined(0)
        .as_string()
        .ok_or_else(|| type_error("fetch(url) needs a url"))?
        .to_std_string_escaped();
    let init = args.get_or_undefined(1).as_object();

    let (status, body) = with_host(|host| perform_fetch(host, &url, init.as_ref(), context))?;
    let response = make_response(status, body, context)?;
    Ok(JsPromise::resolve(JsValue::from(response), context)?.into())
}

fn join_args(args: &[JsValue], context: &mut Context) -> String {
    args.iter()
        .map(|a| {
            a.to_string(context)
                .map(|s| s.to_std_string_escaped())
                .unwrap_or_else(|_| "<unprintable>".into())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn record(line: String, warn: bool) {
    HOST.with(|host| {
        if let Some(ctx) = host.borrow_mut().as_mut() {
            if warn {
                tracing::warn!(plugin = %ctx.plugin, "{line}");
            } else {
                tracing::debug!(plugin = %ctx.plugin, "{line}");
            }
            // bounded: a chatty provider must not grow without limit
            if ctx.log.len() < 200 {
                ctx.log.push(line);
            }
        }
    });
}

fn console_log(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let line = join_args(args, context);
    record(line, false);
    Ok(JsValue::undefined())
}

fn console_warn(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let line = join_args(args, context);
    record(line, true);
    Ok(JsValue::undefined())
}

/// Installs the globals a provider is written against: `fetch` and `console`.
fn install_globals(context: &mut Context) -> JsResult<()> {
    context.register_global_callable(
        js_string!("fetch"),
        1,
        NativeFunction::from_fn_ptr(js_fetch),
    )?;

    let console = ObjectInitializer::new(context)
        .function(NativeFunction::from_fn_ptr(console_log), js_string!("log"), 0)
        .function(NativeFunction::from_fn_ptr(console_log), js_string!("info"), 0)
        .function(NativeFunction::from_fn_ptr(console_log), js_string!("debug"), 0)
        .function(NativeFunction::from_fn_ptr(console_warn), js_string!("warn"), 0)
        .function(NativeFunction::from_fn_ptr(console_warn), js_string!("error"), 0)
        .build();
    context.register_global_property(js_string!("console"), console, Attribute::all())?;
    Ok(())
}

/// Runs one provider's `getStreams` and returns what it produced.
///
/// Blocking by nature (the engine is synchronous), so callers hand this to a
/// blocking thread; [`super::ScriptPlugin::streams`] does that.
pub fn run_provider(
    plugin: &str,
    source: &str,
    tmdb_id: &str,
    media_type: &str,
    season: Option<u32>,
    episode: Option<u32>,
    handle: tokio::runtime::Handle,
    client: reqwest::Client,
) -> ApiResult<Vec<ScriptStream>> {
    let started = std::time::Instant::now();

    HOST.with(|host| {
        *host.borrow_mut() = Some(HostContext {
            plugin: plugin.to_string(),
            handle,
            client,
            log: Vec::new(),
        });
    });
    // Whatever happens below, this thread must not keep a plugin's http client
    // and name alive for the next run that lands on it.
    let _reset = HostReset;

    let mut context = Context::default();
    install_globals(&mut context)
        .map_err(|e| ApiError::Unavailable(format!("plugin {plugin}: {e}")))?;

    // Providers are written for React Native and reach for `global`,
    // `URLSearchParams` and cheerio. The prelude supplies them so a provider
    // from the wild runs unmodified.
    context
        .eval(Source::from_bytes(PRELUDE.as_bytes()))
        .map_err(|e| ApiError::Unavailable(format!("plugin prelude failed: {e}")))?;

    context
        .eval(Source::from_bytes(source.as_bytes()))
        .map_err(|e| ApiError::Unavailable(format!("plugin {plugin} failed to load: {e}")))?;

    let get_streams = context
        .global_object()
        .get(js_string!("getStreams"), &mut context)
        .ok()
        .and_then(|v| v.as_callable())
        .ok_or_else(|| {
            ApiError::Unavailable(format!("plugin {plugin} exports no getStreams function"))
        })?;

    let args = [
        JsValue::from(JsString::from(tmdb_id)),
        JsValue::from(JsString::from(media_type)),
        season.map(JsValue::from).unwrap_or(JsValue::null()),
        episode.map(JsValue::from).unwrap_or(JsValue::null()),
    ];

    let returned = get_streams
        .call(&JsValue::undefined(), &args, &mut context)
        .map_err(|e| ApiError::Unavailable(format!("plugin {plugin} threw: {e}")))?;

    // `getStreams` is async, so it hands back a promise; draining the microtask
    // queue settles it, since every fetch was already resolved by the host.
    let settled = match JsPromise::from_object(
        returned
            .as_object()
            .ok_or_else(|| ApiError::Unavailable(format!("plugin {plugin} returned no value")))?,
    ) {
        Ok(promise) => {
            context.run_jobs().map_err(|e| {
                ApiError::Unavailable(format!("plugin {plugin} job queue failed: {e}"))
            })?;
            match promise.state() {
                boa_engine::builtins::promise::PromiseState::Fulfilled(value) => value,
                boa_engine::builtins::promise::PromiseState::Rejected(err) => {
                    let text = err
                        .to_string(&mut context)
                        .map(|s| s.to_std_string_escaped())
                        .unwrap_or_else(|_| "rejected".into());
                    return Err(ApiError::Unavailable(format!("plugin {plugin}: {text}")));
                }
                boa_engine::builtins::promise::PromiseState::Pending => {
                    return Err(ApiError::Unavailable(format!(
                        "plugin {plugin} never settled"
                    )));
                }
            }
        }
        // a provider that returns an array directly still works
        Err(_) => returned,
    };

    if started.elapsed() > RUN_TIMEOUT {
        return Err(ApiError::Unavailable(format!("plugin {plugin} timed out")));
    }

    // `to_json` yields None for values with no JSON form (undefined, a
    // function); a provider that returns one of those simply found nothing.
    let json = settled
        .to_json(&mut context)
        .map_err(|e| ApiError::Unavailable(format!("plugin {plugin} returned junk: {e}")))?
        .unwrap_or(Value::Null);

    Ok(parse_streams(&json))
}

/// What a provider printed during the run just finished.
pub fn last_run_log() -> Vec<String> {
    HOST.with(|host| {
        host.borrow()
            .as_ref()
            .map(|c| c.log.clone())
            .unwrap_or_default()
    })
}

/// Clears the per-thread host state when a run ends, however it ends.
struct HostReset;

impl Drop for HostReset {
    fn drop(&mut self) {
        HOST.with(|host| {
            *host.borrow_mut() = None;
        });
    }
}

/// Reads the stream list a provider returned, skipping anything unusable
/// rather than failing the whole run for one bad entry.
fn parse_streams(value: &Value) -> Vec<ScriptStream> {
    let Some(rows) = value.as_array() else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|row| {
            let url = row.get("url").and_then(Value::as_str)?;
            if url.is_empty() {
                return None;
            }
            Some(ScriptStream {
                name: row
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                title: row
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                url: url.to_string(),
                quality: row
                    .get("quality")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                headers: row.get("headers").cloned().unwrap_or(Value::Null),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle() -> tokio::runtime::Handle {
        tokio::runtime::Handle::current()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn runs_a_provider_and_reads_its_streams() {
        let source = r#"
            async function getStreams(tmdbId, mediaType, season, episode) {
                console.log("asked for", tmdbId, mediaType);
                return [
                    { name: "Demo", title: "1080p", url: "https://cdn.example/" + tmdbId + ".m3u8", quality: "1080p" },
                    { name: "Demo", title: "720p", url: "https://cdn.example/low.m3u8", quality: "720p" }
                ];
            }
        "#;
        let out = run_provider(
            "demo", source, "27205", "movie", None, None,
            handle(), reqwest::Client::new(),
        )
        .expect("provider ran");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].url, "https://cdn.example/27205.m3u8");
        assert_eq!(out[0].quality, "1080p");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn series_arguments_reach_the_provider() {
        let source = r#"
            async function getStreams(tmdbId, mediaType, season, episode) {
                return [{ name: "S", title: "t", url: "https://x/" + mediaType + "/" + season + "/" + episode, quality: "" }];
            }
        "#;
        let out = run_provider(
            "demo", source, "1396", "tv", Some(2), Some(3),
            handle(), reqwest::Client::new(),
        )
        .unwrap();
        assert_eq!(out[0].url, "https://x/tv/2/3");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_thrown_provider_is_an_error_not_a_panic() {
        let source = "async function getStreams() { throw new Error('upstream down') }";
        let err = run_provider(
            "demo", source, "1", "movie", None, None,
            handle(), reqwest::Client::new(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("upstream down"), "{err}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_provider_without_getstreams_is_rejected() {
        let err = run_provider(
            "demo", "var x = 1", "1", "movie", None, None,
            handle(), reqwest::Client::new(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("getStreams"), "{err}");
    }

    /// The prelude is what lets a provider from the wild run unmodified, so it
    /// is exercised the way those providers actually use it: `require`,
    /// `cheerio.load`, a selector, `.each`, `.attr` and `.text`.
    #[tokio::test(flavor = "multi_thread")]
    async fn cheerio_and_the_node_shims_work() {
        let source = r##"
            var cheerio = require("cheerio-without-node-native");
            async function getStreams(tmdbId) {
                const html =
                  '<html><body><div id="pickDownload">' +
                  '<a href="/one.mkv" class="link">1080p HEVC</a>' +
                  '<a href="/two.mkv" class="link">720p</a>' +
                  '</div><span id="only">solo</span></body></html>';
                const $ = cheerio.load(html);

                const out = [];
                $("div#pickDownload a").each(function (i, el) {
                    out.push({
                        name: "T",
                        title: $(el).text(),
                        url: "https://cdn.example" + $(el).attr("href"),
                        quality: $(el).text().split(" ")[0]
                    });
                });

                // selectors that must not match, and one that must
                if ($("div.missing").length !== 0) throw new Error("bad selector match");
                if ($("#only").text() !== "solo") throw new Error("id selector failed");

                // the RN globals providers rely on
                const params = new URLSearchParams();
                params.append("q", "a b");
                if (params.toString() !== "q=a%20b" && params.toString() !== "q=a+b") {
                    throw new Error("URLSearchParams: " + params.toString());
                }
                if (typeof global === "undefined") throw new Error("no global");
                return out;
            }
        "##;
        let out = run_provider(
            "cheerio-demo", source, "1", "movie", None, None,
            handle(), reqwest::Client::new(),
        )
        .expect("provider ran");

        assert_eq!(out.len(), 2, "both links found");
        assert_eq!(out[0].url, "https://cdn.example/one.mkv");
        assert_eq!(out[0].quality, "1080p");
        assert_eq!(out[1].url, "https://cdn.example/two.mkv");
    }

    /// Scraped pages are malformed constantly; the parser must cope rather
    /// than throw.
    #[tokio::test(flavor = "multi_thread")]
    async fn malformed_html_still_parses() {
        let source = r#"
            var cheerio = require("cheerio");
            async function getStreams() {
                const $ = cheerio.load(
                  '<div class=box><img src=x.png><p>text<br>more' +
                  '<script>var a = "<div>not markup</div>";</script>'
                );
                if ($("div.box").length !== 1) throw new Error("unquoted attribute failed");
                // The markup inside <script> must not become elements. (Its
                // text does show up in .text(), exactly as cheerio behaves.)
                if ($("div").length !== 1) throw new Error("script contents parsed as markup");
                return [{ name: "ok", title: "", url: "https://x/" + $("img").attr("src"), quality: "" }];
            }
        "#;
        let out = run_provider(
            "malformed", source, "1", "movie", None, None,
            handle(), reqwest::Client::new(),
        )
        .expect("provider ran");
        assert_eq!(out[0].url, "https://x/x.png");
    }


    #[tokio::test(flavor = "multi_thread")]
    async fn prelude_diagnostics() {
        let source = r##"
            var report = "";
            function step(label, fn) {
                try { report += " " + label + "=" + fn(); }
                catch (e) { report += " " + label + "!ERR:" + (e && e.message ? e.message : e); }
            }
            var c = null, d = null, sel = null;
            step("load_type", function () { c = require("cheerio"); return typeof c.load });
            step("dollar", function () { d = c.load("<div id=box><b>hi</b></div>"); return typeof d });
            step("call", function () { sel = d("b"); return typeof sel });
            step("len", function () { return sel.length });
            step("text", function () { return sel.text() });
            step("attr", function () { return d("div").attr("id") });
            async function getStreams() {
                return [{ name: "d", title: report, url: "https://x/y", quality: "" }];
            }
        "##;
        let out = run_provider("diag", source, "1", "movie", None, None, handle(), reqwest::Client::new());
        match out {
            Ok(v) => println!("DIAG:{}", v[0].title),
            Err(e) => println!("DIAG ERR: {e}"),
        }
    }


    /// Runs the real provider that ships in `plugins/js/`, against the live
    /// site. Ignored by default because it depends on the network and on that
    /// site being up; run it deliberately when working on the runtime:
    ///
    /// ```text
    /// cargo test --bin space real_provider -- --ignored --nocapture
    /// ```
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn real_provider_against_the_live_site() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .try_init();

        let dir = std::path::Path::new("plugins/js/nuvio-animepahe");
        let source = std::fs::read_to_string(dir.join("provider.js")).expect("provider on disk");

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .user_agent(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36                  (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
            )
            .build()
            .unwrap();

        // Attack on Titan, season 1 episode 1
        match run_provider(
            "nuvio-animepahe", &source, "1429", "tv", Some(1), Some(1),
            handle(), client,
        ) {
            Ok(streams) => {
                println!("PROVIDER RETURNED {} streams", streams.len());
                for s in &streams {
                    println!("  [{}] {} -> {}", s.quality, s.title, s.url);
                }
            }
            Err(err) => println!("PROVIDER ERROR: {err}"),
        }
    }

    #[test]
    fn unusable_rows_are_skipped_not_fatal() {
        let json = serde_json::json!([
            { "url": "https://good/1.m3u8", "quality": "1080p" },
            { "quality": "720p" },
            { "url": "" }
        ]);
        let out = parse_streams(&json);
        assert_eq!(out.len(), 1, "only the row with a url survives");
    }
}
