use std::{sync::OnceLock, time::Duration};

use axum::{
    extract::State,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use chrono::{DateTime, Utc};
use futures_util::{join, StreamExt, TryStreamExt};
use headers::{CacheControl, HeaderMapExt};
use lambda_http::{run, Error};
use rspotify::{
    clients::{BaseClient, OAuthClient},
    model::{AdditionalType, Context, Device, FullTrack, RepeatState, TimeLimits, TimeRange},
    scopes, AuthCodeSpotify, Credentials, Token,
};
use serde::Serialize;
use tower_http::cors::CorsLayer;
use tracing_subscriber::filter::{EnvFilter, LevelFilter};

#[derive(Debug, Clone)]
struct AppState {
    spotify: AuthCodeSpotify,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Data {
    short_term_top: Vec<SimpleTrack>,
    mid_term_top: Vec<SimpleTrack>,
    long_term_top: Vec<SimpleTrack>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SimpleArtist {
    name: String,
    url: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SimpleTrack {
    name: String,
    artists: Vec<SimpleArtist>,
    image_url: Option<String>,
    url: Option<String>,
    duration: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Playing {
    device: Device,
    context: Option<Context>,
    repeat: RepeatState,
    shuffled: bool,
    playing: SimpleTrack,
    progress_secs: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LastPlayed {
    track: SimpleTrack,
    context: Option<Context>,
    played_at: DateTime<Utc>,
}

static DATA_CACHE: OnceLock<Data> = OnceLock::new();

/// Gets data that doesn't change often: top tracks, etc.
async fn data(State(AppState { spotify }): State<AppState>) -> Result<Response, String> {
    let cache_header = CacheControl::new().with_max_age(Duration::from_secs(24 * 60 * 60));
    if let Some(data) = DATA_CACHE.get() {
        let mut res = Json(data).into_response();

        res.headers_mut().typed_insert(cache_header);

        return Ok(res);
    }

    let (short_term_full, mid_term_full, long_term_full) = join!(
        top_for_time_frame(&spotify, 10, TimeRange::ShortTerm),
        top_for_time_frame(&spotify, 10, TimeRange::MediumTerm),
        top_for_time_frame(&spotify, 10, TimeRange::LongTerm)
    );

    let short_term = short_term_full?
        .into_iter()
        .map(full_track_to_simple)
        .collect();
    let mid_term = mid_term_full?
        .into_iter()
        .map(full_track_to_simple)
        .collect();
    let long_term = long_term_full?
        .into_iter()
        .map(full_track_to_simple)
        .collect();

    DATA_CACHE
        .set(Data {
            short_term_top: short_term,
            mid_term_top: mid_term,
            long_term_top: long_term,
        })
        .ok();

    let mut res = Json(DATA_CACHE.get().unwrap()).into_response();

    res.headers_mut().typed_insert(cache_header);

    Ok(res)
}

async fn currently_playing(
    State(AppState { spotify }): State<AppState>,
) -> Result<Response, String> {
    let currently_playing = spotify
        .current_playback(None, Some([&AdditionalType::Track]))
        .await
        .map_err(|e| e.to_string())?;

    if let Some(currently_playing) = currently_playing {
        if currently_playing.is_playing {
            let full_track = match currently_playing.item.unwrap().id().unwrap() {
                rspotify::model::PlayableId::Track(track_id) => spotify
                    .track(track_id, None)
                    .await
                    .map_err(|e| e.to_string())?,
                rspotify::model::PlayableId::Episode(_) => {
                    unreachable!("Should never be playing an episode.")
                }
            };

            let cache_header = CacheControl::new().with_no_cache().with_no_store();

            let mut res = Json(Some(Playing {
                device: currently_playing.device,
                context: currently_playing.context,
                playing: full_track_to_simple(full_track),
                progress_secs: currently_playing.progress.unwrap().num_seconds() as u32,
                repeat: currently_playing.repeat_state,
                shuffled: currently_playing.shuffle_state,
            }))
            .into_response();
            res.headers_mut().typed_insert(cache_header);

            Ok(res)
        } else {
            Ok(Json(None::<Playing>).into_response())
        }
    } else {
        Ok(Json(None::<Playing>).into_response())
    }
}

async fn recently_played(State(AppState { spotify }): State<AppState>) -> Result<Response, String> {
    let mut recent = spotify
        .current_user_recently_played(
            Some(10),
            Some(TimeLimits::Before(chrono::offset::Utc::now())),
        )
        .await
        .map_err(|e| e.to_string())?
        .items;

    // Sort so that most recent is first
    recent.sort_unstable_by(|a, b| b.played_at.cmp(&a.played_at));

    let cache_header = CacheControl::new().with_max_age(Duration::from_secs(3 * 60));
    let mut res = Json(
        recent
            .into_iter()
            .map(|his| LastPlayed {
                track: full_track_to_simple(his.track),
                context: his.context,
                played_at: his.played_at,
            })
            .collect::<Vec<_>>(),
    )
    .into_response();

    res.headers_mut().typed_insert(cache_header);

    Ok(res)
}

async fn top_for_time_frame(
    spotify: &AuthCodeSpotify,
    num: usize,
    time_frame: TimeRange,
) -> Result<Vec<FullTrack>, String> {
    let top_stream = spotify.current_user_top_tracks(Some(time_frame));

    top_stream
        .take(num)
        .try_collect()
        .await
        .map_err(|e| e.to_string())
}

fn full_track_to_simple(full_track: FullTrack) -> SimpleTrack {
    SimpleTrack {
        name: full_track.name,
        artists: full_track
            .artists
            .into_iter()
            .map(|artist: rspotify::model::SimplifiedArtist| SimpleArtist {
                name: artist.name,
                url: artist.external_urls.get("spotify").cloned(),
            })
            .collect(),
        image_url: full_track
            .album
            .images
            .into_iter()
            .next()
            .map(|img| img.url),
        url: full_track.external_urls.get("spotify").cloned(),
        duration: full_track.duration.num_seconds() as u32,
    }
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
        .route("/recent", get(recently_played))
        .layer(CorsLayer::permissive())
        .with_state(AppState { spotify });

    run(app).await
}
