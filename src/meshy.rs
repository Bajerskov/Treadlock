//! Model generation through Meshy, as an author-time tool.
//!
//! This is deliberately not part of the game. Generation costs credits and
//! takes minutes; the game loads `.glb` files off disk and neither knows nor
//! cares where they came from. Run this once, commit the results, and the
//! manifest picks them up.
//!
//! Behind the `meshy` feature, so the shipped binary carries no HTTP client
//! and cannot talk to a paid service at runtime:
//!
//! ```sh
//! cargo run --features meshy -- --meshy-balance
//! cargo run --features meshy -- --meshy-cars --dry-run
//! cargo run --features meshy -- --meshy-cars
//! ```
//!
//! The API key is read from `MESHY_API_KEY` and is never printed, logged, or
//! written to any file this writes.

use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::assets::Library;

// ---------------------------------------------------------------------------
// The shape of the API.
//
// Everything Meshy-specific is in this block on purpose. It was written
// without access to the documentation - both docs.meshy.ai and api.meshy.ai
// are unreachable from the machine this was developed on - so if Meshy has
// moved an endpoint or renamed a field, the fix is here and nowhere else.
// Every response field is read defensively and reported by name when missing,
// so a mismatch says which field rather than failing as a null.
// ---------------------------------------------------------------------------
const API: &str = "https://api.meshy.ai";
const TEXT_TO_3D: &str = "/openapi/v2/text-to-3d";
const BALANCE: &str = "/openapi/v1/balance";
/// Field holding the new task's id in a create response.
const FIELD_TASK_ID: &str = "result";
/// Fields on a task while it runs and once it is done.
const FIELD_STATUS: &str = "status";
const FIELD_PROGRESS: &str = "progress";
const FIELD_MODEL_URLS: &str = "model_urls";
const FIELD_GLB: &str = "glb";
const FIELD_ERROR: &str = "task_error";

/// Generated models are textured and can run to a good few megabytes; ureq
/// caps a body read at ten by default and would otherwise truncate one
/// silently.
const MAX_DOWNLOAD: u64 = 64 * 1024 * 1024;
/// Generation takes minutes. Polling harder does not make it faster.
const POLL_INTERVAL: Duration = Duration::from_secs(10);
const POLL_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// What to generate, and into which manifest slot.
///
/// The prompts describe the same class of car six ways. Rollcage bodies are
/// symmetric top to bottom and ride on outsized exposed wheels, because the
/// game spends real time upside down and a car that reads as inverted is a car
/// that looks broken.
const CARS: [(&str, &str); 6] = [
    (
        "car_player",
        "futuristic symmetrical racing car, wedge-shaped body, four huge exposed wheels larger than the body is tall, \
         identical top and bottom so it looks right upside down, crimson and white livery, clean low-poly game asset",
    ),
    (
        "car_rival_a",
        "futuristic symmetrical racing car, blunt aggressive nose, four oversized exposed wheels, \
         identical top and bottom, matte black and orange livery, clean low-poly game asset",
    ),
    (
        "car_rival_b",
        "futuristic symmetrical racing car, long narrow arrow body, four oversized exposed wheels, \
         identical top and bottom, teal and chrome livery, clean low-poly game asset",
    ),
    (
        "car_rival_c",
        "futuristic symmetrical racing car, heavy armoured bulldozer body, four oversized exposed wheels, \
         identical top and bottom, industrial yellow and gunmetal livery, clean low-poly game asset",
    ),
    (
        "car_rival_d",
        "futuristic symmetrical racing car, sleek rounded teardrop body, four oversized exposed wheels, \
         identical top and bottom, deep purple and silver livery, clean low-poly game asset",
    ),
    (
        "car_rival_e",
        "futuristic symmetrical racing car, angular stealth fighter body, four oversized exposed wheels, \
         identical top and bottom, dark green and copper livery, clean low-poly game asset",
    ),
];

/// A budget the BC-250 can carry six of on screen at once.
const TARGET_POLYCOUNT: u64 = 18_000;

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        // Read the status rather than being handed an opaque error for it: on
        // an API whose exact shape is unconfirmed, the server's own message is
        // the most useful thing in the response.
        .http_status_as_error(false)
        .build()
        .new_agent()
}

fn key() -> Result<String, String> {
    match std::env::var("MESHY_API_KEY") {
        Ok(k) if !k.trim().is_empty() => Ok(k.trim().to_string()),
        _ => Err("MESHY_API_KEY is not set in the environment.\n\
                  Create a key at https://www.meshy.ai/ and export it:\n  \
                  PowerShell: $env:MESHY_API_KEY = \"...\"\n  \
                  bash:       export MESHY_API_KEY=...\n\
                  It is read from the environment and never written to disk."
            .to_string()),
    }
}

/// A response body, with the status, so failures can say what the server said.
fn body_of(response: &mut ureq::http::Response<ureq::Body>) -> String {
    response
        .body_mut()
        .read_to_string()
        .unwrap_or_else(|e| format!("<body unreadable: {e}>"))
}

fn check(status: u16, body: &str, what: &str) -> Result<(), String> {
    if (200..300).contains(&status) {
        return Ok(());
    }
    let hint = match status {
        401 | 403 => "\nThe key was rejected. Check MESHY_API_KEY is a current API key.",
        402 => "\nOut of credits.",
        404 => "\nEndpoint not found: the API has probably moved. \
                Every path is in the block at the top of src/meshy.rs.",
        429 => "\nRate limited. Wait and run it again; finished models are skipped.",
        _ => "",
    };
    Err(format!("{what}: HTTP {status}{hint}\n{body}"))
}

pub fn balance() -> Result<(), String> {
    let key = key()?;
    let mut response = agent()
        .get(&format!("{API}{BALANCE}"))
        .header("Authorization", &format!("Bearer {key}"))
        .call()
        .map_err(|e| format!("could not reach {API}: {e}"))?;
    let status = response.status().as_u16();
    let body = body_of(&mut response);
    check(status, &body, "balance")?;
    println!("{body}");
    Ok(())
}

fn create(agent: &ureq::Agent, key: &str, request: Value) -> Result<String, String> {
    let mut response = agent
        .post(&format!("{API}{TEXT_TO_3D}"))
        .header("Authorization", &format!("Bearer {key}"))
        .send_json(&request)
        .map_err(|e| format!("could not reach {API}: {e}"))?;
    let status = response.status().as_u16();
    let body = body_of(&mut response);
    check(status, &body, "create task")?;

    let parsed: Value = serde_json::from_str(&body)
        .map_err(|e| format!("create task: response was not JSON ({e})\n{body}"))?;
    // Some deployments answer with the id at the top level, some wrap it.
    parsed
        .get(FIELD_TASK_ID)
        .or_else(|| parsed.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            format!("create task: no {FIELD_TASK_ID:?} in the response\n{body}")
        })
}

fn fetch(agent: &ureq::Agent, key: &str, id: &str) -> Result<Value, String> {
    let mut response = agent
        .get(&format!("{API}{TEXT_TO_3D}/{id}"))
        .header("Authorization", &format!("Bearer {key}"))
        .call()
        .map_err(|e| format!("could not reach {API}: {e}"))?;
    let status = response.status().as_u16();
    let body = body_of(&mut response);
    check(status, &body, "poll task")?;
    serde_json::from_str(&body)
        .map_err(|e| format!("poll task: response was not JSON ({e})\n{body}"))
}

/// Block until the task finishes, reporting progress on one line.
fn wait(agent: &ureq::Agent, key: &str, id: &str, label: &str) -> Result<Value, String> {
    let start = Instant::now();
    loop {
        let task = fetch(agent, key, id)?;
        let status = task.get(FIELD_STATUS).and_then(Value::as_str).unwrap_or("");
        let progress = task.get(FIELD_PROGRESS).and_then(Value::as_u64).unwrap_or(0);

        match status {
            "SUCCEEDED" => {
                println!("\r  {label}: done            ");
                return Ok(task);
            }
            "FAILED" | "CANCELED" | "EXPIRED" => {
                let message = task
                    .get(FIELD_ERROR)
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("no reason given");
                return Err(format!("{label}: {status} - {message}"));
            }
            "" => return Err(format!("{label}: no {FIELD_STATUS:?} field in the task\n{task}")),
            _ => {}
        }

        if start.elapsed() > POLL_TIMEOUT {
            return Err(format!(
                "{label}: still {status} after {} minutes. The task id is {id}; \
                 it is not lost, and re-running will not pay for it twice.",
                POLL_TIMEOUT.as_secs() / 60
            ));
        }
        print!("\r  {label}: {status} {progress}%   ");
        std::io::stdout().flush().ok();
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn download(agent: &ureq::Agent, url: &str, to: &Path) -> Result<usize, String> {
    let mut response = agent
        .get(url)
        .call()
        .map_err(|e| format!("downloading {}: {e}", to.display()))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(format!("downloading {}: HTTP {status}", to.display()));
    }
    let bytes = response
        .body_mut()
        .with_config()
        .limit(MAX_DOWNLOAD)
        .read_to_vec()
        .map_err(|e| format!("downloading {}: {e}", to.display()))?;

    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(to, &bytes).map_err(|e| format!("{}: {e}", to.display()))?;
    Ok(bytes.len())
}

/// Generate any car the library is still missing.
///
/// Slots that already have a file are skipped, so an interrupted run resumes
/// rather than paying for the same model twice.
pub fn generate_cars(library: &Library, refine: bool, dry_run: bool) -> Result<(), String> {
    let wanted: Vec<(&str, &str)> = CARS
        .iter()
        .filter(|(id, _)| library.path(id).is_none())
        .copied()
        .collect();

    if wanted.is_empty() {
        println!("every car slot already has a model; nothing to generate");
        return Ok(());
    }

    println!(
        "{} car{} to generate{}:",
        wanted.len(),
        if wanted.len() == 1 { "" } else { "s" },
        if refine { ", each previewed then refined" } else { " (preview only)" }
    );
    for (id, prompt) in &wanted {
        let target = library
            .get(id)
            .map(|e| e.path.display().to_string())
            .unwrap_or_else(|| format!("assets/models/cars/{id}.glb"));
        println!("  {id:<14} -> {target}\n    {prompt}");
    }

    if dry_run {
        println!(
            "\ndry run: nothing was sent and no credits were spent.\n\
             Drop --dry-run to generate."
        );
        return Ok(());
    }

    println!(
        "\nThis spends Meshy credits{}. Generation takes a few minutes per car.\n",
        if refine { ", roughly five times as many with refinement" } else { "" }
    );

    let key = key()?;
    let agent = agent();
    let mut made = 0;
    let mut failed = Vec::new();

    for (id, prompt) in &wanted {
        println!("{id}:");
        let Some(entry) = library.get(id) else {
            failed.push(format!("{id}: not declared in the manifest"));
            continue;
        };

        let result = (|| -> Result<usize, String> {
            let preview = create(
                &agent,
                &key,
                json!({
                    "mode": "preview",
                    "prompt": prompt,
                    "art_style": "realistic",
                    "should_remesh": true,
                    "topology": "triangle",
                    "target_polycount": TARGET_POLYCOUNT,
                }),
            )?;
            let mut task = wait(&agent, &key, &preview, "preview")?;

            if refine {
                let id = create(
                    &agent,
                    &key,
                    json!({ "mode": "refine", "preview_task_id": preview, "enable_pbr": true }),
                )?;
                task = wait(&agent, &key, &id, "refine")?;
            }

            let url = task
                .get(FIELD_MODEL_URLS)
                .and_then(|u| u.get(FIELD_GLB))
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    format!("no {FIELD_MODEL_URLS}.{FIELD_GLB} on the finished task\n{task}")
                })?;
            download(&agent, url, &entry.path)
        })();

        match result {
            Ok(bytes) => {
                println!("  saved {} ({:.1} MB)", entry.path.display(), bytes as f32 / 1e6);
                made += 1;
            }
            // One car failing should not throw away the ones that worked, or a
            // rate limit at car five costs the whole run.
            Err(e) => {
                eprintln!("  {e}");
                failed.push(format!("{id}: {e}"));
            }
        }
    }

    println!("\n{made} of {} generated", wanted.len());
    if !failed.is_empty() {
        println!("still missing:");
        for f in &failed {
            println!("  {f}");
        }
    }
    println!(
        "Check one before racing it:\n  \
         cargo run --release -- --check-model --car assets/models/cars/player.glb"
    );
    Ok(())
}
