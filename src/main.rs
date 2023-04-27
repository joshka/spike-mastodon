#![warn(
    clippy::pedantic,
    clippy::nursery,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cargo
)]

use anyhow::{Context, Result};
use directories::ProjectDirs;
use mastodon_async::helpers::toml;
use mastodon_async::page::Page;
use mastodon_async::prelude::Status;
use mastodon_async::registration::Registered;
use mastodon_async::{helpers, scopes::Scopes, Registration};
use mastodon_async::{Data, Mastodon};
use std::fs::create_dir_all;
use std::path::PathBuf;
use std::{
    fs::File,
    io::{self, BufRead, Write},
};
use tracing::{debug, instrument, warn};
use tracing::{error, info};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_log::LogTracer;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, EnvFilter, Layer};

#[tokio::main]
async fn main() -> Result<()> {
    let (_json_guard, _txt_guard) = setup_logging()?;
    info!("Starting spike-mastodon");
    if let Err(err) = run().await {
        error!(?err, "error");
    }
    Ok(())
}

/// Setup tracing for the application. This includes the following:
/// - a JSON log file
/// - a text log file
/// - logging to stderr
/// - sending logs from the log crate to tracing subscribers
///
/// A real app would probably choose only one of these
fn setup_logging() -> Result<(WorkerGuard, WorkerGuard)> {
    // handle logs from the log crate by forwarding them to tracing
    LogTracer::init()?;

    let json_file = File::create("logfile.json")?;
    let (json_writer, json_guard) = tracing_appender::non_blocking(json_file);
    let json_layer = fmt::layer()
        .json()
        .with_writer(json_writer)
        .with_filter(create_filter()?);

    let txt_file = File::create("logfile.txt")?;
    let (txt_writer, txt_guard) = tracing_appender::non_blocking(txt_file);

    let txt_layer = fmt::layer()
        .with_writer(txt_writer)
        .with_filter(create_filter()?);

    let stderr_layer = fmt::layer().with_writer(io::stderr).with_filter(
        EnvFilter::default()
            .add_directive("spike_mastodon=trace".parse()?)
            .add_directive("info".parse()?),
    );

    let subscriber = tracing_subscriber::registry()
        .with(json_layer)
        .with(txt_layer)
        .with(stderr_layer);

    tracing::subscriber::set_global_default(subscriber)
        .context("setting default subscriber failed")?;

    Ok((json_guard, txt_guard))
}

fn create_filter() -> Result<EnvFilter> {
    let filter = EnvFilter::default()
        .add_directive("mastodon_async=trace".parse()?)
        .add_directive("spike_mastodon=trace".parse()?)
        .add_directive("info".parse()?);
    Ok(filter)
}

#[instrument(err)]
async fn run() -> Result<()> {
    let mastodon = match load_credentials() {
        Ok(data) => Mastodon::from(data),
        Err(reason) => {
            info!(%reason, "No credentials found. This is fine if you're running this for the first time.");
            let server_name = get_server_name()?;
            let registration = register(server_name).await?;
            let mastodon = authenticate(registration).await?;
            save_credentials(&mastodon)?;
            mastodon
        }
    };
    verify_credentials(&mastodon).await?;

    show_timeline(&mastodon).await?;

    Ok(())
}

#[instrument(err)]
fn load_credentials() -> Result<Data> {
    let path = config_folder()?.join("credentials.toml");
    let data = toml::from_file(&path).with_context(|| format!("cannot load file {path:?}"))?;
    Ok(data)
}

#[instrument(skip_all, err)]
fn save_credentials(client: &Mastodon) -> Result<()> {
    let folder = config_folder()?;
    create_dir_all(folder.clone()).context("Can't create config folder")?;
    let path = folder.join("credentials.toml");
    toml::to_file(&client.data, &path).with_context(|| format!("cannot save file {path:?}"))?;
    Ok(())
}

#[instrument(err, ret)]
fn config_folder() -> Result<PathBuf> {
    let project_dirs = ProjectDirs::from("com", "joshka", "mastodon-async")
        .context("Couldn't determine config folder path")?;
    Ok(project_dirs.config_dir().into())
}

#[instrument(err, ret)]
fn get_server_name() -> Result<String> {
    let mut stdout = io::stdout().lock();
    let mut stdin = io::stdin().lock();

    writeln!(&mut stdout, "Enter server name:").context("failed to write to stdout")?;
    stdout.flush().context("failed to flush stdout")?;

    let mut input = String::new();
    stdin
        .read_line(&mut input)
        .context("failed to read input")?;

    Ok(input.trim().to_owned())
}

#[instrument(err)]
async fn register(server_name: String) -> Result<Registered> {
    let registered = Registration::new(server_name)
        .client_name("joshka-mastodon-async")
        .redirect_uris("urn:ietf:wg:oauth:2.0:oob")
        .scopes(Scopes::read_all())
        .website("https://github.com/joshka/mastodon-async")
        .build()
        .await
        .context("Couldn't register app")?;
    let (base, client_id, _client_secret, _redirect, scopes, _force_login) =
        registered.clone().into_parts();
    info!(base, client_id, %scopes, "registration complete");
    Ok(registered)
}

#[instrument(skip_all, err)]
async fn authenticate(registration: Registered) -> Result<Mastodon> {
    let url = registration
        .authorize_url()
        .context("Couldn't get authorize URL")?;
    webbrowser::open(&url).context("opening browser")?;
    let client = helpers::cli::authenticate(registration)
        .await
        .context("Couldn't authenticate")?;
    info!("authentication succeeded");
    Ok(client)
}

#[instrument(skip_all, err)]
async fn verify_credentials(client: &Mastodon) -> Result<(), anyhow::Error> {
    let account = client
        .verify_credentials()
        .await
        .context("Couldn't get account")?;
    info!(acct = account.acct,  id = %account.id, name = account.display_name, "verified credentials");
    Ok(())
}

#[instrument(name = "home", skip_all, err)]
async fn show_timeline(client: &Mastodon) -> Result<()> {
    let mut timeline = load_home_timeline(client).await?;
    // log the initial page links
    log_page_links(&timeline);

    // intentionally load the previous page while we're at the first page to
    // check that the behavior of the page object doesn't dead-end at the
    // beginning. This should not fail, but it also should not update the page
    // links
    load_prev_page(&mut timeline).await?;
    // this should log the same as the initial page links
    log_page_links(&timeline);

    // moving to the next page should load the next page and update the page
    // links
    load_next_page(&mut timeline).await?;
    // this should log two different links
    log_page_links(&timeline);

    // this should move back to the initial page
    load_prev_page(&mut timeline).await?;
    // this should log the same as the initial page links
    log_page_links(&timeline);

    Ok(())
}

#[instrument(name = "initial", skip_all, err)]
async fn load_home_timeline(client: &Mastodon) -> Result<Page<Status>> {
    let timeline = client
        .get_home_timeline()
        .await
        .context("Couldn't get timeline")?;
    info!("loaded initial page of home timeline");
    for item in &timeline.initial_items {
        debug!(uri = %item.uri);
    }
    Ok(timeline)
}

#[instrument(name = "next_page", skip_all, err)]
async fn load_next_page(timeline: &mut Page<Status>) -> Result<()> {
    let url = timeline.next.clone().context("no next page")?;
    let page = timeline
        .next_page()
        .await
        .context("Couldn't get next page")?;
    info!(%url, "loaded next page");
    log_page_items(page);
    Ok(())
}

#[instrument(name = "prev_page", skip_all, err)]
async fn load_prev_page(timeline: &mut Page<Status>) -> Result<()> {
    let url = timeline.prev.clone().context("no prev page")?;
    let page = timeline
        .prev_page()
        .await
        .context("Couldn't get prev page")?;
    info!(%url, "loaded prev page");
    log_page_items(page);
    Ok(())
}

fn log_page_items(page: Option<Vec<Status>>) {
    page.map_or_else(
        || warn!("the page loaded successfully, but there is no data"),
        |items| {
            for item in items {
                debug!(uri = %item.uri);
            }
        },
    );
}

/// This exists because there was an issue with the way that the previous and
/// next pages were loaded when going to the previous page at the beginning or
/// the next page at the end.
fn log_page_links(page: &Page<Status>) {
    debug!(
        prev = page.prev.as_ref().map_or("None", |u| u.as_str()),
        next = page.next.as_ref().map_or("None", |u| u.as_str()),
        "page links"
    );
}
