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
// use tokio::fs::File;
use tokio::io::AsyncWriteExt;
// use std::env;

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
    dy_path: std::path::PathBuf,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
struct DouYinDownloadTask {
    url: String,
    file_name: String,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
struct DouYinDownloadReq {
    sec_uid: String, //网址 用于文件夹      MS4wLjABAAAA6Ks9K7OGdw7IlxnL1OlAAaGWnh9QIzmaPqQm985hNxU
    douyin_download_tasks: Vec<DouYinDownloadTask>,
    nickname: String,  //常变化的.昵称  用于aweme_json文件夹命名
    user_json: String, //__pace_f
}

async fn download_douyin_user_awemes(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Json(douyin_download_req): axum::extract::Json<DouYinDownloadReq>,
) -> impl axum::response::IntoResponse {
    let sec_uid = &douyin_download_req.sec_uid;
    let nickname = &douyin_download_req.nickname;
    let user_json = &douyin_download_req.user_json;
    let total_count = douyin_download_req.douyin_download_tasks.len();
    let dir_path = &state
        .dy_path
        .join(sanitize_windows_filename_strict(&sec_uid).unwrap());

    if !dir_path.exists() {
        tokio::fs::create_dir_all(dir_path)
            .await
            .expect("Failed to create directory");
    }

    let user_info_dir_path = &dir_path.join("user-info");
    if !(user_json.is_empty() || nickname.is_empty()) {
        tokio::fs::create_dir_all(user_info_dir_path)
            .await
            .expect("Failed to create directory");
    }
    let mut joinset = tokio::task::JoinSet::new();

    for douyin_download_task in douyin_download_req.douyin_download_tasks {
        let file_path = dir_path
            .join(sanitize_windows_filename_strict(&douyin_download_task.file_name).unwrap());
        let url = douyin_download_task.url;
        let semaphore = state.download_semaphore.clone(); // 使用全局信号量
        let task_client = state.http_client.clone();
        let task_pg_pool = state.pg_pool.clone();
        let user_info_dir_path_clone = user_info_dir_path.clone();
        let nickname_clone = nickname.clone();
        joinset.spawn(async move {
            let _permit = semaphore.acquire_owned().await.unwrap();
            let task_pg_connect=task_pg_pool.get().await.unwrap();
            // if file_path.exists() {
            if task_pg_connect
                .query_opt(
                    "SELECT 1 FROM douyin_download_history WHERE video_id = $1",
                    &[&file_path.to_string_lossy()],
                )
                .await
                .unwrap()
                .is_some()
            {
                return Ok("existed");
            }
            let response = match task_client
                .get(&url)
                .header("Referer", "https://www.douyin.com/")
                .send()
                .await
            {
                Ok(res) => res,
                Err(e) => return Err((e.to_string(), url)),
            };

            if !response.status().is_success() {
                return Err((
                    format!("!response.status().is_success(): {}", response.status()),
                    url,
                ));
            }

            let content = match response.bytes().await {
                Ok(bytes) => bytes,
                Err(e) => return Err((e.to_string(), url)),
            };
            if content.is_empty() {
                return Err((format!("content.is_empty()"), url));
            }
            if douyin_download_task.file_name == "avatar.jpeg" {
                let path1 = user_info_dir_path_clone.join(format!(
                    "{}@{}.jpeg",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis(),
                    sanitize_windows_filename_strict(&nickname_clone).unwrap()
                ));
                let path2 = user_info_dir_path_clone.join("avatar.jpeg");

                if let Err(e) = tokio::fs::write(&path1, &content).await {
                    return Err((e.to_string(), url));
                }
                if let Err(e) = tokio::fs::write(&path2, content).await {
                    return Err((e.to_string(), url));
                }
            } else {
                match tokio::fs::File::create_new(&file_path).await {
                    Ok(mut f) => {
                        //must& f.write_all(&content)
                        if let Err(e) = f.write_all(&content).await {
                            return Err((e.to_string(), url));
                        };
                        // 下载成功后，记得把这个标准 ID 存入数据库，防止下次重复
                        task_pg_connect.execute(
                            "INSERT INTO douyin_download_history (video_id) VALUES ($1) ON CONFLICT DO NOTHING",
                            &[&file_path.to_string_lossy()]
                        ).await.unwrap();
                    }
                    Err(e) => {
                        return Err((e.to_string(), url));
                    }
                }
            }

            Ok("OK")
        });
    }

    let mut success_count: u32 = 0;
    let mut failed_urls = Vec::new();

    while let Some(res) = joinset.join_next().await {
        match res {
            Ok(Ok(t)) => {
                success_count += 1;
                println!("{t}: {total_count}---{success_count}");
            }
            Ok(Err((err_info, url))) => {
                eprintln!("err_info: {err_info} : {url}");
                failed_urls.push(url);
            }
            Err(e) => eprintln!("Task panicked: {:?}", e),
        }
    }
    println!(
        "{} : 作品下载已完成！",
        if nickname.is_empty() {
            sec_uid
        } else {
            nickname
        }
    );
    if failed_urls.is_empty() {
        let failed_file_path = dir_path.join("failed_downloads.html");
        if failed_file_path.exists() {
            tokio::fs::remove_file(failed_file_path)
                .await
                .expect("Failed to delete failed_downloads.html");
        }
        if !(user_json.is_empty() || nickname.is_empty()) {
            tokio::fs::write(user_info_dir_path.join("user.json"), user_json)
                .await
                .unwrap();
            tokio::fs::write(
                user_info_dir_path.join(format!(
                    "{}@{}.json",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis(),
                    sanitize_windows_filename_strict(nickname).unwrap()
                )),
                user_json,
            )
            .await
            .unwrap();
            println!("{nickname} : user.json 完成");
        } else {
            println!("user_json.is_empty() || nickname.is_empty()");
        }
    } else {
        let html_content = format!(
            r#"<html><body><h1><a href="https://www.douyin.com/user/{}">{}</a></h1><ul>{}</ul></body></html>"#,
            sec_uid,
            if nickname.is_empty() {
                "nickname.is_empty()"
            } else {
                nickname
            },
            failed_urls
                .iter()
                .map(|url| format!("<li><a href=\"{}\">{}</a></li>", url, url))
                .collect::<Vec<_>>()
                .join("")
        );
        tokio::fs::write(dir_path.join("failed_downloads.html"), html_content)
            .await
            .expect("Failed to write HTML file");
    }

    std::process::Command::new("C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe")
        .arg(dir_path.to_str().unwrap())
        .spawn()
        .unwrap();

    format!(
        "{} : 已完成！",
        if nickname.is_empty() {
            "nickname.is_empty()"
        } else {
            nickname
        }
    )
}

fn sanitize_windows_filename_strict(filename: &str) -> Result<String, &str> {
    // 1. 替换非法字符
    let sanitized: String = filename
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' | '\u{0000}'..='\u{001F}' => '-',
            _ => c,
        })
        .collect();

    // 2. 去除末尾的空格和点
    let trimmed = sanitized.trim_end_matches(|c| c == ' ' || c == '.');

    // 3. 如果结果为空（例如输入全是合法但被 trim 掉的字符），提供一个默认值
    if trimmed.is_empty() {
        return Err("empty now");
    }
    Ok(trimmed.to_string())
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
    let dir_path = &std::path::Path::new("C:\\Users\\aa\\Desktop\\zup").join(&data.title);

    if !dir_path.exists() {
        tokio::fs::create_dir_all(dir_path)
            .await
            .expect("Failed to create directory");
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

            match download_image(&task_client, &url, &file_path).await {
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
    println!("{} : 已完成！", &data.title);

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
        tokio::fs::write(dir_path.join("failed_downloads.html"), html_content)
            .await
            .expect("Failed to write HTML file");
    } else {
        let failed_file_path = dir_path.join("failed_downloads.html");
        if failed_file_path.exists() {
            tokio::fs::remove_file(failed_file_path)
                .await
                .expect("Failed to delete failed_downloads.html");
        }
    }

    std::process::Command::new("C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe")
        .arg(dir_path.to_str().unwrap())
        .spawn()
        .unwrap();

    format!("{} : 已完成！", &data.title)
}

async fn download_image(
    client: &reqwest::Client,
    url: &str,
    path: &std::path::Path,
) -> Result<(), String> {
    let mut request_builder = client.get(url);
    if let Ok(parsed_url) = reqwest::Url::parse(url) {
        if let Some(host) = parsed_url.host_str() {
            request_builder =
                request_builder.header("Referer", format!("{}://{}/", parsed_url.scheme(), host));
        }
    }
    let response = request_builder.send().await.map_err(|e| e.to_string())?;

    if !response.status().is_success() {
        return Err(format!("Failed to download image: {}", response.status()));
    }

    let content = response.bytes().await.map_err(|e| e.to_string())?;
    if content.is_empty() {
        return Err("Downloaded file is empty".to_string());
    }
    tokio::fs::File::create_new(path)
        .await
        .map_err(|e| e.to_string())?
        .write_all(&content)
        .await
        .map_err(|e| e.to_string())?;

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

    // 执行建表语句
    // 使用 TEXT PRIMARY KEY 确保无论文件名多长都能存入
    pg_pool
        .get()
        .await
        .unwrap()
        .batch_execute(
            "CREATE TABLE IF NOT EXISTS douyin_download_history (
            video_id TEXT PRIMARY KEY,
            created_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
        );",
        )
        .await
        .unwrap();
    let user_ostr = &std::env::var_os("USERPROFILE").unwrap();
    let dy_path = std::path::PathBuf::from(user_ostr).join("d-y");
    tokio::fs::create_dir_all(&dy_path)
        .await
        .expect("Failed to create directory");
    sync_one_level_subfolders(&pg_pool, &dy_path).await.unwrap();
    pub async fn sync_one_level_subfolders(
        pool: &deadpool_postgres::Pool,
        root_path: &std::path::PathBuf,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let pg_connect = pool.get().await?;
        let mut all_filenames = Vec::new();

        println!("正在扫描一级子文件夹: {:#?} ...", root_path);

        // .min_depth(2): 跳过根目录(1级)和子文件夹目录本身(1级)
        // .max_depth(2): 限制只查到子文件夹内的文件(2级)
        for entry in walkdir::WalkDir::new(root_path)
            .min_depth(2)
            .max_depth(2)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            // 确保只处理文件
            if entry.file_type().is_file() {
                if let Some(full_path) = entry.path().to_str() {
                    all_filenames.push(full_path.to_string());
                }
            }
        }

        if all_filenames.is_empty() {
            println!("未发现符合层级要求的文件。");
            return Ok(());
        }

        // 批量插入逻辑保持不变，依然高效
        let stmt = "INSERT INTO douyin_download_history (video_id) 
                SELECT * FROM UNNEST($1::text[]) 
                ON CONFLICT (video_id) DO NOTHING";

        let refs: Vec<&str> = all_filenames.iter().map(|s| s.as_str()).collect();
        for chunk in refs.chunks(10000) {
            pg_connect.execute(stmt, &[&chunk]).await?;
        }

        println!("✅ 成功同步 {} 个文件记录。", all_filenames.len());
        Ok(())
    }

    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("Failed to create HTTP client");

    let app_state = Arc::new(AppState {
        pg_pool,
        http_client,                                     // 共享的 reqwest::Client
        download_semaphore: Arc::new(Semaphore::new(6)), // 全局6个并发许可
        dy_path,
    });
    let app = axum::Router::new()
        .route("/zup", axum::routing::post(handle_post))
        .route("/download_douyin_user_awemes", axum::routing::post(download_douyin_user_awemes))
        .route("/rarbg/batch_pq", axum::routing::post(get_items_batch_pq))
        .route(
            "/download_poster/{override}/{product_number}/{video_poster}",
            axum::routing::get(
                |axum::extract::Path((re_write,pinfan, poster_url)): axum::extract::Path<(
                    bool,
                    String,
                    String,
                )>| async move {
                    let cover_path =
                        std::path::Path::new("C:/Users/aa/Desktop/download_poster").join(&pinfan);
                    match (std::fs::exists(&cover_path),re_write) {
                        (Ok(true),false) => (
                            [(
                                axum::http::header::CONTENT_TYPE,
                                file_format::FileFormat::from_file(&cover_path)
                                    .unwrap()
                                    .media_type()
                                    .to_string(),
                            )],
                            bytes::Bytes::from(tokio::fs::read(&cover_path).await.unwrap()),
                        ),
                        (Ok(existing),_) => {
                            let resp = reqwest::get(&poster_url).await.unwrap();
                            let web_content_type = resp.headers().get(reqwest::header::CONTENT_TYPE).expect(&poster_url)
                                .to_str()
                                .unwrap()
                                .to_string();
                            let web_content_length = resp.headers().get(reqwest::header::CONTENT_LENGTH).expect(&poster_url)
                                .to_str()
                                .unwrap()
                                .to_string();
                            let bites = resp.bytes().await.unwrap();
                            if web_content_type.to_lowercase().starts_with("image/") && !(2733 > web_content_length.parse().unwrap() && poster_url.starts_with("https://awsimgsrc.dmm.co.jp/pics_dig/digital/video/"))&&!(existing&&(&poster_url).starts_with("https://awsimgsrc.dmm.co.jp/pics_dig/digital/video/")&&tokio::fs::read(&cover_path).await.unwrap().len()==web_content_length.parse::<usize>().unwrap()) {
                                tokio::fs::write(&cover_path,&bites).await.unwrap();
                            }
                            (
                                [(axum::http::header::CONTENT_TYPE, web_content_type)],
                                bites,
                            )
                        }
                        (Err(_),_) => (
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
                use std::os::windows::process::CommandExt;
                let _ = std::process::Command::new(r"C:\Program Files\DAUM\PotPlayer\PotPlayerMini64.exe")
                .arg(v_url)
                .raw_arg(format!(r#"/user_agent="{ua}""#))
                .spawn();
            }),
        )
        .route(
            "/open_with_vlc/{video_url}/{browser_user_agent}",
            axum::routing::get(|axum::extract::Path((v_url, ua)): axum::extract::Path<(
                    String,
                    String,
                )>| async move {
                use std::os::windows::process::CommandExt;
                let _ = std::process::Command::new(r"C:\Program Files\VideoLAN\VLC\vlc.exe")
                .arg(v_url)
                .raw_arg(format!(r#":http-user-agent="{ua}""#))
                .arg("--no-bluray-menu")
                .spawn();
            }),
        )
        .with_state(app_state);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:31343")
        .await
        .unwrap();
    axum::serve(listener, app).await.unwrap();
}
