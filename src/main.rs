use axum::{extract::State, routing::get, Router};
use futures_util::{StreamExt, TryStreamExt};
use lambda_http::{run, service_fn, Body, Error, Request, Response};
use rspotify::{
    clients::{BaseClient, OAuthClient},
    scopes, AuthCodeSpotify, Credentials, Token,
};
use tower_http::cors::CorsLayer;
use tracing_subscriber::filter::{EnvFilter, LevelFilter};

#[derive(Debug, Clone)]
struct AppState {
    spotify: AuthCodeSpotify,
}

/// This is the main body for the function.
/// Write your code inside it.
/// There are some code example in the following URLs:
/// - https://github.com/awslabs/aws-lambda-rust-runtime/tree/main/examples
async fn data(State(AppState { spotify }): State<AppState>) -> Result<(), String> {
    let playing_stream = spotify.current_user_top_tracks(None);

    let playing = playing_stream
        .try_collect::<Vec<_>>()
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}

async fn currently_playing(State(AppState { spotify }): State<AppState>) -> Result<(), String> {
    let currently_playing = spotify
        .current_playback(None, Some([]))
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        // disable printing the name of the module in every log line.
        .with_target(false)
        // disabling time is handy because CloudWatch will add the ingestion time.
        .without_time()
        .init();
    dotenvy::dotenv().ok();

    let mut spotify = AuthCodeSpotify::from_token(Token {
        refresh_token: Some(std::env::var("REFRESH_TOKEN").unwrap()),
        scopes: scopes!(
            "user-read-currently-playing",
            "user-read-recently-played",
            "user-top-read",
            "user-read-playback-position",
            "user-read-playback-state"
        ),
        ..Default::default()
    });
    spotify.creds = Credentials::from_env().unwrap();

    spotify.refresh_token().await.unwrap();

    let app = Router::new()
        .route("/", get(data))
        .route("/playing", get(currently_playing))
        .layer(CorsLayer::permissive())
        .with_state(AppState { spotify });

    run(app).await
}
