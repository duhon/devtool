use crate::{Error, Result, Runtime};
use serde_json::{Map, Value, json};
use services_contracts::{ServiceConfig, service_config, service_configs};
use tokio::{io::AsyncWriteExt, sync::mpsc};
use tokio_util::sync::CancellationToken;
pub fn service_args(args: &[String]) -> Result<(String, &[String])> {
    if args.first().map(String::as_str) == Some("--service") {
        return args
            .get(1)
            .map(|s| (s.clone(), &args[2..]))
            .ok_or_else(|| Error::new("--service requires a value"));
    }
    if let Some(s) = args.first().and_then(|s| s.strip_prefix("--service=")) {
        return Ok((s.into(), &args[1..]));
    }
    args.first()
        .map(|s| (s.clone(), &args[1..]))
        .ok_or_else(|| Error::new("Missing service name"))
}
pub fn parse_params(config: &ServiceConfig, args: &[String]) -> Result<Value> {
    let mut flags: Map<String, Value> = Map::new();
    let positional: Vec<_> = config.params.iter().filter(|p| p.positional).collect();
    let mut pos = 0;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        let (param, value) = if !arg.starts_with('-') {
            let p = positional
                .get(pos)
                .ok_or_else(|| Error::new(format!("Unexpected argument \"{arg}\"")))?;
            pos += 1;
            (*p, arg.clone())
        } else {
            let (option, inline) = arg
                .split_once('=')
                .map(|(a, b)| (a, Some(b)))
                .unwrap_or((arg.as_str(), None));
            let p = config
                .params
                .iter()
                .find(|p| {
                    p.flag.as_ref().is_some_and(|f| option == format!("--{f}"))
                        || p.alias.as_ref().is_some_and(|a| option == format!("-{a}"))
                })
                .ok_or_else(|| Error::new(format!("Unrecognized argument \"{arg}\"")))?;
            let value = if let Some(value) = inline {
                value.to_string()
            } else {
                let v = args
                    .get(i)
                    .ok_or_else(|| Error::new(format!("{option} requires a value")))?;
                i += 1;
                v.clone()
            };
            (p, value)
        };
        let flag = param.flag.as_ref().unwrap();
        if param.multi {
            flags
                .entry(flag.clone())
                .or_insert(json!([]))
                .as_array_mut()
                .unwrap()
                .push(json!(value));
        } else {
            flags.insert(flag.clone(), json!(value));
        }
    }
    let mut params = Map::new();
    for param in &config.params {
        let Some(flag) = &param.flag else { continue };
        let Some(raw) = flags.get(flag) else {
            if let Some(v) = &param.default {
                params.insert(param.name.clone(), v.clone());
            } else if param.required {
                return Err(Error::new(format!("Missing required --{flag}")));
            }
            continue;
        };
        if param.kind == "keyvalue" {
            let values = if let Some(a) = raw.as_array() {
                a.clone()
            } else {
                vec![raw.clone()]
            };
            for entry in values {
                let entry = entry.as_str().unwrap();
                let (key, value) = entry
                    .split_once('=')
                    .filter(|(key, _)| !key.is_empty())
                    .ok_or_else(|| {
                        Error::new(format!("--{flag} expects key=value (got \"{entry}\")"))
                    })?;
                let target = param.keys.get(key.trim()).ok_or_else(|| {
                    Error::new(format!(
                        "Unknown --{flag} key \"{key}\" (expected one of: {})",
                        param.keys.keys().cloned().collect::<Vec<_>>().join(", ")
                    ))
                })?;
                params.insert(target.clone(), json!(value));
            }
        } else if param.kind == "json" {
            params.insert(
                param.name.clone(),
                serde_json::from_str(raw.as_str().unwrap_or(""))
                    .map_err(|_| Error::new(format!("--{flag} must be valid JSON")))?,
            );
        } else {
            params.insert(param.name.clone(), raw.clone());
        }
    }
    Ok(Value::Object(params))
}
pub fn usage(config: Option<&ServiceConfig>) -> String {
    let configs = config
        .map(|c| vec![c.clone()])
        .unwrap_or_else(service_configs);
    let mut out = String::from("Usage: ./dev [--service] <service> [options]\n");
    for config in configs {
        out.push_str(&format!("\n{} — {}\n", config.service, config.description));
        for p in config.params {
            if let Some(flag) = p.flag {
                out.push_str(&format!(
                    "  {}--{flag} <value>{}{}\n",
                    p.alias.map(|s| format!("-{s}, ")).unwrap_or_default(),
                    if p.required {
                        " (required)"
                    } else {
                        " (optional)"
                    },
                    if p.multi { " (repeatable)" } else { "" }
                ));
                if !p.keys.is_empty() {
                    out.push_str(&format!(
                        "    keys: {}\n",
                        p.keys.keys().cloned().collect::<Vec<_>>().join(", ")
                    ));
                }
            }
        }
    }
    out
}
pub async fn run(runtime: Runtime, args: &[String]) -> Result<i32> {
    if args.is_empty() || args == ["--help"] {
        eprint!("{}", usage(None));
        return Ok(0);
    }
    let (name, args) = service_args(args)?;
    let config =
        service_config(&name).ok_or_else(|| Error::new(format!("Unknown service \"{name}\"")))?;
    if args.iter().any(|s| s == "--help" || s == "-h") {
        eprint!("{}", usage(Some(&config)));
        return Ok(0);
    }
    let params = parse_params(&config, args)?;
    if name == "terminal" {
        let cancel = CancellationToken::new();
        let (tx, mut rx) = mpsc::channel(128);
        let run = runtime.registry.stream(&name, &params, cancel.clone(), tx);
        tokio::pin!(run);
        let mut interrupted = false;
        let info = loop {
            tokio::select! {
                event=rx.recv()=>if let Some(event)=event {
                    let data=event["data"].as_str().unwrap_or("").as_bytes();
                    if event["type"]=="stderr"{tokio::io::stderr().write_all(data).await?;}else{tokio::io::stdout().write_all(data).await?;}
                },
                result=&mut run=>break result?,
                _=tokio::signal::ctrl_c(),if !interrupted=>{interrupted=true;cancel.cancel();}
            }
        };
        while let Ok(event) = rx.try_recv() {
            let data = event["data"].as_str().unwrap_or("").as_bytes();
            if event["type"] == "stderr" {
                tokio::io::stderr().write_all(data).await?;
            } else {
                tokio::io::stdout().write_all(data).await?;
            }
        }
        if let Some(error) = info["error"].as_str() {
            eprintln!("{error}");
        }
        return Ok(info["code"]
            .as_i64()
            .map(|c| c as i32)
            .unwrap_or(if interrupted { 130 } else { 1 }));
    }
    let result = tokio::select! {
        result=runtime.registry.call(&name,&params)=>result?,
        _=tokio::signal::ctrl_c()=>return Ok(130),
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    let status = result["status"].as_u64().unwrap_or(0);
    Ok(
        if (name == "grpc" && status != 0) || (name != "grpc" && status >= 400) {
            1
        } else {
            0
        },
    )
}
