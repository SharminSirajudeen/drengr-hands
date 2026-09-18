// Prereq: an iOS simulator must already be booted (xcrun simctl list devices booted).
// Optionally set IOS_SIMULATOR_UDID to pin a specific device.

use std::f32::consts::PI;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result};
use drengr_hands::screen::ui_element::Point;
use drengr_hands::transport::{
    detect::detect_devices, simctl::SimctlTransport, DeviceOs, DeviceTransport,
};

const CANVAS_W: i32 = 400;
const CANVAS_H: i32 = 560;
const SERVER_PORT: u16 = 8765;
const CANVAS_DIR: &str = "/tmp/drengr_canvas";
const RESULT_PATH: &str = "/tmp/scenery_result.png";
const VIDEO_PATH: &str = "/tmp/scenery_demo.mov";

// Calibrated against iOS 18 simctl screenshots: palette spans logical
// y=63..120, canvas begins at y~120. Status bar above eats the rest.
const SAFARI_TOP: i32 = 63;
const PALETTE_HEIGHT: i32 = 57;
const CANVAS_DY: i32 = SAFARI_TOP + PALETTE_HEIGHT; // 120

// Swatch row vertical center.
const SWATCH_Y: i32 = SAFARI_TOP + PALETTE_HEIGHT / 2; // 91

// Order matches the palette in index.html.
const COLORS: &[&str] = &[
    "black", "red", "orange", "yellow", "green", "blue", "brown", "gray",
];

#[tokio::main]
async fn main() -> Result<()> {
    // Honour IOS_SIMULATOR_UDID if set, otherwise pick the first booted iOS sim.
    let pinned = std::env::var("IOS_SIMULATOR_UDID")
        .ok()
        .filter(|s| !s.is_empty());
    let devices = detect_devices().await;
    let device = match pinned {
        Some(udid) => devices
            .into_iter()
            .find(|d| d.id == udid && matches!(d.os, DeviceOs::Ios))
            .context("IOS_SIMULATOR_UDID does not match any booted iOS simulator")?,
        None => devices
            .into_iter()
            .find(|d| matches!(d.os, DeviceOs::Ios))
            .context("no iOS simulator booted — run `xcrun simctl boot <udid>` first")?,
    };
    println!("device: {} ({})", device.model, device.id);

    write_canvas_html()?;
    let mut server = start_http_server()?;
    let _server_guard = ServerGuard(&mut server);

    // Give python http.server a moment to bind the port.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let transport = SimctlTransport::new(device.id.clone());
    // Reset Safari so our URL lands on a fresh tab.
    let _ = transport.terminate_app("com.apple.mobilesafari").await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Stale recording from a prior run will block recordVideo; remove it.
    let _ = std::fs::remove_file(VIDEO_PATH);
    let mut recorder = start_recording(&device.id)?;

    transport.launch_app("com.apple.mobilesafari").await?;
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // Per-run cache-bust forces Safari to fetch the updated palette HTML.
    let cache_bust = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let url = format!("http://localhost:{}/?clear=1&v={}", SERVER_PORT, cache_bust);
    transport.open_url(&url).await?;
    // Cold Safari needs ~4s to focus URL bar, navigate, and paint.
    tokio::time::sleep(Duration::from_secs(4)).await;

    let strokes = scenery_strokes();
    let total_points: usize = strokes.iter().map(|(_, _, pts)| pts.len()).sum();

    for (label, color, pts) in &strokes {
        println!("  -> tap color: {}", color);
        tap_color(&transport, color).await?;
        tokio::time::sleep(Duration::from_millis(200)).await;
        // Longer strokes need more time or WDA chokes on tightly packed moves.
        let dur = ((pts.len() as u32) * 18).clamp(200, 1200);
        transport.draw_path(pts, dur).await?;
        println!("drew {} [{}] ({} pts, {}ms)", label, color, pts.len(), dur);
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    // Let the last stroke paint before we screenshot.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let png = transport.screenshot().await?;
    std::fs::write(RESULT_PATH, &png)?;

    // Stop recording: SIGINT lets simctl flush a valid mov.
    tokio::time::sleep(Duration::from_millis(500)).await;
    stop_recording(&mut recorder);
    // Wait for simctl to finalize the file.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let _ = recorder.wait();

    let video_size = std::fs::metadata(VIDEO_PATH).map(|m| m.len()).unwrap_or(0);
    println!(
        "\nstrokes: {} | points: {} | screenshot: {} ({} bytes) | video: {} ({} bytes)",
        strokes.len(),
        total_points,
        RESULT_PATH,
        png.len(),
        VIDEO_PATH,
        video_size
    );
    Ok(())
}

/// Tap the swatch for `name`. Swatch i is centered at x = i*50 + 25.
async fn tap_color(transport: &SimctlTransport, name: &str) -> Result<()> {
    let idx = COLORS
        .iter()
        .position(|c| *c == name)
        .with_context(|| format!("unknown color: {}", name))? as i32;
    let x = idx * 50 + 25;
    transport.tap(x, SWATCH_Y).await?;
    Ok(())
}

/// Build all scenery strokes as (label, color, points) in canvas-local coords
/// then shifted down by CANVAS_DY into screen space.
fn scenery_strokes() -> Vec<(&'static str, &'static str, Vec<Point>)> {
    let p = |x: i32, y: i32| Point::new(x, y + CANVAS_DY);
    let mut out: Vec<(&'static str, &'static str, Vec<Point>)> = Vec::new();

    // Sun — polyline around a circle. Closes at start so the stroke encloses fill area.
    let mut sun = Vec::with_capacity(25);
    let (cx, cy, r) = (320.0, 80.0, 32.0);
    for i in 0..=24 {
        let t = (i as f32) / 24.0 * 2.0 * PI;
        sun.push(p((cx + r * t.cos()) as i32, (cy + r * t.sin()) as i32));
    }
    out.push(("sun", "yellow", sun));

    out.push((
        "mountain_A",
        "gray",
        vec![p(30, 360), p(140, 160), p(250, 360)],
    ));
    out.push((
        "mountain_B",
        "gray",
        vec![p(180, 360), p(270, 200), p(370, 360)],
    ));

    // River — wavy line across the bottom.
    let mut river = Vec::new();
    let base_y = 480;
    let mut x = 0;
    let mut flip = false;
    while x <= 400 {
        river.push(p(x, if flip { base_y + 12 } else { base_y - 12 }));
        x += 40;
        flip = !flip;
    }
    out.push(("river", "blue", river));

    out.push((
        "house_base",
        "brown",
        vec![
            p(230, 360),
            p(230, 440),
            p(340, 440),
            p(340, 360),
            p(230, 360),
        ],
    ));
    out.push((
        "house_roof",
        "red",
        vec![p(220, 360), p(285, 300), p(350, 360)],
    ));
    out.push((
        "house_door",
        "black",
        vec![p(270, 440), p(270, 400), p(305, 400), p(305, 440)],
    ));

    out.push((
        "tree1_trunk",
        "brown",
        vec![p(70, 380), p(70, 345), p(90, 345), p(90, 380)],
    ));
    out.push((
        "tree1_foliage",
        "green",
        vec![p(40, 345), p(80, 260), p(120, 345), p(40, 345)],
    ));
    out.push((
        "tree2_trunk",
        "brown",
        vec![p(150, 375), p(150, 350), p(165, 350), p(165, 375)],
    ));
    out.push((
        "tree2_foliage",
        "green",
        vec![p(125, 350), p(157, 285), p(190, 350), p(125, 350)],
    ));

    out
}

/// Always overwrite so iterating on the HTML applies on next run.
fn write_canvas_html() -> Result<()> {
    let dir = PathBuf::from(CANVAS_DIR);
    std::fs::create_dir_all(&dir)?;
    let html = format!(
        r##"<!DOCTYPE html>
<html>
<head>
<meta name="viewport" content="width=device-width, initial-scale=1, user-scalable=no">
<title>Drengr Canvas</title>
<style>
  html,body{{margin:0;padding:0;background:#fff;font-family:-apple-system,system-ui,sans-serif;}}
  #palette{{display:flex;flex-direction:row;height:60px;width:{w}px;}}
  .sw{{width:50px;height:60px;border:1px solid #222;padding:0;margin:0;box-sizing:border-box;cursor:pointer;}}
  .sw.active{{box-shadow:inset 0 0 0 4px #fff;}}
  canvas{{display:block;touch-action:none;background:#fafafa;border-bottom:1px solid #ccc;}}
  #status{{padding:6px 10px;font-size:13px;color:#333;height:30px;box-sizing:border-box;}}
  #status b{{color:#0a0;}}
</style>
</head>
<body>
<div id="palette">
  <button class="sw active" data-c="#000000" data-n="black"  style="background:#000000"></button>
  <button class="sw"        data-c="#E53935" data-n="red"    style="background:#E53935"></button>
  <button class="sw"        data-c="#FB8C00" data-n="orange" style="background:#FB8C00"></button>
  <button class="sw"        data-c="#FDD835" data-n="yellow" style="background:#FDD835"></button>
  <button class="sw"        data-c="#43A047" data-n="green"  style="background:#43A047"></button>
  <button class="sw"        data-c="#1E88E5" data-n="blue"   style="background:#1E88E5"></button>
  <button class="sw"        data-c="#8D6E63" data-n="brown"  style="background:#8D6E63"></button>
  <button class="sw"        data-c="#757575" data-n="gray"   style="background:#757575"></button>
</div>
<canvas id="c" width="{w}" height="{h}"></canvas>
<div id="status">Strokes: <b id="n">0</b> | Points: <b id="p">0</b> | Color: <b id="cn">black</b></div>
<script>
const c = document.getElementById('c'), ctx = c.getContext('2d');
const nEl = document.getElementById('n'), pEl = document.getElementById('p'), cnEl = document.getElementById('cn');
let strokes = 0, points = 0, drawing = false;
ctx.lineWidth = 4; ctx.lineCap = 'round'; ctx.lineJoin = 'round';
ctx.strokeStyle = '#000000'; ctx.fillStyle = '#000000';

if (new URLSearchParams(location.search).get('clear') === '1') {{
  ctx.clearRect(0, 0, c.width, c.height);
}}

document.querySelectorAll('.sw').forEach(btn => {{
  btn.addEventListener('click', e => {{
    e.preventDefault();
    document.querySelectorAll('.sw').forEach(b => b.classList.remove('active'));
    btn.classList.add('active');
    ctx.strokeStyle = btn.dataset.c;
    ctx.fillStyle = btn.dataset.c;
    cnEl.textContent = btn.dataset.n;
  }});
  btn.addEventListener('touchstart', e => {{ e.preventDefault(); btn.click(); }});
}});

function pos(e){{
  const r = c.getBoundingClientRect();
  const t = e.touches ? e.touches[0] : e;
  return [t.clientX - r.left, t.clientY - r.top];
}}
function start(e){{ e.preventDefault(); drawing = true; strokes++; nEl.textContent = strokes;
  const [x,y] = pos(e); ctx.beginPath(); ctx.moveTo(x,y); }}
function move(e){{ if(!drawing) return; e.preventDefault();
  const [x,y] = pos(e); ctx.lineTo(x,y); ctx.stroke(); points++; pEl.textContent = points; }}
function end(e){{ if(!drawing) return; e.preventDefault(); drawing = false; ctx.closePath(); }}

c.addEventListener('touchstart', start);
c.addEventListener('touchmove', move);
c.addEventListener('touchend', end);
c.addEventListener('mousedown', start);
c.addEventListener('mousemove', move);
c.addEventListener('mouseup', end);
</script>
</body>
</html>"##,
        w = CANVAS_W,
        h = CANVAS_H
    );
    std::fs::write(dir.join("index.html"), html)?;
    Ok(())
}

fn start_http_server() -> Result<Child> {
    // Inline server emits no-cache headers; SimpleHTTPRequestHandler doesn't.
    let script = format!(
        r#"
import http.server, socketserver
PORT = {port}
class H(http.server.SimpleHTTPRequestHandler):
    def end_headers(self):
        self.send_header('Cache-Control','no-store, no-cache, must-revalidate, max-age=0')
        self.send_header('Pragma','no-cache')
        super().end_headers()
socketserver.TCPServer.allow_reuse_address = True
with socketserver.TCPServer(('127.0.0.1', PORT), H) as s:
    s.serve_forever()
"#,
        port = SERVER_PORT
    );
    let child = Command::new("python3")
        .args(["-c", &script])
        .current_dir(CANVAS_DIR)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn python3 http.server — is python3 on PATH?")?;
    Ok(child)
}

/// Background screen recording via `xcrun simctl io <udid> recordVideo`.
fn start_recording(udid: &str) -> Result<Child> {
    let child = Command::new("xcrun")
        .args(["simctl", "io", udid, "recordVideo", "--force", VIDEO_PATH])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn xcrun simctl recordVideo")?;
    Ok(child)
}

/// SIGINT lets recordVideo finalize a valid mov; SIGKILL would corrupt it.
fn stop_recording(child: &mut Child) {
    let pid = child.id() as i32;
    unsafe {
        libc::kill(pid, libc::SIGINT);
    }
}

struct ServerGuard<'a>(&'a mut Child);
impl Drop for ServerGuard<'_> {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
