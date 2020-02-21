use actix_web::{get, middleware, web, App, HttpRequest, HttpServer};
use diesel::{r2d2::ConnectionManager, PgConnection};

use crate::game;

#[get("/health")]
async fn health(_: HttpRequest) -> &'static str {
    "ok"
}

pub async fn launch(db_pool: r2d2::Pool<ConnectionManager<PgConnection>>) -> std::io::Result<()> {
    HttpServer::new(move || {
        App::new()
            .data(db_pool.clone())
            .wrap(middleware::DefaultHeaders::new().header("X-Version", env!("CARGO_PKG_VERSION")))
            .wrap(middleware::Compress::default())
            .wrap(middleware::Logger::default())
            // limit the maximum amount of data that server will accept
            .data(web::JsonConfig::default().limit(4096))
            .data(web::PayloadConfig::default().limit(262_144))
            .service(health)
            .service(
                web::scope("/games")
                    .service(game::create_game)
                    .service(game::get_games),
            )
    })
    .bind("127.0.0.1:8080")?
    .run()
    .await
}
