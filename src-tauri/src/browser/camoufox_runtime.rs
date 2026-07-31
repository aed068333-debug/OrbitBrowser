use crate::domain::environment::Environment;
use crate::domain::proxy::ProxyKind;
use crate::errors::AppResult;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const WORKER_SCRIPT: &str = r#"
import base64
import json
import queue
import signal
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

running = True
ready = False
requests = queue.Queue()
events = queue.Queue()

def handle_stop(signum, frame):
    global running
    running = False

def compact(value):
    return {k: v for k, v in value.items() if v not in (None, "", [], {})}

def build_proxy(profile):
    proxy = profile.get("proxy") or {}
    server = proxy.get("server")
    if not server:
        return None
    result = {"server": server}
    if proxy.get("username"):
        result["username"] = proxy["username"]
    if proxy.get("password"):
        result["password"] = proxy["password"]
    return result

def build_kwargs(profile):
    kwargs = {
        "headless": bool(profile.get("headless", False)),
        "persistent_context": True,
        "user_data_dir": profile["user_data_dir"],
        "humanize": True,
        "block_webrtc": bool(profile.get("web_rtc_protection", True)),
    }
    proxy = build_proxy(profile)
    if proxy:
        kwargs["proxy"] = proxy
        kwargs["geoip"] = True
    locale = profile.get("locale")
    if locale and locale != "auto":
        kwargs["locale"] = locale
    os_name = profile.get("os")
    if os_name:
        kwargs["os"] = os_name
    cjk_fonts = profile.get("cjk_fonts")
    if cjk_fonts:
        kwargs["fonts"] = cjk_fonts
    width = profile.get("viewport_width")
    height = profile.get("viewport_height")
    if width and height:
        kwargs["window"] = (int(width), int(height))
    return compact(kwargs)

class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        return

    def do_GET(self):
        if self.path != "/health":
            self.send_error(404)
            return
        self.send_response(200 if ready else 503)
        self.send_header("content-type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps({"ok": ready}).encode("utf-8"))

    def do_POST(self):
        if self.path != "/rpc":
            self.send_error(404)
            return
        length = int(self.headers.get("content-length") or "0")
        try:
            payload = json.loads(self.rfile.read(length).decode("utf-8") or "{}")
            response = queue.Queue(maxsize=1)
            requests.put((payload, response))
            result = response.get(timeout=90)
            body = json.dumps(result, ensure_ascii=False).encode("utf-8")
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        except Exception as exc:
            body = json.dumps({"ok": False, "error": str(exc)}).encode("utf-8")
            self.send_response(500)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

def context_script():
    return r"""
(() => {
  const HTML_EXCERPT_LIMIT = 20000;
  const VISIBLE_TEXT_LIMIT = 12000;
  const INTERACTIVE_ELEMENTS_LIMIT = 120;
  const truncateText = (value, maxLength) => {
    const text = String(value || "");
    return text.length > maxLength
      ? `${text.slice(0, maxLength)}...[truncated ${text.length - maxLength} chars]`
      : text;
  };
  const normalizeText = (value) => String(value || "").replace(/\s+/g, " ").trim();
  const cssEscape = (value) => {
    if (globalThis.CSS && CSS.escape) return CSS.escape(value);
    return String(value).replace(/[^a-zA-Z0-9_-]/g, "\\$&");
  };
  const selectorFor = (el) => {
    if (!el || !el.tagName) return "";
    if (el.id) return `#${cssEscape(el.id)}`;
    const testId = el.getAttribute("data-testid") || el.getAttribute("data-test");
    if (testId) return `[data-testid="${String(testId).replace(/"/g, '\\"')}"]`;
    const name = el.getAttribute("name");
    if (name) return `${el.tagName.toLowerCase()}[name="${String(name).replace(/"/g, '\\"')}"]`;
    const parts = [];
    let current = el;
    while (current && current.nodeType === Node.ELEMENT_NODE && parts.length < 5) {
      const tag = current.tagName.toLowerCase();
      const parent = current.parentElement;
      if (!parent) {
        parts.unshift(tag);
        break;
      }
      const siblings = Array.from(parent.children).filter((item) => item.tagName === current.tagName);
      const index = siblings.indexOf(current) + 1;
      parts.unshift(siblings.length > 1 ? `${tag}:nth-of-type(${index})` : tag);
      current = parent;
    }
    return parts.join(" > ");
  };
  const elementKind = (el) => {
    const tag = el.tagName.toLowerCase();
    if (tag === "a") return "link";
    if (tag === "button" || el.getAttribute("role") === "button") return "button";
    if (tag === "select") return "select";
    if (tag === "textarea") return "textarea";
    return "input";
  };
  const labelFor = (el) => {
    const id = el.id;
    const label = id ? document.querySelector(`label[for="${cssEscape(id)}"]`) : null;
    return normalizeText(
      el.getAttribute("aria-label") ||
      el.getAttribute("placeholder") ||
      el.getAttribute("title") ||
      el.innerText ||
      el.value ||
      label?.innerText ||
      el.getAttribute("name") ||
      el.getAttribute("href") ||
      el.tagName.toLowerCase()
    );
  };
  const interactiveElements = Array.from(
    document.querySelectorAll("a,button,input,textarea,select,[role='button'],[contenteditable='true']")
  )
    .filter((el) => {
      const rect = el.getBoundingClientRect();
      return rect.width > 0 && rect.height > 0;
    })
    .slice(0, INTERACTIVE_ELEMENTS_LIMIT)
    .map((el) => ({
      kind: elementKind(el),
      label: truncateText(labelFor(el), 160),
      selector: truncateText(selectorFor(el), 240),
    }));
  return {
    htmlExcerpt: truncateText(document.documentElement?.outerHTML || "", HTML_EXCERPT_LIMIT),
    visibleText: truncateText(normalizeText(document.body?.innerText || ""), VISIBLE_TEXT_LIMIT),
    interactiveElements,
  };
})()
"""

def execute_action(page, payload):
    action = payload.get("action")
    params = payload.get("params") or {}
    if action == "goto":
        page.goto(params["url"], wait_until="load", timeout=int(params.get("timeout_ms") or 30000))
        return None
    if action == "navigate_without_wait":
        page.goto(params["url"], wait_until="commit", timeout=5000)
        return None
    if action == "click":
        page.click(params["selector"], timeout=int(params.get("timeout_ms") or 30000))
        return None
    if action == "mouse_click":
        page.mouse.click(float(params["x"]), float(params["y"]), button=params.get("button") or "left")
        return None
    if action == "type_text":
        page.fill(params["selector"], params.get("text") or "", timeout=int(params.get("timeout_ms") or 30000))
        return None
    if action == "wait_for_selector":
        page.wait_for_selector(params["selector"], timeout=int(params.get("timeout_ms") or 10000))
        return None
    if action == "wait":
        page.wait_for_timeout(int(params.get("milliseconds") or 0))
        return None
    if action == "evaluate":
        return page.evaluate(params["expression"])
    if action == "title":
        return page.title()
    if action == "url":
        return page.url
    if action == "screenshot_png":
        return base64.b64encode(page.screenshot(full_page=True, type="png")).decode("ascii")
    if action == "set_geolocation":
        page.context.set_geolocation({
            "latitude": float(params["latitude"]),
            "longitude": float(params["longitude"]),
            "accuracy": float(params.get("accuracy") or 20),
        })
        page.context.grant_permissions(["geolocation"])
        return None
    if action == "set_timezone":
        return None
    if action == "context_snapshot":
        data = page.evaluate(context_script())
        shot = None
        if params.get("include_screenshot"):
            shot = base64.b64encode(page.screenshot(full_page=True, type="png")).decode("ascii")
        return {
            "url": page.url,
            "title": page.title(),
            "screenshot_base64": shot,
            "html_excerpt": data.get("htmlExcerpt", ""),
            "visible_text": data.get("visibleText", ""),
            "interactive_elements": data.get("interactiveElements", []),
            "console_entries": [],
            "network_entries": [],
        }
    if action == "next_recording_event":
        timeout_ms = int(params.get("timeout_ms") or 750)
        try:
            return events.get(timeout=max(timeout_ms, 0) / 1000.0)
        except queue.Empty:
            return None
    raise ValueError(f"unknown action: {action}")

def cjk_preflight(page, locale):
    if not locale or not (
        locale.startswith("zh") or locale.startswith("ja") or locale.startswith("ko")
    ):
        return None
    try:
        ok = page.evaluate("""() => {
            const c = document.createElement('canvas'); c.width = 120; c.height = 120;
            const x = c.getContext('2d');
            x.font = '90px sans-serif';
            x.fillText('\\u4e2d', 0, 90);
            const a = x.getImageData(0, 0, 120, 120).data;
            let d1 = 0; for (let i = 0; i < a.length; i += 4) if (a[i] < 128) d1++;
            x.fillStyle = '#fff'; x.fillRect(0, 0, 120, 120);
            x.font = '90px sans-serif';
            x.fillText('\\ufffd', 0, 90);
            const b = x.getImageData(0, 0, 120, 120).data;
            let d2 = 0; for (let i = 0; i < b.length; i += 4) if (b[i] < 128) d2++;
            return Math.abs(d1 - d2) / Math.max(d1, d2, 1) > 0.2;
        }""")
        if ok is False:
            print(
                "CJK font preflight failed: Chinese/Japanese/Korean text may render as boxes; "
                "the target platform font list lacks a locally installed CJK font.",
                file=sys.stderr,
            )
    except Exception as exc:
        print(f"CJK font preflight skipped: {exc}", file=sys.stderr)


def install_recorders(page):
    def emit(event):
        events.put(event)

    def on_request(request):
        url = request.url
        if not should_record_url(url):
            return
        emit({
            "kind": "request",
            "method": request.method,
            "url": url,
            "status": None,
            "resource_type": request.resource_type,
            "title": None,
            "timestamp": "",
        })

    def on_response(response):
        url = response.url
        if not should_record_url(url):
            return
        emit({
            "kind": "response",
            "method": None,
            "url": url,
            "status": response.status,
            "resource_type": None,
            "title": None,
            "timestamp": "",
        })

    page.on("request", on_request)
    page.on("response", on_response)
    page.on("domcontentloaded", lambda: emit({
        "kind": "page_domcontent",
        "method": None,
        "url": page.url,
        "status": None,
        "resource_type": None,
        "title": page.title(),
        "timestamp": "",
    }))
    page.on("load", lambda: emit({
        "kind": "page_load",
        "method": None,
        "url": page.url,
        "status": None,
        "resource_type": None,
        "title": page.title(),
        "timestamp": "",
    }))

def should_record_url(url):
    return bool(url) and not (
        url.startswith("data:") or
        url.startswith("blob:") or
        url.startswith("devtools:")
    )

def drain_requests(page):
    processed = 0
    while True:
        try:
            payload, response = requests.get_nowait()
        except queue.Empty:
            return processed
        try:
            response.put({"ok": True, "value": execute_action(page, payload)})
        except Exception as exc:
            response.put({"ok": False, "error": str(exc)})
        processed += 1

def main():
    global ready
    if len(sys.argv) != 2:
        print("usage: orbit_camoufox_worker.py <profile.json>", file=sys.stderr)
        return 64
    signal.signal(signal.SIGTERM, handle_stop)
    signal.signal(signal.SIGINT, handle_stop)
    profile = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
    control_port = int(profile["control_port"])
    try:
        from camoufox.sync_api import Camoufox
    except Exception as exc:
        print(f"failed to import camoufox: {exc}", file=sys.stderr)
        return 2

    server = ThreadingHTTPServer(("127.0.0.1", control_port), Handler)
    import threading
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()

    with Camoufox(**build_kwargs(profile)) as browser:
        # Camoufox 0.4.11 can corrupt Brotli/Zstd responses on some sites.
        browser.set_extra_http_headers({"Accept-Encoding": "gzip, deflate"})

        page = browser.pages[0] if browser.pages else browser.new_page()
        install_recorders(page)
        start_url = profile.get("start_url") or "about:blank"
        if start_url:
            page.goto(start_url, wait_until="domcontentloaded", timeout=45000)
        cjk_preflight(page, profile.get("locale"))
        ready = True
        print(json.dumps({"event": "ready"}), flush=True)
        while running:
            open_pages = [candidate for candidate in browser.pages if not candidate.is_closed()]
            if not open_pages:
                break
            if page.is_closed():
                page = open_pages[0]
            if drain_requests(page) == 0:
                time.sleep(0.05)
        ready = False
    server.shutdown()
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
"#;

#[derive(Debug, Clone)]
pub struct CamoufoxLaunchPlan {
    pub worker_script_path: PathBuf,
    pub profile_json_path: PathBuf,
    pub log_path: PathBuf,
}

pub fn build(
    data_dir: &Path,
    env: &Environment,
    profile_dir: &Path,
    control_port: u16,
    runtime_locale: &str,
) -> AppResult<CamoufoxLaunchPlan> {
    let runtime_dir = data_dir.join("runtime").join("camoufox");
    std::fs::create_dir_all(&runtime_dir)?;
    let worker_script_path = runtime_dir.join("orbit_camoufox_worker.py");
    std::fs::write(&worker_script_path, WORKER_SCRIPT)?;

    let runtime_stem = runtime_file_stem(&env.id, control_port);
    let profile_json_path = runtime_dir.join(format!("{runtime_stem}.json"));
    let log_path = runtime_dir.join(format!("{runtime_stem}.log"));
    std::fs::write(
        &profile_json_path,
        serde_json::to_string_pretty(&worker_profile(
            env,
            profile_dir,
            control_port,
            runtime_locale,
        ))?,
    )?;

    Ok(CamoufoxLaunchPlan {
        worker_script_path,
        profile_json_path,
        log_path,
    })
}

fn runtime_file_stem(environment_id: &str, control_port: u16) -> String {
    format!("{environment_id}-{control_port}")
}

fn worker_profile(
    env: &Environment,
    profile_dir: &Path,
    control_port: u16,
    runtime_locale: &str,
) -> Value {
    json!({
        "environment_id": env.id,
        "user_data_dir": profile_dir.to_string_lossy(),
        "control_port": control_port,
        "headless": env.headless,
        "web_rtc_protection": env.web_rtc_protection,
        "locale": runtime_locale,
        "os": camoufox_os_constraint(env.platform.as_deref()),
        "cjk_fonts": cjk_font_families_for_locale(runtime_locale),
        "viewport_width": env.viewport_width,
        "viewport_height": env.viewport_height,
        "start_url": env.start_url.as_deref().unwrap_or("about:blank"),
        "proxy": proxy_profile(env),
    })
}

/// True for locales that need CJK glyphs (Chinese, Japanese, Korean).
fn locale_is_cjk(locale: &str) -> bool {
    let lower = locale.trim().to_ascii_lowercase();
    lower.starts_with("zh") || lower.starts_with("ja") || lower.starts_with("ko")
}

/// CJK font families installed on the *host* OS. Camoufox isolates rendering
/// to the target platform's font list; on macOS/Windows hosts the bundled
/// `fonts/` directory is never registered with the platform font system
/// (daijro/camoufox#706), so a Windows-target font list (SimSun, Microsoft
/// YaHei, ...) resolves to nothing and Chinese/Japanese/Korean text renders as
/// tofu boxes. Feeding the host's CJK families through Camoufox's `fonts`
/// option puts real CJK fonts back into the renderer while keeping the target
/// OS fingerprint intact.
fn cjk_font_families() -> Vec<&'static str> {
    if cfg!(target_os = "macos") {
        vec![
            "PingFang SC",
            "Songti SC",
            "Kaiti SC",
            "STHeiti",
            "Hiragino Sans GB",
        ]
    } else if cfg!(target_os = "windows") {
        vec![
            "Microsoft YaHei",
            "SimSun",
            "SimHei",
            "KaiTi",
            "FangSong",
            "DengXian",
            "NSimSun",
        ]
    } else {
        vec![
            "Noto Sans CJK SC",
            "Noto Sans SC",
            "WenQuanYi Zen Hei",
            "WenQuanYi Micro Hei",
        ]
    }
}

fn cjk_font_families_for_locale(locale: &str) -> Option<Vec<&'static str>> {
    locale_is_cjk(locale).then(cjk_font_families)
}

fn proxy_profile(env: &Environment) -> Value {
    let Some(host) = env.proxy_config.host.as_deref() else {
        return json!(null);
    };
    let Some(port) = env.proxy_config.port else {
        return json!(null);
    };
    let scheme = match env.proxy_config.kind {
        ProxyKind::Http => "http",
        ProxyKind::Https => "https",
        ProxyKind::Socks4 => "socks4",
        ProxyKind::Socks5 => "socks5",
        ProxyKind::None => return json!(null),
    };
    let mut value = json!({
        "server": format!("{scheme}://{host}:{port}"),
    });
    if !env.proxy_config.is_subscription_binding() {
        if let Some(username) = env.proxy_config.username.clone() {
            value["username"] = json!(username);
        }
        if let Some(password) = env.proxy_config.password.clone() {
            value["password"] = json!(password);
        }
    }
    value
}

fn camoufox_os_constraint(platform: Option<&str>) -> Value {
    json!(resolved_camoufox_os(platform))
}

fn resolved_camoufox_os(platform: Option<&str>) -> &'static str {
    let platform = platform.unwrap_or_default().to_ascii_lowercase();
    if platform.contains("win") {
        "windows"
    } else if platform.contains("mac") {
        "macos"
    } else if platform.contains("linux") {
        "linux"
    } else {
        host_camoufox_os()
    }
}

fn host_camoufox_os() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_target_os_maps_to_camoufox_constraint() {
        assert_eq!(camoufox_os_constraint(Some("windows")), json!("windows"));
        assert_eq!(camoufox_os_constraint(Some("MacIntel")), json!("macos"));
        assert_eq!(camoufox_os_constraint(Some("linux")), json!("linux"));
    }

    #[test]
    fn auto_target_os_uses_the_host_platform() {
        assert_eq!(
            camoufox_os_constraint(Some("auto")),
            json!(host_camoufox_os())
        );
        assert_eq!(camoufox_os_constraint(None), json!(host_camoufox_os()));
    }

    #[test]
    fn runtime_files_are_isolated_per_launch() {
        assert_ne!(
            runtime_file_stem("env-1", 9100),
            runtime_file_stem("env-1", 9101)
        );
    }

    #[test]
    fn worker_stops_after_the_last_page_closes() {
        assert!(WORKER_SCRIPT.contains("if not open_pages:"));
        assert!(WORKER_SCRIPT.contains("ready = False"));
    }

    #[test]
    fn worker_avoids_camoufox_compressed_response_corruption() {
        assert!(WORKER_SCRIPT
            .contains("browser.set_extra_http_headers({\"Accept-Encoding\": \"gzip, deflate\"})"));
    }

    #[test]
    fn worker_passes_host_cjk_fonts_into_camoufox() {
        assert!(WORKER_SCRIPT.contains("kwargs[\"fonts\"] = cjk_fonts"));
    }

    #[test]
    fn worker_preflights_cjk_rendering() {
        assert!(WORKER_SCRIPT.contains("def cjk_preflight"));
        assert!(WORKER_SCRIPT.contains("CJK font preflight failed"));
        assert!(WORKER_SCRIPT.contains("cjk_preflight(page, locale)"));
    }

    #[test]
    fn cjk_locale_detection_matches_zh_ja_ko() {
        assert!(locale_is_cjk("zh-CN"));
        assert!(locale_is_cjk("zh-TW"));
        assert!(locale_is_cjk("zh-HK"));
        assert!(locale_is_cjk("ja-JP"));
        assert!(locale_is_cjk("ko-KR"));
        assert!(!locale_is_cjk("en-US"));
        assert!(!locale_is_cjk("fr-FR"));
        assert!(!locale_is_cjk(""));
    }

    #[test]
    fn cjk_font_families_are_nonempty_on_every_host() {
        assert!(!cjk_font_families().is_empty());
    }

    #[test]
    fn cjk_fonts_are_only_injected_for_cjk_locales() {
        let with_zh = json!({"locale": "zh-CN", "cjk_fonts": cjk_font_families_for_locale("zh-CN")});
        assert!(
            with_zh["cjk_fonts"]
                .as_array()
                .is_some_and(|families| !families.is_empty())
        );
        let with_en = json!({"locale": "en-US", "cjk_fonts": cjk_font_families_for_locale("en-US")});
        assert!(with_en["cjk_fonts"].is_null());
    }

    fn health_ok(port: u16) -> bool {
        use std::io::{Read as _, Write as _};
        if let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) {
            let _ = stream.write_all(b"GET /health HTTP/1.0\r\n\r\n");
            let mut buf = [0u8; 512];
            if let Ok(n) = stream.read(&mut buf) {
                return String::from_utf8_lossy(&buf[..n]).contains("200 OK");
            }
        }
        false
    }

    #[test]
    #[ignore = "launches the local Camoufox browser runtime"]
    fn camoufox_worker_starts_ready_without_cjk_errors() -> crate::errors::AppResult<()> {
        use crate::domain::environment::{BrowserKind, EnvironmentMode};
        use crate::domain::proxy::ProxyConfig;
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        let data_dir = std::env::temp_dir().join(format!(
            "orbit-camoufox-smoke-{}",
            uuid::Uuid::new_v4()
        ));
        let profile_dir = data_dir.join("profile");
        std::fs::create_dir_all(&profile_dir)?;
        let control_port = crate::browser::port_allocator::allocate()?;
        let env = Environment {
            id: "env_camoufox_smoke".to_string(),
            name: "Camoufox Smoke".to_string(),
            group_id: None,
            tags: Vec::new(),
            notes: None,
            browser_kind: BrowserKind::Camoufox,
            chrome_path_override: None,
            profile_dir: profile_dir.to_string_lossy().to_string(),
            proxy_config: ProxyConfig::default(),
            locale: "zh-CN".to_string(),
            timezone_id: None,
            geolocation_latitude: None,
            geolocation_longitude: None,
            user_agent: None,
            platform: None,
            web_rtc_protection: true,
            viewport_width: 900,
            viewport_height: 700,
            device_scale_factor: 1.0,
            environment_mode: EnvironmentMode::Standard,
            seed: None,
            headless: true,
            start_url: Some("about:blank".to_string()),
            last_proxy_test: None,
            created_at: String::new(),
            updated_at: String::new(),
            deleted_at: None,
        };
        let launch_plan = build(&data_dir, &env, &profile_dir, control_port, "zh-CN")?;
        let profile_json: Value =
            serde_json::from_str(&std::fs::read_to_string(&launch_plan.profile_json_path)?)?;
        assert!(
            profile_json["cjk_fonts"]
                .as_array()
                .is_some_and(|families| !families.is_empty()),
            "worker profile must inject host CJK fonts for zh-CN locale"
        );
        let python_path = crate::browser::camoufox_locator::resolve(None)?;
        let log = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&launch_plan.log_path)?;
        let stderr_log = log.try_clone()?;
        let mut child = Command::new(&python_path)
            .arg(&launch_plan.worker_script_path)
            .arg(&launch_plan.profile_json_path)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr_log))
            .spawn()
            .expect("worker should spawn");

        let deadline = Instant::now() + Duration::from_secs(120);
        let mut ready = false;
        let mut early_exit: Option<std::process::ExitStatus> = None;
        while Instant::now() < deadline {
            if let Some(status) = child.try_wait()? {
                early_exit = Some(status);
                break;
            }
            if health_ok(control_port) {
                ready = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(400));
        }
        let _ = child.kill();
        let _ = child.wait();
        let log_text = std::fs::read_to_string(&launch_plan.log_path).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&data_dir);

        if let Some(status) = early_exit {
            panic!(
                "worker exited before ready with {status}; log:\n{log_text}"
            );
        }
        assert!(ready, "worker did not become ready; log:\n{log_text}");
        assert!(
            !log_text.contains("NameError"),
            "worker log contains an unhandled NameError:\n{log_text}"
        );
        Ok(())
    }
}
