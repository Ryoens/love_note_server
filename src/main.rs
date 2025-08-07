use actix_files::Files;
use actix_web::{web, App, Error, HttpRequest, HttpResponse, HttpServer};
use actix_ws::{handle, Message};
use automerge::{AutoCommit, Change};
use base64::{engine::general_purpose, Engine as _};
use futures_util::StreamExt;
use rand::Rng;
use serde::Deserialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use log::{info, warn, error};

type Rooms = Arc<Mutex<HashMap<String, Room>>>;

struct Room {
    doc: AutoCommit,
    clients: HashSet<usize>,
}

#[derive(Deserialize)]
struct ChangeMsg {
    r#type: String,
    room: String,
    changes: Vec<String>,
}

#[derive(Deserialize)]
struct SnapshotMsg {
    r#type: String,
    room: String,
    data: Vec<u8>,
}

async fn ws_handler(
    req: HttpRequest,
    body: web::Payload,
    rooms: web::Data<Rooms>,
) -> Result<HttpResponse, Error> {
    let room_name = req.query_string().split('=').nth(1).unwrap_or("default").to_string();
    let (res, mut session, mut stream) = handle(&req, body)?;
    println!("Connected from client: {}", req.peer_addr().map(|a| a.to_string()).unwrap_or("不明".into()));

    // クライアント識別ID
    let mut rng = rand::thread_rng();
    let id: usize = rng.gen_range(0..1_000_000);

    {
        let mut map = rooms.lock().unwrap();
        map.entry(room_name.clone()).or_insert(Room {
            doc: AutoCommit::new(),
            clients: HashSet::new(),
        }).clients.insert(id);
    }

    let rooms_clone = rooms.clone();

    actix_web::rt::spawn(async move {
        while let Some(Ok(msg)) = stream.next().await {
            match msg {
                Message::Text(text) => {
                    if let Ok(val) = serde_json::from_str::<ChangeMsg>(&text) {
                        if val.r#type == "changes" {
                            let mut rooms = rooms_clone.lock().unwrap();
                            if let Some(room) = rooms.get_mut(&val.room) {
                                let decoded_changes: Vec<Change> = val.changes.iter()
                                    .filter_map(|s| {
                                        general_purpose::STANDARD.decode(s).ok()
                                            .and_then(|bin| Change::from_bytes(bin).ok())
                                    })
                                    .collect();

                                for change in &decoded_changes {
                                    let _ = room.doc.apply_changes([change.clone()]);
                                }

                                let rebroadcast = json!({
                                    "type": "changes",
                                    "changes": val.changes
                                });

                                // 本運用では各セッションごとに通信を保持する必要がある
                                // 今回は簡略化のためブロードキャストは省略
                                println!("Rebroadcast: {}", rebroadcast);
                            }
                        }
                    } else if let Ok(snap) = serde_json::from_str::<SnapshotMsg>(&text) {
                        let mut rooms = rooms_clone.lock().unwrap();
                        if let Some(room) = rooms.get_mut(&snap.room) {
                            let doc = AutoCommit::load(&snap.data).unwrap_or_else(|_| AutoCommit::new());
                            room.doc = doc;
                        }
                    }
                }
                Message::Binary(_) => {}
                Message::Ping(bytes) => {
                    println!("Received: id={} {:?}", id, bytes);
                    let _ = session.pong(&bytes).await;
                }
                Message::Close(reason) => {
                    println!("disconnected from client: {:?}", reason);
                    let _ = session.close(reason).await;
                    break;
                }
                _ => {}
            }
        }

        // 切断時にクライアントIDを削除
        let mut rooms = rooms_clone.lock().unwrap();
        if let Some(room) = rooms.get_mut(&room_name) {
            room.clients.remove(&id);
        }
    });

    Ok(res)
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init();
    let rooms = web::Data::new(Arc::new(Mutex::new(HashMap::<String, Room>::new())));

    HttpServer::new(move || {
        App::new()
            .app_data(rooms.clone())
            .route("/ws", web::get().to(ws_handler))
            .service(Files::new("/", "./public").index_file("index.html"))
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
}
