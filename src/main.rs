use anyhow::{Context, Result};
use mastodon_async::helpers::toml;
use mastodon_async::page::Page;
use mastodon_async::prelude::Status;
use mastodon_async::registration::Registered;
use mastodon_async::{helpers, scopes::Scopes, Registration};
use mastodon_async::{Data, Mastodon};
use std::{
    fs::File,
    io::{self, BufRead, Write},
};
use tracing::subscriber;
use tracing::{error, info, metadata::LevelFilter};
use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, EnvFilter, Layer, Registry};

#[tokio::main]
async fn main() -> Result<()> {
    let file = File::create("logfile.json")?;
    let (non_blocking, _guard) = tracing_appender::non_blocking(file);
    let filter = EnvFilter::default()
        .add_directive("hyper=info".parse().unwrap())
        .add_directive("reqwest=info".parse().unwrap())
        .add_directive("mastodon_async=trace".parse().unwrap())
        .add_directive("info".parse().unwrap());
    let file_layer = fmt::layer()
        .json()
        .with_writer(non_blocking)
        .with_filter(filter);
    let stderr_layer = fmt::layer()
        .with_writer(io::stderr)
        .with_filter(LevelFilter::INFO);
    let subscriber = Registry::default().with(file_layer).with(stderr_layer);
    subscriber::set_global_default(subscriber).context("Couldn't set subscriber")?;

    if let Err(err) = run().await {
        error!(?err, "error");
    }
    Ok(())
}

async fn run() -> Result<()> {
    let mastodon = match load_credentials() {
        Ok(data) => Mastodon::from(data),
        Err(error) => {
            info!(?error, "No credentials found, registering");
            let server_name = get_server_name()?;
            let registration = register(server_name).await?;
            let mastodon = authenticate(registration).await?;
            save_credentials(&mastodon).await?;
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

fn load_credentials() -> Result<Data> {
    let path = xdg::BaseDirectories::with_prefix("spike-mastodon")
        .context("cannot open config folder")?
        .find_config_file("credentials.toml")
        .context("cannot find config file")?;
    let data = toml::from_file(path).context("cannot load file")?;
    Ok(data)
}

fn get_server_name() -> Result<String> {
    let mut stdout = io::stdout().lock();
    let mut stdin = io::stdin().lock();

    writeln!(&mut stdout, "Enter server name:").context("failed to write to stdout")?;
    stdout.flush().context("failed to flush stdout")?;

    let mut input = String::new();
    stdin
        .read_line(&mut input)
        .context("failed to read input")?;

    Ok(input.trim().to_string())
}

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

async fn verify_credentials(client: &Mastodon) -> Result<(), anyhow::Error> {
    let account = client
        .verify_credentials()
        .await
        .context("Couldn't get account")?;
    info!(?account, "verified credentials");
    Ok(())
}

async fn save_credentials(client: &Mastodon) -> Result<()> {
    let path = xdg::BaseDirectories::with_prefix("spike-mastodon")
        .context("cannot open config folder")?
        .place_config_file("credentials.toml")
        .context("cannot place config file")?;
    toml::to_file(&client.data, &path).with_context(|| format!("cannot save file {path:?}"))?;
    Ok(())
}

async fn get_home_timeline(client: &Mastodon) -> Result<Page<Status>> {
    let timeline = client
        .get_home_timeline()
        .await
        .context("Couldn't get timeline")?;
    info!("got timeline");
    Ok(timeline)
}

fn get_initial_items(timeline: &mut Page<Status>) {
    let items: Vec<String> = timeline
        .initial_items
        .clone()
        .into_iter()
        .map(|status| status.uri)
        .collect();
    info!(?items, "initial items");
}

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
