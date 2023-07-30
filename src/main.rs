use anyhow::{bail, Context, Result};
use clap::Parser;
use indicatif::{ProgressBar, ProgressIterator, ProgressState, ProgressStyle};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::value::RawValue;
use std::{
    cell::RefCell,
    collections::HashMap,
    fmt::{Debug, Write},
    fs,
    io::Read,
    net::TcpStream,
    path::Path,
    rc::Rc,
};
use websocket::{sync::Client, ClientBuilder, Message, OwnedMessage};

// TODO
// {"jsonrpc":"2.0","method":"status_update","params":{"capture_available":false}}
//

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "method", content = "params")]
enum Method {
    Auth { device: String, force: String },
    DeleteUpf { image_id: String },
    GetUpfInfos,
    GetStatus,
    GetOptions,
    GetOptionList,
    GetOption { name: String },
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
struct IncomingRequest<'r> {
    jsonrpc: String,
    method: String,
    #[serde(borrow)]
    params: &'r RawValue,
}

#[derive(Debug, Serialize, Deserialize)]
struct Response<'r> {
    id: u32,
    jsonrpc: String,
    #[serde(borrow)]
    result: &'r RawValue,
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

#[derive(Debug, Serialize, Deserialize)]
struct ResponseGetOptionList {
    options: Vec<CameraOption>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum CameraOption {
    Boolean {
        name: String,
        constraints: Vec<Constraint<bool>>,
    },
    Enumeration {
        name: String,
        constraints: Vec<Constraint<String>>,
    },
    Number {
        name: String,
        constraints: Vec<Constraint<String>>,
    },
    Integer {
        name: String,
        constraints: Vec<Constraint<u64>>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "constraint", rename_all = "snake_case")]
enum Constraint<T> {
    Values { value: Vec<T> },
    Min { value: T },
    Max { value: T },
}

#[derive(Debug, Serialize, Deserialize)]
struct ResponseGetOption {
    name: String,
    value: StringOrNumber,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StringOrNumber {
    String(String),
    Number(f64),
    Bool(bool),
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

fn send<T: Debug + DeserializeOwned>(
    client: &mut Client<TcpStream>,
    req_id: &mut u32,
    method: Method,
) -> Result<T> {
    *req_id += 1;
    let id = *req_id;
    let text = serde_json::to_string(&Request {
        id,
        method,
        jsonrpc: "2.0",
    })?;
    client.send_message(&Message::text(text))?;

    #[derive(Debug)]
    enum PacketIncoming<'a> {
        Response(Response<'a>),
        IncomingRequest(IncomingRequest<'a>),
    }

    loop {
        for text in recv(client)?.lines() {
            let res = serde_json::from_str(text)
                .map(PacketIncoming::Response)
                .or_else(|_| serde_json::from_str(text).map(PacketIncoming::IncomingRequest))
                .with_context(|| format!("Error parsing packet {}", &text))?;

            match res {
                PacketIncoming::Response(r) if r.id == id => {
                    let text = r.result.get();
                    return serde_json::from_str::<T>(text)
                        .with_context(|| format!("Error parsing response {}", &text));
                }
                other => {
                    println!("unexpected packet {:#?}", other);
                }
            }
        }
    }
}

fn recv(client: &mut Client<TcpStream>) -> Result<String> {
    match client.recv_message()? {
        OwnedMessage::Text(text) => Ok(text),
        OwnedMessage::Close(_) => bail!("Websocket closed"),
        OwnedMessage::Binary(_) => unimplemented!(),
        OwnedMessage::Ping(_) => unimplemented!(),
        OwnedMessage::Pong(_) => unimplemented!(),
    }
}

#[cfg(feature = "ssdp")]
#[tokio::main(flavor = "current_thread")]
async fn find_camera() -> Result<String> {
    use cotton_ssdp::{AsyncService, Notification};
    use futures::StreamExt;
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
    Ok(location)
}

/// 3rd party REPL for Panono 360 Camera
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Websocket address for the camera. If ommitted, it attempt to locate it with SSDP
    /// Example on WiFi: ws://192.168.80.80:12345/8086
    address: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let address = match args.address {
        Some(address) => {
            println!("Connecting to {}", address);
            address
        }
        None => {
            #[cfg(feature = "ssdp")]
            {
                find_camera()?
            }
            #[cfg(not(feature = "ssdp"))]
            {
                bail!("Automatic discovery (\"ssdp\" feature, Linux only) is disabled. See --help to specify manually")
            }
        }
    };

    let output_dir = Path::new("upfs");

    let client = Rc::new(RefCell::new(
        ClientBuilder::new(&address)
            .unwrap()
            .add_protocol("rust-websocket")
            .connect_insecure()?,
    ));

    let mut req_id = 0;

    let auth: ResponseStatus = send(
        &mut client.borrow_mut(),
        &mut req_id,
        Method::Auth {
            device: "test".to_string(),
            force: "test".to_string(),
        },
    )?;
    println!("{:#?}", auth);

    use easy_repl::{command, CommandStatus, Repl};

    let repl = Repl::builder();

    let c = client.clone();
    let repl = repl.add(
        "delete",
        command! {
            "Delete UPF by ID",
            (id: String) => |image_id| {
                let res: ResponseDelete = send(&mut c.borrow_mut(), &mut req_id, Method::DeleteUpf{ image_id })?;
                println!("{:#?}", res);
                Ok(CommandStatus::Done)
            }
        },
    );

    let c = client.clone();
    let repl = repl.add(
        "download",
        command! {
            "Download any new UPFs",
            () => || {
                let res: ResponseGetUpfInfos = send(&mut c.borrow_mut(), &mut req_id, Method::GetUpfInfos)?;
                let mut to_download = vec![];
                for upf in &res.upf_infos {
                    let path = output_dir.join(&format!("{}.upf", upf.image_id));
                    if path.exists() {
                        println!("{} already exists, skipping...", path.display());
                    } else {
                        to_download.push((upf, path));
                    }
                }
                fs::create_dir(output_dir).ok();
                for (i, (upf, path)) in to_download.iter().enumerate() {
                    println!("[{}/{}] downloading {} to {}", i + 1, to_download.len(), upf.image_id, path.display());
                    let res = ureq::get(&upf.upf_url)
                        .call()?;
                    let size = res.header("Content-Length").and_then(|l| l.parse::<usize>().ok()).unwrap_or(upf.size as usize);
                    let pb = ProgressBar::new(size as u64).with_style(ProgressStyle::with_template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})")
                        .unwrap()
                        .with_key("eta", |state: &ProgressState, w: &mut dyn Write| write!(w, "{:.1}s", state.eta().as_secs_f64()).unwrap())
                        .progress_chars("#>-"));
                    let mut data = Vec::with_capacity(size);
                    for b in res.into_reader().bytes().progress_with(pb) {
                        data.push(b?);
                    }
                    fs::write(path, data)?;
                }
                println!("complete");
                Ok(CommandStatus::Done)
            }
        },
    );

    let c = client.clone();
    let repl = repl.add(
        "get_upf_infos",
        command! {
            "List all UPFs",
            () =>
            || {
                let res: ResponseGetUpfInfos = send(&mut c.borrow_mut(), &mut req_id, Method::GetUpfInfos)?;
                let mut upfs = res.upf_infos.iter().collect::<Vec<_>>();
                upfs.sort_by_key(|u| &u.capture_date);
                for upf in upfs {
                    println!("{}  {}  {:>7}  {}", upf.capture_date, upf.image_id, upf.size, upf.upf_url);
                }
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
                let res: ResponseStatus = send(&mut c.borrow_mut(), &mut req_id, Method::GetStatus)?;
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
                let res: ResponseStatus = send(&mut c.borrow_mut(), &mut req_id, Method::GetOptions)?;
                println!("{:#?}", res);
                Ok(CommandStatus::Done)
            }
        },
    );

    let c = client.clone();
    let repl = repl.add(
        "get_option_list",
        command! {
            "Get option list",
            () => || {
                let res: ResponseGetOptionList = send(&mut c.borrow_mut(), &mut req_id, Method::GetOptionList)?;
                println!("{:#?}", res);
                Ok(CommandStatus::Done)
            }
        },
    );

    let c = client.clone();
    let repl = repl.add(
        "get_option_value",
        command! {
            "Get option value",
            (name: String) => |name| {
                let res: ResponseGetOption = send(&mut c.borrow_mut(), &mut req_id, Method::GetOption { name })?;
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
                let res: ResponseCapture = send(&mut c.borrow_mut(), &mut req_id, Method::Capture)?;
                println!("{:#?}", res);
                Ok(CommandStatus::Done)
            }
        },
    );

    let mut repl = repl.build().expect("Failed to create repl");

    repl.run().expect("Critical REPL error");

    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn options() {
        serde_json::from_str::<ResponseGetOptionList>(
            r#"{
            "options": [{
                    "constraints": [{
                        "constraint": "values",
                        "value": [true, false]
                    }],
                    "name": "AutoExposure",
                    "type": "Boolean"
                },
                {
                    "constraints": [{
                        "constraint": "values",
                        "value": ["0", "3000", "4500", "5500", "6500", "8000"]
                    }],
                    "name": "ColorTemperature",
                    "type": "Enumeration"
                },
                {
                    "constraints": [{
                            "constraint": "min",
                            "value": "0.25"
                        },
                        {
                            "constraint": "max",
                            "value": "2000"
                        }
                    ],
                    "name": "ExposureTime",
                    "type": "Number"
                },
                {
                    "constraints": [{
                        "constraint": "values",
                        "value": ["50", "100", "200", "400", "800"]
                    }],
                    "name": "ISO",
                    "type": "Enumeration"
                },
                {
                    "constraints": [{
                            "constraint": "min",
                            "value": 0
                        },
                        {
                            "constraint": "max",
                            "value": 10000
                        }
                    ],
                    "name": "TriggerDelay",
                    "type": "Integer"
                }
            ]
        }"#,
        )
        .unwrap();
    }

    #[test]
    fn constraint() {
        serde_json::from_str::<CameraOption>(
            r#"{
            "constraints": [{
                "constraint": "values",
                "value": [true, false]
            }],
            "name": "AutoExposure",
            "type": "Boolean"
        }"#,
        )
        .unwrap();
    }
}
