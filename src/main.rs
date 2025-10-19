// use axum::{Router, extract::Path, routing::get};
// use deadpool_postgres::{Config, Pool, Runtime};
// use reqwest::Client;
// use serde::{Deserialize, Serialize};
// use std::fs;
// use std::io::copy;
// use std::path::Path;
// use std::process::Command;
use std::sync::Arc;
use tokio::sync::Semaphore;
// use tokio::task::JoinSet;
// use tokio_postgres::NoTls;

#[derive(Debug, serde::Serialize)]
struct Item {
    hash: String,
    title: String,
    dt: String,
    cat: String,
    size: Option<i64>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
struct ImageData {
    title: String,
    img_url_array: Vec<String>,
    page_url: String,
}

#[derive(serde::Deserialize, Debug)]
struct SearchRequest {
    titles: Vec<String>,
}
#[derive(Clone)]
struct AppState {
    pg_pool: deadpool_postgres::Pool,
    http_client: reqwest::Client,       // 共享的 reqwest::Client
    download_semaphore: Arc<Semaphore>, // 全局下载信号量
}

async fn get_items_batch_pq(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Json(search_request): axum::extract::Json<SearchRequest>,
) -> impl axum::response::IntoResponse {
    let client = state.pg_pool.get().await.unwrap();
    let titles = &search_request.titles;

    let mut query_str = String::from("SELECT hash, title, dt, cat, size FROM items WHERE ");
    for (index, _) in titles.iter().enumerate() {
        if index > 0 {
            query_str.push_str(" OR ");
        }
        query_str.push_str(&format!("lower(title) LIKE lower(${})", index + 1));
    }
    query_str.push_str(" ORDER BY title ASC LIMIT 10000");

    let stmt = client.prepare(&query_str).await.unwrap();
    let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = titles
        .iter()
        .map(|s| s as &(dyn tokio_postgres::types::ToSql + Sync))
        .collect();

    let rows = client.query(&stmt, &params.as_slice()).await.unwrap();
    let items: Vec<Item> = rows
        .iter()
        .map(|row| Item {
            hash: row.get(0),
            title: row.get(1),
            dt: row.get(2),
            cat: row.get(3),
            size: row.get(4),
        })
        .collect();

    axum::Json(items)
}

async fn handle_post(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Json(data): axum::extract::Json<ImageData>,
) -> impl axum::response::IntoResponse {
    let page_url = &data.page_url;
    let dir_path = std::path::Path::new("C:\\Users\\aa\\Desktop\\zup").join(&data.title);

    if !dir_path.exists() {
        std::fs::create_dir_all(&dir_path).expect("Failed to create directory");
    }

    let mut joinset = tokio::task::JoinSet::new();

    for (index, url) in data.img_url_array.iter().enumerate() {
        let file_name = format!("{:04}.jpg", index + 1);
        let file_path = dir_path.join(&file_name);
        let url = url.clone();
        let semaphore = state.download_semaphore.clone(); // 使用全局信号量
        let task_client = state.http_client.clone();
        joinset.spawn(async move {
            let _permit = semaphore.acquire_owned().await.unwrap();

            if file_path.exists() {
                return Ok("existed");
            }

            match download_image(task_client, &url, &file_path).await {
                Ok(_) => Ok("OK"),
                Err(e) => {
                    eprintln!("e: {}: {}", e, url);
                    Err(url)
                }
            }
        });
    }

    let total_count = data.img_url_array.len();
    let mut success_count = 0;
    let mut failed_urls = Vec::new();

    while let Some(res) = joinset.join_next().await {
        match res {
            Ok(Ok(t)) => {
                success_count += 1;
                println!("{t}: {total_count}---{success_count}");
            }
            Ok(Err(url)) => failed_urls.push(url),
            Err(e) => eprintln!("Task panicked: {:?}", e),
        }
    }
    println!("{}\n已完成！", &data.title);

    if !failed_urls.is_empty() {
        let html_content = format!(
            r#"<html><body><h1><a href="{}">{}</a></h1><ul>{}</ul></body></html>"#,
            page_url,
            &data.title,
            failed_urls
                .iter()
                .map(|url| format!("<li><a href=\"{}\">{}</a></li>", url, url))
                .collect::<Vec<_>>()
                .join("")
        );
        std::fs::write(dir_path.join("failed_downloads.html"), html_content)
            .expect("Failed to write HTML file");
    } else {
        let failed_file_path = dir_path.join("failed_downloads.html");
        if failed_file_path.exists() {
            std::fs::remove_file(failed_file_path).expect("Failed to delete failed_downloads.html");
        }
    }

    let _ =
        std::process::Command::new("C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe")
            .arg(dir_path.to_str().unwrap())
            .output();

    format!("{}\n已完成！", &data.title)
}

async fn download_image(
    client: reqwest::Client,
    url: &str,
    path: &std::path::Path,
) -> Result<(), String> {
    let response = client.get(url).send().await.map_err(|e| e.to_string())?;

    if !response.status().is_success() {
        return Err(format!("Failed to download image: {}", response.status()));
    }

    let content = response.bytes().await.map_err(|e| e.to_string())?;
    if content.is_empty() {
        return Err("Downloaded file is empty".to_string());
    }

    let mut file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    std::io::copy(&mut content.as_ref(), &mut file).map_err(|e| e.to_string())?;

    Ok(())
}

async fn init_pool() -> deadpool_postgres::Pool {
    let mut cfg = deadpool_postgres::Config::new();
    cfg.host = Some("localhost".to_string());
    cfg.user = Some("postgres".to_string());
    cfg.password = Some("4545".to_string());
    cfg.dbname = Some("rarbg".to_string());
    cfg.create_pool(
        Some(deadpool_postgres::Runtime::Tokio1),
        tokio_postgres::NoTls,
    )
    .unwrap()
}

#[tokio::main]
async fn main() {
    let pg_pool = init_pool().await;
    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("Failed to create HTTP client");

    let app_state = Arc::new(AppState {
        pg_pool,
        http_client,                                     // 共享的 reqwest::Client
        download_semaphore: Arc::new(Semaphore::new(6)), // 全局6个并发许可
    });
    let app = axum::Router::new()
        .route("/zup", axum::routing::post(handle_post))
        .route("/rarbg/batch_pq", axum::routing::post(get_items_batch_pq))
        .route(
            "/download_poster/{product_number}/{video_poster}",
            axum::routing::get(
                |axum::extract::Path((pinfan, poster_url)): axum::extract::Path<(
                    String,
                    String,
                )>| async move {
                    let cover_path =
                        std::path::Path::new("C:/Users/aa/Desktop/download_poster").join(&pinfan);
                    match std::fs::exists(&cover_path) {
                        Ok(true) => (
                            [(
                                axum::http::header::CONTENT_TYPE,
                                file_format::FileFormat::from_file(&cover_path)
                                    .unwrap()
                                    .media_type()
                                    .to_string(),
                            )],
                            bytes::Bytes::from(std::fs::read(&cover_path).unwrap()),
                        ),
                        Ok(false) => {
                            let resp = reqwest::get(&poster_url).await.unwrap();
                            let web_content_type = resp.headers()[reqwest::header::CONTENT_TYPE]
                                .to_str()
                                .unwrap()
                                .to_string();
                            let web_content_length = resp.headers()[reqwest::header::CONTENT_LENGTH]
                                .to_str()
                                .unwrap()
                                .to_string();
                            let bites = resp.bytes().await.unwrap();
                            if web_content_type.to_lowercase().starts_with("image/") && !(2733 > web_content_length.parse().unwrap() && poster_url.starts_with("https://awsimgsrc.dmm.co.jp/pics_dig/digital/video/")) {
                                std::io::copy(
                                    &mut bites.as_ref(),
                                    &mut std::fs::File::create_new(&cover_path).unwrap(),
                                )
                                .unwrap();
                            }
                            (
                                [(axum::http::header::CONTENT_TYPE, web_content_type)],
                                bites,
                            )
                        }
                        Err(_) => (
                            [(axum::http::header::CONTENT_TYPE, "image/jpeg".to_string())],
                            bytes::Bytes::new(),
                        ),
                    }
                },
            ),
        )
        .route(
            "/open_with_potplayer/{video_url}/{browser_user_agent}",
            axum::routing::get(|axum::extract::Path((v_url, ua)): axum::extract::Path<(
                    String,
                    String,
                )>| async move {
                let bat_path = "C:\\Users\\aa\\Desktop\\potplayer_with_user_agent.bat";
                std::fs::write(
                    bat_path,
                    format!(
                        "\"{}\" \"{}\" /user_agent=\"{}\"",
                        "C:\\Program Files\\DAUM\\PotPlayer\\PotPlayerMini64.exe", v_url, ua
                    )
                    .as_bytes(),
                )
                .unwrap();
                let _ = std::process::Command::new(bat_path).spawn();
            }),
        )
        .with_state(app_state);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:31343")
        .await
        .unwrap();
    axum::serve(listener, app).await.unwrap();
}
