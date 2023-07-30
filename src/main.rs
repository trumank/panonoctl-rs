use anyhow::{bail, Context, Result};
use cotton_ssdp::{AsyncService, Notification};
use futures::StreamExt;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{cell::RefCell, collections::HashMap, fmt::Debug, net::TcpStream, rc::Rc};
use websocket::{sync::Client, ClientBuilder, Message, OwnedMessage};

// TODO
// {"jsonrpc":"2.0","method":"status_update","params":{"capture_available":false}}
//

#[derive(Debug, Serialize, Deserialize)]
struct MethodAuth {
    device: String,
    force: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct MethodDeleteUpf {
    image_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "method", content = "params")]
enum Method {
    Auth(MethodAuth),
    DeleteUpf(MethodDeleteUpf),
    GetUpfInfos,
    GetStatus,
    GetOptions,
    Capture,
}

#[derive(Debug, Serialize, Deserialize)]
struct Request {
    id: u32,
    #[serde(flatten)]
    method: Method,
    jsonrpc: &'static str,
}

#[derive(Debug, Serialize, Deserialize)]
struct Response<T> {
    id: u32,
    jsonrpc: String,
    result: T,
    warning: Option<ResponseWarning>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ResponseWarning {
    code: u32,
    message: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ResponseStatus {
    auth_token: String,
    capture_available: bool,
    current_time: String,
    device_id: String,
    firmware_update_url: String,
    firmware_version: String,
    is_auth: bool,
    serial_number: String,
    storage: HashMap<String, Storage>,
    update_ready: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct ResponseGetUpfInfos {
    is_full: bool,
    upf_infos: Vec<UpfInfo>,
}

// TODO error reponse {"error":{"code":309,"details":{"panorama":{"message":"no_panorama","sender":"delete_upf"},"preview":{"message":"no_preview","sender":"delete_upf"}},"request":{"id":3,"jsonrpc":"2.0","method":"delete_upf","params":{"image_id":"4fd70dfc074340296cc2ebb92158a18d"}}},"id":3,"jsonrpc":"2.0"}
#[derive(Debug, Serialize, Deserialize)]
struct ResponseDelete {
    panorama: bool,
    preview: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct ResponseCapture {
    capture_available: bool,
    options: CaptureOptions,
}

#[derive(Debug, Serialize, Deserialize)]
struct CaptureOptions {
    #[serde(rename = "AutoExposure")]
    auto_exposure: bool,
    #[serde(rename = "ColorTemperature")]
    color_temperatue: String,
    #[serde(rename = "ExposureTime")]
    exposure_time: f64,
    #[serde(rename = "ISO")]
    iso: String,
    #[serde(rename = "TriggerDelay")]
    trigger_delay: f64,
}

#[derive(Debug, Serialize, Deserialize)]
struct UpfInfo {
    capture_date: String,
    image_id: String,
    preview_url: String,
    size: u64,
    upf_url: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Storage {
    total: u64,
    usage: u64,
}

fn send(client: &mut Client<TcpStream>, req_id: &mut u32, method: Method) -> Result<()> {
    *req_id += 1;
    let text = serde_json::to_string(&Request {
        id: *req_id,
        method,
        jsonrpc: "2.0",
    })?;
    client.send_message(&Message::text(text))?;
    Ok(())
}

fn recv<T: Debug + DeserializeOwned>(client: &mut Client<TcpStream>) -> Result<Response<T>> {
    match client.recv_message()? {
        OwnedMessage::Text(text) => {
            //println!("{}", text);
            Ok(serde_json::from_str::<Response<T>>(&text)
                .with_context(|| format!("Error parsing {}", &text))?)
            //println!("{:#?}", res.result);
        }
        OwnedMessage::Close(_) => {
            bail!("Websocket closed")
        }
        OwnedMessage::Binary(_) => unimplemented!(),
        OwnedMessage::Ping(_) => unimplemented!(),
        OwnedMessage::Pong(_) => unimplemented!(),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let mut netif = cotton_netif::get_interfaces_async()?;
    let mut ssdp = AsyncService::new()?;

    let mut stream = ssdp.subscribe("panono:ball-camera");
    println!("Searching for camera...");
    let location = loop {
        tokio::select! {
            notification = stream.next() => {
                if let Some(Notification::Alive { location, .. }) = notification {
                    break location;
                }
            },
            e = netif.next() => {
                if let Some(Ok(event)) = e {
                    ssdp.on_network_event(&event);
                }
            }
        }
    };
    println!("Camera found at {location}");

    let client = Rc::new(RefCell::new(
        ClientBuilder::new(&location)
            .unwrap()
            .add_protocol("rust-websocket")
            .connect_insecure()?,
    ));

    let mut req_id = 0;

    send(
        &mut client.borrow_mut(),
        &mut req_id,
        Method::Auth(MethodAuth {
            device: "test".to_string(),
            force: "test".to_string(),
        }),
    )?;
    let auth: Response<ResponseStatus> = recv(&mut client.borrow_mut())?;
    println!("{:#?}", auth);

    //return Ok(());

    use easy_repl::{command, CommandStatus, Repl};

    let repl = Repl::builder();

    let c = client.clone();
    let repl = repl.add(
        "get_upf_infos",
        command! {
        "List all UPFs",
        () =>
            || {
            send(&mut c.borrow_mut(), &mut req_id, Method::GetUpfInfos)?;
            let res: Response<ResponseGetUpfInfos> = recv(&mut c.borrow_mut())?;
            println!("{:#?}", res);
            Ok(CommandStatus::Done)
            }},
    );

    let c = client.clone();
    let repl = repl
        .add(
            "delete",
            command! {
                "Delete UPF by ID",
                (id: String) => |image_id| {
                    send(&mut c.borrow_mut(), &mut req_id, Method::DeleteUpf(MethodDeleteUpf { image_id }))?;
                    let res: Response<ResponseDelete> = recv(&mut c.borrow_mut())?;
                    println!("{:#?}", res);
                    Ok(CommandStatus::Done)
                }
            },
        );

    let c = client.clone();
    let repl = repl.add(
        "get_status",
        command! {
            "Get device status",
            () => || {
                send(&mut c.borrow_mut(), &mut req_id, Method::GetStatus)?;
                let res: Response<ResponseStatus> = recv(&mut c.borrow_mut())?;
                println!("{:#?}", res);
                Ok(CommandStatus::Done)
            }
        },
    );

    let c = client.clone();
    let repl = repl.add(
        "get_options",
        command! {
            "Get options",
            () => || {
                send(&mut c.borrow_mut(), &mut req_id, Method::GetOptions)?;
                let res: Response<ResponseStatus> = recv(&mut c.borrow_mut())?;
                println!("{:#?}", res);
                Ok(CommandStatus::Done)
            }
        },
    );

    let c = client.clone();
    let repl = repl.add(
        "capture",
        command! {
            "Capture new panorama",
            () => || {
                send(&mut c.borrow_mut(), &mut req_id, Method::Capture)?;
                let res: Response<ResponseCapture> = recv(&mut c.borrow_mut())?;
                println!("{:#?}", res);
                Ok(CommandStatus::Done)
            }
        },
    );

    let mut repl = repl.build().expect("Failed to create repl");

    repl.run().expect("Critical REPL error");

    Ok(())
}
