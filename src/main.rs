use actix_web::http::StatusCode;
use actix_web::web::Data;
use actix_web::{middleware, post, web, App, HttpResponse, HttpServer, Responder};
use reqwest::{Client, Request, Url};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use serde_json::Value as JsonValue;
type JsonMap = serde_json::Map<String, serde_json::Value>;
type Snowflake = u64;
type Timestamp = chrono::DateTime<chrono::Utc>;

#[derive(Deserialize)]
struct MiUser {
    name: String,
    username: String,
    host: Option<String>,
    #[serde(rename = "avatarUrl")]
    avatar_url: String,
}

#[derive(Deserialize)]
struct MiDriveFile {
    url: String,
    r#type: String,
}

#[derive(Deserialize)]
struct MiNote {
    id: String,
    #[serde(rename = "createdAt")]
    created_at: Timestamp,
    text: Option<String>,
    cw: Option<String>,
    user: MiUser,
    #[serde(default)]
    files: Vec<MiDriveFile>,
}

#[derive(Serialize)]
struct DiWebhookPayload {
    embeds: Vec<DiEmbed>,
    allowed_mentions: DiAllowedMentions,
}

#[derive(Serialize)]
struct DiAllowedMentions {
    parse: [&'static str; 0],
}

#[derive(Serialize)]
struct DiEmbed {
    // for webhook so except for type, provider, video, height, width, proxy_url
    title: String,
    description: String,
    url: String,
    timestamp: Timestamp,
    //color: u32,
    author: DiEmbedAuthor,
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<DiEmbedImage>,
}

#[derive(Serialize)]
struct DiEmbedAuthor {
    name: String,
    url: String,
    icon_url: String,
}

#[derive(Serialize)]
struct DiEmbedImage {
    url: String,
}

#[post("/{webhook_id}/{webhook_token}/misskey")]
async fn misskey_to_discord(
    path: web::Path<(Snowflake, String)>,
    http_client: Data<Client>,
    payload: web::Json<JsonMap>,
) -> impl Responder {
    let (webhook_id, webhook_token) = path.into_inner();

    // see https://misskey-hub.net/docs/features/webhook.html
    match payload.get("type").and_then(JsonValue::as_str) {
        None => return HttpResponse::build(StatusCode::BAD_REQUEST).body("type field not found"),
        Some(ty @ ("follow" | "followed" | "unfollow" | "reply" | "renote" | "mention")) => {
            return HttpResponse::build(StatusCode::BAD_REQUEST)
                .body(format!("Unsupported event type: {ty}"))
        }
        Some("note") => {
            // simple note webhook
        }
        Some(ty) if ty.starts_with("note@") => {
            // nirila extension: admin other user webhook
        }
        Some(unknown) => {
            return HttpResponse::build(StatusCode::BAD_REQUEST)
                .body(format!("Unknown event type: {unknown}"))
        }
    }

    // see https://github.com/misskey-dev/misskey/pull/11752
    let Some(server) = payload.get("server").and_then(JsonValue::as_str) else {
        return HttpResponse::build(StatusCode::BAD_REQUEST)
            .body("No 'server' payload found. this proxy requires misskey 2023.9.0-beta.2 or later.");
    };
    let server = server.trim_end_matches("/");

    let Some(note) = payload.get("body")
        .and_then(JsonValue::as_object)
        .and_then(|x| x.get("note")) else {
        return HttpResponse::build(StatusCode::BAD_REQUEST)
            .body("webhokk payload not found")
    };

    let Ok(note) = serde_json::from_value::<MiNote>(note.clone()) else {
        return HttpResponse::build(StatusCode::BAD_REQUEST)
            .body("webhokk payload parse error")
    };

    let image = note
        .files
        .into_iter()
        .filter(|x| {
            matches!(
                x.r#type.as_str(),
                "image/jpeg" | "image/png" | "image/gif" | "image/webp"
            )
        })
        .next();

    let di_image = image.map(|image| DiEmbedImage { url: image.url });

    let author_url = match note.user.host {
        None => format!("{server}/@{user_name}", user_name = note.user.username),
        Some(host) => format!(
            "{server}/@{user_name}@{host}",
            user_name = note.user.username
        ),
    };

    let embed = DiEmbed {
        title: format!("{} (@{})", note.user.name, note.user.username),
        description: note.text.unwrap_or(String::from("(no content)")),
        url: format!("{server}/notes/{note_id}", note_id = note.id),
        timestamp: note.created_at,
        image: di_image,
        author: DiEmbedAuthor {
            name: format!("@{}", note.user.username),
            url: author_url,
            icon_url: note.user.avatar_url,
        },
    };

    let payload = DiWebhookPayload {
        embeds: vec![embed],
        // mentions disallowed
        allowed_mentions: DiAllowedMentions { parse: [] },
    };

    let webhook_url = format!("https://discord.com/api/webhooks/{webhook_id}/{webhook_token}");

    let response = http_client
        .post(webhook_url)
        .json(&payload)
        .send()
        .await
        .expect("unexpected err");

    if response.status().is_client_error() || response.status().is_server_error() {
        log::error!(
            "error response from discord: {}",
            response.text().await.expect("unparseable error response")
        );

        return HttpResponse::build(StatusCode::INTERNAL_SERVER_ERROR).body("discord returns error");
    }

    HttpResponse::build(StatusCode::CREATED).body("successfully created")
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));

    // TODO: configure UA and else
    let http_client = Data::new(Client::new());

    HttpServer::new(move || {
        App::new()
            // enable logger
            .wrap(middleware::Logger::default())
            .app_data(web::JsonConfig::default().limit(4096)) // <- limit size of the payload (global configuration)
            .app_data(http_client.clone())
            .service(misskey_to_discord)
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
}
