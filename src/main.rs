use actix_web::http::StatusCode;
use actix_web::web::Data;
use actix_web::{middleware, post, web, App, HttpResponse, HttpServer, Responder};
use lru::LruCache;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::str::FromStr;
use std::sync::Mutex;

use serde_json::Value as JsonValue;
type JsonMap = serde_json::Map<String, serde_json::Value>;
type Snowflake = u64;
type Timestamp = chrono::DateTime<chrono::Utc>;

struct DedupNote {
    by_webhook: Mutex<LruCache<(Snowflake, String, String), ()>>,
}

#[derive(Deserialize)]
struct MiUser {
    name: Option<String>,
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
    // cw: Option<String>,
    user: MiUser,
    #[serde(default)]
    files: Vec<MiDriveFile>,
}

#[derive(Deserialize)]
struct MiAbuseReportPayload {
    //id: String,
    #[serde(rename = "targetUser")]
    target_user: Option<MiUser>,
    reporter: Option<MiUser>,
    comment: String,
}

// https://discord.com/developers/docs/resources/webhook#execute-webhook-jsonform-params
#[derive(Serialize)]
struct DiWebhookPayload {
    embeds: Vec<DiEmbed>,
    allowed_mentions: DiAllowedMentions,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
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

#[post("/discord/{webhook_id}/{webhook_token}/misskey")]
async fn misskey_to_discord(
    path: web::Path<(Snowflake, String)>,
    http_client: Data<Client>,
    dedup_note: Data<DedupNote>,
    payload: web::Json<JsonMap>,
) -> impl Responder {
    let (webhook_id, webhook_token) = path.into_inner();

    // see https://github.com/misskey-dev/misskey/pull/11752
    let Some(server) = payload.get("server").and_then(JsonValue::as_str) else {
        return HttpResponse::build(StatusCode::BAD_REQUEST).body(
            "No 'server' payload found. this proxy requires misskey 2023.9.0-beta.2 or later.",
        );
    };
    let server = server.trim_end_matches('/');

    // see https://misskey-hub.net/docs/features/webhook.html
    match payload.get("type").and_then(JsonValue::as_str) {
        None => return HttpResponse::build(StatusCode::BAD_REQUEST).body("type field not found"),
        Some(ty @ ("follow" | "followed" | "unfollow")) => {
            return HttpResponse::build(StatusCode::BAD_REQUEST)
                .body(format!("Unsupported event type: {ty}"))
        }
        Some("abuseReport") => {
            // simple note webhook
            proxy_abuse_report_to_webhook(&payload, &http_client, webhook_id, &webhook_token).await
        }
        Some("note" | "reply" | "mention" | "renote") => {
            // simple note webhook
            proxy_note_to_webhook(
                &payload,
                &http_client,
                &dedup_note,
                server,
                webhook_id,
                &webhook_token,
            )
            .await
        }
        Some(ty) if ty.starts_with("note@") => {
            // nirila extension: admin other user webhook
            proxy_note_to_webhook(
                &payload,
                &http_client,
                &dedup_note,
                server,
                webhook_id,
                &webhook_token,
            )
            .await
        }
        Some(unknown) => {
            return HttpResponse::build(StatusCode::BAD_REQUEST)
                .body(format!("Unknown event type: {unknown}"))
        }
    }
}

async fn proxy_note_to_webhook(
    payload: &JsonMap,
    http_client: &Client,
    dedup_note: &DedupNote,
    server: &str,
    webhook_id: Snowflake,
    webhook_token: &str,
) -> HttpResponse {
    let Some(note) = payload
        .get("body")
        .and_then(JsonValue::as_object)
        .and_then(|x| x.get("note"))
    else {
        return HttpResponse::build(StatusCode::BAD_REQUEST).body("webhokk payload not found");
    };

    let Ok(note) = serde_json::from_value::<MiNote>(note.clone()) else {
        return HttpResponse::build(StatusCode::BAD_REQUEST).body("webhokk payload parse error");
    };

    {
        // dedup notes
        let mut dedup_by_webhook = dedup_note.by_webhook.lock().unwrap();
        let result = dedup_by_webhook.push((webhook_id, server.to_string(), note.id.clone()), ());
        if result
            .map(|((hook_id, server, note_id), ())| {
                hook_id == webhook_id && server == server && note_id == note.id
            })
            .unwrap_or(false)
        {
            return HttpResponse::build(StatusCode::OK)
                .body("duplicated note so not sent to discord");
        }
    }

    let image = note.files.into_iter().find(|x| {
        matches!(
            x.r#type.as_str(),
            "image/jpeg" | "image/png" | "image/gif" | "image/webp"
        )
    });

    let di_image = image.map(|image| DiEmbedImage { url: image.url });

    let author_url = match note.user.host {
        None => format!("{server}/@{user_name}", user_name = note.user.username),
        Some(host) => format!(
            "{server}/@{user_name}@{host}",
            user_name = note.user.username
        ),
    };

    let embed = DiEmbed {
        title: format!(
            "{} (@{})",
            note.user.name.as_ref().unwrap_or(&note.user.username),
            note.user.username
        ),
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
        content: None,
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

        return HttpResponse::build(StatusCode::INTERNAL_SERVER_ERROR)
            .body("discord returns error");
    }

    HttpResponse::build(StatusCode::CREATED).body("successfully created")
}

async fn proxy_abuse_report_to_webhook(
    payload: &JsonMap,
    http_client: &Client,
    webhook_id: Snowflake,
    webhook_token: &str,
) -> HttpResponse {
    let Some(payload) = payload.get("body") else {
        println!("no body: ${payload:?}");
        return HttpResponse::build(StatusCode::BAD_REQUEST).body("webhokk payload not found");
    };

    let Ok(payload) = serde_json::from_value::<MiAbuseReportPayload>(payload.clone()) else {
        println!(
            "bad payload: {:?}",
            serde_json::from_value::<MiAbuseReportPayload>(payload.clone())
                .err()
                .unwrap()
        );
        return HttpResponse::build(StatusCode::BAD_REQUEST).body("webhokk payload parse error");
    };

    fn user_args(user: &Option<MiUser>) -> String {
        if let Some(user) = user {
            if let Some(host) = &user.host {
                format!("@{}@{}", user.username, host)
            } else {
                format!("@{}", user.username)
            }
        } else {
            "unknown_user".to_string()
        }
    }

    let content = format!(
        concat!(
            "New abuse report created!\n",
            "Reporter: {reporter}\n",
            "Target User: {target}\n",
            "Comment\n",
            "{comment}"
        ),
        reporter = user_args(&payload.reporter),
        target = user_args(&payload.target_user),
        comment = payload.comment,
    );

    let payload = DiWebhookPayload {
        embeds: vec![],
        // mentions disallowed
        allowed_mentions: DiAllowedMentions { parse: [] },
        content: Some(content),
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

        return HttpResponse::build(StatusCode::INTERNAL_SERVER_ERROR)
            .body("discord returns error");
    }

    HttpResponse::build(StatusCode::CREATED).body("successfully created")
}

static DEFAULT_USER_AGENT: &str = concat!(
    "misskey-discord-webhook-proxy/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/niri-la/misskey-discord-webhook-proxy)"
);

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));

    let mut args = std::env::args();
    let _exe_name = args.next();
    let addrs = args
        .map(|x| SocketAddr::from_str(&x))
        .collect::<Result<Vec<SocketAddr>, _>>()
        .expect("listen addr invalid");

    let client = Client::builder()
        .user_agent(
            std::env::var("USER_AGENT")
                .as_deref()
                .unwrap_or(DEFAULT_USER_AGENT),
        )
        .build()
        .expect("building http client");
    let http_client = Data::new(client);

    let dedup_note = DedupNote {
        by_webhook: Mutex::new(LruCache::new(NonZeroUsize::new(1024).unwrap())),
    };
    let dedup_note = Data::new(dedup_note);

    HttpServer::new(move || {
        App::new()
            // enable logger
            .wrap(middleware::Logger::default())
            .app_data(web::JsonConfig::default().limit(4096)) // <- limit size of the payload (global configuration)
            .app_data(http_client.clone())
            .app_data(dedup_note.clone())
            .service(misskey_to_discord)
    })
    .bind(addrs.as_slice())?
    .run()
    .await
}
