use futures_util::{
    future::ready,
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use log::debug;
use pinky_swear::{Pinky, PinkySwear};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    connect_async, tungstenite::protocol::Message, MaybeTlsStream, WebSocketStream,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandRequest {
    id: u8,
    r#type: String,
    uri: String,
    payload: Option<Value>,
}

pub enum Command {
    CreateToast(String),
    OpenBrowser(String),
    TurnOff,
    SetChannel(String),
    SetInput(String),
    SetMute(bool),
    SetVolume(i8),
    GetChannelList,
    GetCurrentChannel,
    OpenChannel(String),
    GetExternalInputList,
    SwitchInput(String),
    IsMuted,
    GetVolume,
    PlayMedia,
    StopMedia,
    PauseMedia,
    RewindMedia,
    ForwardMedia,
    ChannelUp,
    ChannelDown,
    Turn3DOn,
    Turn3DOff,
    GetServicesList,
}
pub struct CommandResponse {
    pub id: u8,
    pub payload: Option<Value>,
}

static HANDSHAKE: &'static str = r#"
{
    "type": "register",
    "id": "register_0",
    "payload": {
        "forcePairing": false,
        "pairingType": "PROMPT",
        "client-key": "694552d52cbf3baca53ba60e7d71a067",
        "manifest": {
            "manifestVersion": 1,
            "appVersion": "1.1",
            "signed": {
                "created": "20140509",
                "appId": "com.lge.test",
                "vendorId": "com.lge",
                "localizedAppNames": {
                    "": "LG Remote App",
                    "ko-KR": "리모컨 앱",
                    "zxx-XX": "ЛГ Rэмotэ AПП"
                },
                "localizedVendorNames": {
                    "": "LG Electronics"
                },
                "permissions": [
                    "TEST_SECURE",
                    "CONTROL_INPUT_TEXT",
                    "CONTROL_MOUSE_AND_KEYBOARD",
                    "READ_INSTALLED_APPS",
                    "READ_LGE_SDX",
                    "READ_NOTIFICATIONS",
                    "SEARCH",
                    "WRITE_SETTINGS",
                    "WRITE_NOTIFICATION_ALERT",
                    "CONTROL_POWER",
                    "READ_CURRENT_CHANNEL",
                    "READ_RUNNING_APPS",
                    "READ_UPDATE_INFO",
                    "UPDATE_FROM_REMOTE_APP",
                    "READ_LGE_TV_INPUT_EVENTS",
                    "READ_TV_CURRENT_TIME"
                ],
                "serial": "2f930e2d2cfe083771f68e4fe7bb07"
            },
            "permissions": [
                "LAUNCH",
                "LAUNCH_WEBAPP",
                "APP_TO_APP",
                "CLOSE",
                "TEST_OPEN",
                "TEST_PROTECTED",
                "CONTROL_AUDIO",
                "CONTROL_DISPLAY",
                "CONTROL_INPUT_JOYSTICK",
                "CONTROL_INPUT_MEDIA_RECORDING",
                "CONTROL_INPUT_MEDIA_PLAYBACK",
                "CONTROL_INPUT_TV",
                "CONTROL_POWER",
                "READ_APP_STATUS",
                "READ_CURRENT_CHANNEL",
                "READ_INPUT_DEVICE_LIST",
                "READ_NETWORK_STATE",
                "READ_RUNNING_APPS",
                "READ_TV_CHANNEL_LIST",
                "WRITE_NOTIFICATION_TOAST",
                "READ_POWER_STATE",
                "READ_COUNTRY_INFO"
            ],
            "signatures": [
                {
                    "signatureVersion": 1,
                    "signature": "eyJhbGdvcml0aG0iOiJSU0EtU0hBMjU2Iiwia2V5SWQiOiJ0ZXN0LXNpZ25pbmctY2VydCIsInNpZ25hdHVyZVZlcnNpb24iOjF9.hrVRgjCwXVvE2OOSpDZ58hR+59aFNwYDyjQgKk3auukd7pcegmE2CzPCa0bJ0ZsRAcKkCTJrWo5iDzNhMBWRyaMOv5zWSrthlf7G128qvIlpMT0YNY+n/FaOHE73uLrS/g7swl3/qH/BGFG2Hu4RlL48eb3lLKqTt2xKHdCs6Cd4RMfJPYnzgvI4BNrFUKsjkcu+WD4OO2A27Pq1n50cMchmcaXadJhGrOqH5YmHdOCj5NSHzJYrsW0HPlpuAx/ECMeIZYDh6RMqaFM2DXzdKX9NmmyqzJ3o/0lkk/N97gfVRLW5hA29yeAwaCViZNCP8iC9aO0q9fQojoa7NQnAtw=="
                }
            ]
        }
    }
}
"#;

pub struct WebosClient {
    write: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    registered: Arc<Mutex<bool>>,
    next_command_id: Arc<Mutex<u8>>,
    pending_requests: Arc<Mutex<HashMap<u8, Pinky<CommandResponse>>>>,
}

impl WebosClient {
    pub async fn new(address: &str) -> Result<WebosClient, String> {
        let url = url::Url::parse(address).expect("Could not parse given address");
        let (ws_stream, _) = connect_async(url).await.expect("Failed to connect");
        debug!("WebSocket handshake has been successfully completed");
        let (mut write, read) = ws_stream.split();

        let registered = Arc::from(Mutex::from(false));
        let next_command_id = Arc::from(Mutex::from(0));
        let reg = registered.clone();

        let pending_requests = Arc::from(Mutex::from(HashMap::new()));
        let requests_to_process = pending_requests.clone();
        tokio::spawn(
            async move { process_messages_from_server(read, reg, requests_to_process).await },
        );
        write.send(Message::text(HANDSHAKE)).await.unwrap();

        Ok(WebosClient {
            write,
            next_command_id,
            registered: registered.clone(),
            pending_requests,
        })
    }

    pub async fn send_command(&mut self, cmd: Command) -> Result<CommandResponse, String> {
        if !*self.registered.lock().unwrap() {
            return Err(String::from("Not registered"));
        }
        match self.next_command_id.lock() {
            Ok(mut val) => {
                *val += 1;
                match self
                    .write
                    .send(Message::text(
                        serde_json::to_string(&create_command(*val, cmd)).unwrap(),
                    ))
                    .await
                {
                    Ok(_) => {
                        let (promise, pinky) = PinkySwear::<CommandResponse>::new();
                        self.pending_requests.lock().unwrap().insert(*val, pinky);
                        Ok(promise.await)
                    }
                    Err(_) => Err(String::from("Could not send command")),
                }
            }
            Err(_) => Err(String::from("Could not generate next id")),
        }
    }
}

async fn process_messages_from_server(
    sink: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    registered: Arc<Mutex<bool>>,
    pending_requests: Arc<Mutex<HashMap<u8, Pinky<CommandResponse>>>>,
) {
    sink.for_each(|message| match message {
        Ok(_message) => {
            if let Some(text_message) = _message.clone().into_text().ok() {
                if let Ok(json) = serde_json::from_str::<Value>(&text_message) {
                    if json["type"] == "registered" {
                        *registered.lock().unwrap() = true;
                    } else if *registered.lock().unwrap() {
                        let response = CommandResponse {
                            id: json["id"].as_i64().unwrap() as u8,
                            payload: Some(json["payload"].clone()),
                        };

                        let requests = pending_requests.lock().unwrap();
                        requests.get(&response.id).unwrap().swear(response);
                    }
                }
            }
            ready(())
        }
        Err(_) => ready(()),
    })
    .await
}

fn create_command(id: u8, cmd: Command) -> Option<CommandRequest> {
    match cmd {
        Command::CreateToast(text) => Some(CommandRequest {
            id,
            r#type: String::from("request"),
            uri: String::from("ssap://system.notifications/createToast"),
            payload: Some(json!({ "message": text })),
        }),
        Command::OpenBrowser(url) => Some(CommandRequest {
            id,
            r#type: String::from("request"),
            uri: String::from("ssap://system.launcher/open"),
            payload: Some(json!({ "target": url })),
        }),
        Command::TurnOff => Some(CommandRequest {
            id,
            r#type: String::from("request"),
            uri: String::from("ssap://system/turnOff"),
            payload: None,
        }),
        Command::SetChannel(channel_id) => Some(CommandRequest {
            id,
            r#type: String::from("request"),
            uri: String::from("ssap://tv/openChannel"),
            payload: Some(json!({ "channelId": channel_id })),
        }),
        Command::SetInput(input_id) => Some(CommandRequest {
            id,
            r#type: String::from("request"),
            uri: String::from("ssap://tv/switchInput"),
            payload: Some(json!({ "inputId": input_id })),
        }),
        Command::SetMute(mute) => Some(CommandRequest {
            id,
            r#type: String::from("request"),
            uri: String::from("ssap://audio/setMute"),
            payload: Some(json!({ "mute": mute })),
        }),
        Command::SetVolume(volume) => Some(CommandRequest {
            id,
            r#type: String::from("request"),
            uri: String::from("ssap://audio/setVolume"),
            payload: Some(json!({ "volume": volume })),
        }),
        Command::GetChannelList => Some(CommandRequest {
            id,
            r#type: String::from("request"),
            uri: String::from("ssap://tv/getChannelList"),
            payload: None,
        }),
        Command::GetCurrentChannel => Some(CommandRequest {
            id,
            r#type: String::from("request"),
            uri: String::from("ssap://tv/getCurrentChannel"),
            payload: None,
        }),
        Command::OpenChannel(channel_id) => Some(CommandRequest {
            id,
            r#type: String::from("request"),
            uri: String::from("ssap://tv/openChannel"),
            payload: Some(json!({ "channelId": channel_id })),
        }),
        Command::GetExternalInputList => Some(CommandRequest {
            id,
            r#type: String::from("request"),
            uri: String::from("ssap://tv/getExternalInputList"),
            payload: None,
        }),
        Command::SwitchInput(input_id) => Some(CommandRequest {
            id,
            r#type: String::from("request"),
            uri: String::from("ssap://tv/switchInput"),
            payload: Some(json!({ "inputId": input_id })),
        }),
        Command::IsMuted => Some(CommandRequest {
            id,
            r#type: String::from("request"),
            uri: String::from("ssap://audio/getStatus"),
            payload: None,
        }),
        Command::GetVolume => Some(CommandRequest {
            id,
            r#type: String::from("request"),
            uri: String::from("ssap://audio/getVolume"),
            payload: None,
        }),
        Command::PlayMedia => Some(CommandRequest {
            id,
            r#type: String::from("request"),
            uri: String::from("ssap://media.controls/play"),
            payload: None,
        }),
        Command::StopMedia => Some(CommandRequest {
            id,
            r#type: String::from("request"),
            uri: String::from("ssap://media.controls/stop"),
            payload: None,
        }),
        Command::PauseMedia => Some(CommandRequest {
            id,
            r#type: String::from("request"),
            uri: String::from("ssap://media.controls/pause"),
            payload: None,
        }),
        Command::RewindMedia => Some(CommandRequest {
            id,
            r#type: String::from("request"),
            uri: String::from("ssap://media.controls/rewind"),
            payload: None,
        }),
        Command::ForwardMedia => Some(CommandRequest {
            id,
            r#type: String::from("request"),
            uri: String::from("ssap://media.controls/fastForward"),
            payload: None,
        }),
        Command::ChannelUp => Some(CommandRequest {
            id,
            r#type: String::from("request"),
            uri: String::from("ssap://tv/channelUp"),
            payload: None,
        }),
        Command::ChannelDown => Some(CommandRequest {
            id,
            r#type: String::from("request"),
            uri: String::from("ssap://tv/channelDown"),
            payload: None,
        }),
        Command::Turn3DOn => Some(CommandRequest {
            id,
            r#type: String::from("request"),
            uri: String::from("ssap://com.webos.service.tv.display/set3DOn"),
            payload: None,
        }),
        Command::Turn3DOff => Some(CommandRequest {
            id,
            r#type: String::from("request"),
            uri: String::from("ssap://com.webos.service.tv.display/set3DOff"),
            payload: None,
        }),
        Command::GetServicesList => Some(CommandRequest {
            id,
            r#type: String::from("request"),
            uri: String::from("ssap://com.webos.service.update/getCurrentSWInformation"),
            payload: None,
        }),
    }
}
