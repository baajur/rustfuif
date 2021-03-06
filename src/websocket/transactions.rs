use std::time::{Duration, Instant};

use actix::prelude::*;
use actix_identity::Identity;
use actix_web::web::{Data, Path};
use actix_web::{web, Error, HttpRequest, HttpResponse};
use actix_web_actors::ws;

use crate::auth;
use crate::db;
use crate::games::Game;
use crate::websocket::server;

/// How often heartbeat pings are sent
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
/// How long before lack of client response causes a timeout
const CLIENT_TIMEOUT: Duration = Duration::from_secs(10);

/// Entry point for our route
pub async fn route(
    req: HttpRequest,
    stream: web::Payload,
    srv: Data<Addr<server::TransactionServer>>,
    game_id: Path<i64>,
    id: Identity,
    pool: Data<db::Pool>,
) -> Result<HttpResponse, Error> {
    let user = auth::get_user(&id)?;

    let game_id = *game_id;

    web::block(move || {
        let conn = pool.get()?;
        if !Game::verify_user(game_id, user.id, &conn)? {
            forbidden!("you are not in this game");
        }
        Ok(())
    })
    .await?;

    ws::start(
        SalesUpdates {
            id: 0,
            hb: Instant::now(),
            game_id,
            addr: srv.get_ref().clone(),
        },
        &req,
        stream,
    )
}

struct SalesUpdates {
    /// unique session id
    id: usize,
    /// Client must send ping at least once per 10 seconds (CLIENT_TIMEOUT),
    /// otherwise we drop connection.
    hb: Instant,
    /// joined room
    game_id: i64,
    /// Chat server
    addr: Addr<server::TransactionServer>,
}

impl Actor for SalesUpdates {
    type Context = ws::WebsocketContext<Self>;

    /// Method is called on actor start.
    /// We register ws session with ChatServer
    fn started(&mut self, ctx: &mut Self::Context) {
        // we'll start heartbeat process on session start.
        self.hb(ctx);

        // register self in chat server. `AsyncContext::wait` register
        // future within context, but context waits until this future resolves
        // before processing any other events.
        // HttpContext::state() is instance of WsChatSessionState, state is shared
        // across all routes within application
        let addr = ctx.address();
        self.addr
            .send(server::Connect {
                addr: addr.recipient(),
                game_id: self.game_id,
            })
            .into_actor(self)
            .then(|res, act, ctx| {
                match res {
                    Ok(res) => act.id = res,
                    // something is wrong with chat server
                    _ => ctx.stop(),
                }
                fut::ready(())
            })
            .wait(ctx);
    }

    fn stopping(&mut self, _: &mut Self::Context) -> Running {
        // notify server
        self.addr.do_send(server::Disconnect { id: self.id });
        Running::Stop
    }
}

/// Handle messages from server, we simply send it to peer websocket
impl Handler<server::Sale> for SalesUpdates {
    type Result = ();

    fn handle(&mut self, sale: server::Sale, ctx: &mut Self::Context) {
        ctx.text(serde_json::to_string(&sale).unwrap_or_default());
    }
}

/// WebSocket message handler
impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for SalesUpdates {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        let msg = match msg {
            Err(_) => {
                ctx.stop();
                return;
            }
            Ok(msg) => msg,
        };

        trace!("Websocket received message: {:?}", msg);
        match msg {
            ws::Message::Ping(msg) => {
                self.hb = Instant::now();
                ctx.pong(&msg);
            }
            ws::Message::Pong(_) => {
                self.hb = Instant::now();
            }
            ws::Message::Text(_) => {
                debug!("ignoring incoming messages for now");
            }
            ws::Message::Binary(_) => debug!("Unexpected binary"),
            ws::Message::Close(reason) => {
                ctx.close(reason);
                ctx.stop();
            }
            ws::Message::Continuation(_) => {
                ctx.stop();
            }
            ws::Message::Nop => (),
        }
    }
}

impl SalesUpdates {
    /// helper method that sends ping to client every second.
    ///
    /// also this method checks heartbeats from client
    fn hb(&self, ctx: &mut ws::WebsocketContext<Self>) {
        ctx.run_interval(HEARTBEAT_INTERVAL, |act, ctx| {
            // check client heartbeats
            if Instant::now().duration_since(act.hb) > CLIENT_TIMEOUT {
                // heartbeat timed out
                error!("Websocket Client heartbeat failed, disconnecting!");

                // notify chat server
                act.addr.do_send(server::Disconnect { id: act.id });

                // stop actor
                ctx.stop();

                // don't try to send a ping
                return;
            }

            ctx.ping(b"");
        });
    }
}
