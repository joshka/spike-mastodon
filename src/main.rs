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
use tracing::instrument;
use tracing::{error, info, metadata::LevelFilter};
use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, EnvFilter, Layer};

#[tokio::main]
async fn main() -> Result<()> {
    let json_filter = EnvFilter::default()
        .add_directive("hyper=info".parse()?)
        .add_directive("reqwest=info".parse()?)
        .add_directive("mastodon_async=trace".parse()?)
        .add_directive("info".parse()?);
    let json_file = File::create("logfile.json")?;
    let (json_writer, _json_guard) = tracing_appender::non_blocking(json_file);
    let json_layer = fmt::layer()
        .json()
        .with_writer(json_writer)
        .with_filter(json_filter);

    let txt_filter = EnvFilter::default()
        .add_directive("hyper=info".parse()?)
        .add_directive("reqwest=info".parse()?)
        .add_directive("mastodon_async=trace".parse()?)
        .add_directive("info".parse()?);
    let txt_file = File::create("logfile.txt")?;
    let (txt_writer, _guard) = tracing_appender::non_blocking(txt_file);
    let txt_layer = fmt::layer().with_writer(txt_writer).with_filter(txt_filter);

    let stderr_layer = fmt::layer()
        .with_writer(io::stderr)
        .with_filter(LevelFilter::INFO);

    tracing_subscriber::registry()
        .with(json_layer)
        .with(txt_layer)
        .with(stderr_layer)
        .init();

    if let Err(err) = run().await {
        error!(?err, "error");
    }
    Ok(())
}

#[instrument(err, ret)]
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
    let mut timeline = get_home_timeline(&mastodon).await?;
    get_initial_items(&mut timeline);
    get_next_page(&mut timeline).await?;
    get_next_page(&mut timeline).await?;
    get_next_page(&mut timeline).await?;
    get_prev_page(&mut timeline).await?;

    Ok(())
}

#[instrument(err, ret)]
fn load_credentials() -> Result<Data> {
    let path = config_folder()?.join("credentials.toml");
    let data = toml::from_file(&path).with_context(|| format!("cannot load file {path:?}"))?;
    Ok(data)
}

#[instrument(err, ret)]
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

#[instrument(err, ret)]
async fn register(server_name: String) -> Result<Registered> {
    let registration = Registration::new(server_name)
        .client_name("joshka-mastodon-async")
        .redirect_uris("urn:ietf:wg:oauth:2.0:oob")
        .scopes(Scopes::read_all())
        .website("https://github.com/joshka/mastodon-async")
        .build()
        .await
        .context("Couldn't register app")?;
    info!(?registration, "registration complete");
    Ok(registration)
}

#[instrument(err, ret)]
async fn authenticate(registration: Registered) -> Result<Mastodon> {
    let url = registration
        .authorize_url()
        .context("Couldn't get authorize URL")?;
    webbrowser::open(&url).context("opening browser")?;
    let client = helpers::cli::authenticate(registration)
        .await
        .context("Couldn't authenticate")?;
    info!(?client.data, "authenticated");
    Ok(client)
}

#[instrument(err, ret)]
async fn verify_credentials(client: &Mastodon) -> Result<(), anyhow::Error> {
    let account = client
        .verify_credentials()
        .await
        .context("Couldn't get account")?;
    info!(?account, "verified credentials");
    Ok(())
}

#[instrument(err, ret)]
async fn get_home_timeline(client: &Mastodon) -> Result<Page<Status>> {
    let timeline = client
        .get_home_timeline()
        .await
        .context("Couldn't get timeline")?;
    info!("got timeline");
    Ok(timeline)
}

#[instrument]
fn get_initial_items(timeline: &mut Page<Status>) {
    let items: Vec<String> = timeline
        .initial_items
        .clone()
        .into_iter()
        .map(|status| status.uri)
        .collect();
    info!(?items, "initial items");
}

#[instrument(err, ret)]
async fn get_next_page(timeline: &mut Page<Status>) -> Result<()> {
    let items: Vec<String> = timeline
        .next_page()
        .await
        .context("Couldn't get next page")?
        .unwrap_or_default()
        .into_iter()
        .map(|status| status.uri)
        .collect();
    info!(?items, "next items");
    Ok(())
}

#[instrument(err, ret)]
async fn get_prev_page(timeline: &mut Page<Status>) -> Result<()> {
    let items: Vec<String> = timeline
        .prev_page()
        .await
        .context("Couldn't get prev page")?
        .unwrap_or_default()
        .into_iter()
        .map(|status| status.uri)
        .collect();
    info!(?items, "prev items");
    Ok(())
}
