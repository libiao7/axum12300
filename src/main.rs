use axum::{Router, extract::Path, routing::get};
#[tokio::main]
async fn main() {
    let app = Router::new().route(
        "/download_poster/{product_number}/{video_poster}",
        get(
            |Path((pinfan, poster_url)): Path<(String, String)>| async move {
                let cover_path =
                    std::path::Path::new("C:/Users/aa/Desktop/download_poster").join(&pinfan);
                match std::fs::exists(&cover_path) {
                    Ok(true) => bytes::Bytes::from(std::fs::read(&cover_path).unwrap()),
                    Ok(false) => {
                        let bites = reqwest::get(poster_url)
                            .await
                            .unwrap()
                            .bytes()
                            .await
                            .unwrap();
                        std::io::copy(
                            &mut bites.as_ref(),
                            &mut std::fs::File::create_new(&cover_path).unwrap(),
                        )
                        .unwrap();
                        bites
                    }
                    Err(_) => reqwest::get(poster_url)
                        .await
                        .unwrap()
                        .bytes()
                        .await
                        .unwrap(),
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("0.0.0.0:12300")
        .await
        .unwrap();
    axum::serve(listener, app).await.unwrap();
}
