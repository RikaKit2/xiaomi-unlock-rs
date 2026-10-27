use chrono::{Datelike, FixedOffset, TimeZone, Utc};
use clap::{Parser, Subcommand};
use reqwest::header::{CONTENT_TYPE, COOKIE, HeaderMap, HeaderValue, USER_AGENT};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::net::UdpSocket;
use tokio::time::sleep;

const API_HOST: &str = "sgp-api.buy.mi.com";
const STATUS_URL: &str = "https://sgp-api.buy.mi.com/bbs/api/global/user/bl-switch/state";
const APPLY_URL: &str = "https://sgp-api.buy.mi.com/bbs/api/global/apply/bl-auth";

const NTP_SERVERS: &[&str] = &[
    "time.cloudflare.com:123",
    "pool.ntp.org:123",
    "time.google.com:123",
    "time.android.com:123",
];

#[derive(Parser, Debug)]
#[command(name = "xiaomi-unlock")]
#[command(about = "High-performance Xiaomi HyperOS Bootloader Unlocker in Rust", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Проверить статус аккаунта и токена
    Status,
    /// Синхронизировать NTP и выполнить автоматическую подачу заявки в 23:59:59.750 по Пекину
    Run {
        /// Количество параллельных запросов в burst-пакете (по умолчанию 10)
        #[arg(short, long, default_value_t = 10)]
        burst: usize,
    },
}

#[derive(Deserialize, Serialize, Debug)]
struct Config {
    #[serde(default)]
    cookie: String,
    #[serde(default)]
    skip_cookie_check: bool,
}

#[derive(Deserialize, Debug)]
struct ApiResponse<T> {
    code: i64,
    #[serde(default)]
    message: String,
    data: Option<T>,
}

#[derive(Deserialize, Debug)]
struct StatusData {
    is_pass: Option<i64>,
    button_state: Option<i64>,
    #[serde(default)]
    deadline_format: String,
}

#[derive(Deserialize, Debug)]
struct ApplyData {
    apply_result: Option<i64>,
    #[serde(default)]
    deadline_format: String,
}

fn load_token() -> Result<String, String> {
    let home =
        std::env::var("HOME").map_err(|_| "Не удалось получить переменную $HOME".to_string())?;
    let path = PathBuf::from(home)
        .join(".config")
        .join("xiaomi-unlock-tool")
        .join("config.json");

    if !path.exists() {
        return Err(format!(
            "Файл конфигурации не найден по пути: {}",
            path.display()
        ));
    }

    let content = fs::read_to_string(&path)
        .map_err(|e| format!("Не удалось прочитать {}: {}", path.display(), e))?;

    let cfg: Config = serde_json::from_str(&content)
        .map_err(|e| format!("Ошибка парсинга JSON в {}: {}", path.display(), e))?;

    let token = cfg.cookie.trim().to_string();
    if token.is_empty() {
        return Err(format!("Поле 'cookie' пустое в {}", path.display()));
    }

    Ok(token)
}

fn generate_device_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seed = format!("{}-{}", now.as_nanos(), rand_pseudo());
    let mut hasher = Sha1::new();
    hasher.update(seed.as_bytes());
    let result = hasher.finalize();
    result.iter().map(|b| format!("{:02X}", b)).collect()
}

fn rand_pseudo() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    nanos.wrapping_mul(6364136223846793005).wrapping_add(1)
}

fn build_headers(token: &str, device_id: &str) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    let cookie_val = format!(
        "new_bbs_serviceToken={};versionCode=500411;versionName=5.4.11;deviceId={};",
        token, device_id
    );
    headers.insert(USER_AGENT, HeaderValue::from_static("okhttp/4.12.0"));
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    headers.insert(
        COOKIE,
        HeaderValue::from_str(&cookie_val)
            .map_err(|e| format!("Некорректный заголовок Cookie: {}", e))?,
    );
    Ok(headers)
}

fn beijing_timezone() -> FixedOffset {
    FixedOffset::east_opt(8 * 3600).expect("UTC+8 timezone")
}

async fn fetch_ntp_offset() -> Option<Duration> {
    for &server in NTP_SERVERS {
        if let Ok(addr) = tokio::net::lookup_host(server).await {
            let mut resolved: Vec<SocketAddr> = addr.collect();
            if let (Some(sock_addr), Ok(socket)) =
                (resolved.pop(), UdpSocket::bind("0.0.0.0:0").await)
            {
                let mut packet = [0u8; 48];
                packet[0] = 0x1B; // NTP v3 client request
                let send_res = socket.send_to(&packet, sock_addr).await;
                if send_res.is_err() {
                    continue;
                }
                let mut buf = [0u8; 48];
                let recv_timeout =
                    tokio::time::timeout(Duration::from_millis(2000), socket.recv_from(&mut buf))
                        .await;

                if let Ok(Ok((size, _))) = recv_timeout {
                    if size < 48 {
                        continue;
                    }
                    let secs = u32::from_be_bytes([buf[40], buf[41], buf[42], buf[43]]) as u64;
                    let frac = u32::from_be_bytes([buf[44], buf[45], buf[46], buf[47]]) as u64;
                    if secs > 2208988800 {
                        let unix_secs = secs - 2208988800;
                        let unix_nanos = (frac * 1_000_000_000) >> 32;
                        let ntp_time = Duration::new(unix_secs, unix_nanos as u32);
                        println!(
                            "[NTP] Синхронизировано с {}: {:?} (Unix timestamp)",
                            server, ntp_time
                        );
                        return Some(ntp_time);
                    }
                }
            }
        }
    }
    None
}

async fn check_status(client: &reqwest::Client, token: &str) -> Result<bool, String> {
    let device_id = generate_device_id();
    let headers = build_headers(token, &device_id)?;

    println!("[*] Запрос состояния аккаунта (GET {})...", STATUS_URL);
    let resp = client
        .get(STATUS_URL)
        .headers(headers)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("Ошибка сетевого запроса: {}", e))?;

    let status_code = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("Ошибка чтения ответа: {}", e))?;

    if !status_code.is_success() {
        return Err(format!("HTTP статус {} от сервера: {}", status_code, text));
    }

    let parsed: ApiResponse<StatusData> = serde_json::from_str(&text)
        .map_err(|e| format!("Некорректный JSON ответа: {}\nТело: {}", e, text))?;

    match parsed.code {
        0 => {
            if let Some(data) = parsed.data {
                let is_pass = data.is_pass.unwrap_or(-1);
                let btn = data.button_state.unwrap_or(-1);
                println!(
                    "[+] Ответ сервера: code=0, is_pass={}, button_state={}, deadline='{}'",
                    is_pass, btn, data.deadline_format
                );
                if is_pass == 4 && btn == 1 {
                    println!("[+] Аккаунт готов к подаче заявки.");
                    Ok(true)
                } else if is_pass == 4 && btn == 2 {
                    println!(
                        "[!] Аккаунт временно заблокирован до: {}",
                        data.deadline_format
                    );
                    Ok(false)
                } else if is_pass == 4 && btn == 3 {
                    println!("[!] Аккаунт ещё не готов (менее 30 дней в Mi Community).");
                    Ok(false)
                } else if is_pass == 1 {
                    println!(
                        "[+] Аккаунт уже одобрен! Действителен до: {}",
                        data.deadline_format
                    );
                    Ok(false)
                } else {
                    println!(
                        "[?] Неизвестное состояние: is_pass={}, button_state={}",
                        is_pass, btn
                    );
                    Ok(false)
                }
            } else {
                println!("[+] Ответ сервера: code=0 (нет payload data)");
                Ok(false)
            }
        }
        100004 | 10004 => {
            println!(
                "[-] Ошибка авторизации: Cookie/serviceToken устарел или недействителен (code={})",
                parsed.code
            );
            Ok(false)
        }
        other => {
            println!(
                "[-] Ответ сервера: code={}, message='{}'",
                other, parsed.message
            );
            Ok(false)
        }
    }
}

async fn prewarm_connections(client: &reqwest::Client, token: &str, count: usize) {
    println!(
        "[*] Запуск pre-warm прогрева пула соединений ({} запросов к {})...",
        count, API_HOST
    );
    let mut handles = Vec::with_capacity(count);
    for _ in 0..count {
        let client = client.clone();
        let token = token.to_string();
        handles.push(tokio::spawn(async move {
            let dev_id = generate_device_id();
            if let Ok(headers) = build_headers(&token, &dev_id) {
                let _ = client
                    .get(STATUS_URL)
                    .headers(headers)
                    .timeout(Duration::from_millis(3000))
                    .send()
                    .await;
            }
        }));
    }
    for h in handles {
        let _ = h.await;
    }
    println!("[+] Pre-warm завершён. TLS 1.3 сокеты установлены в пуле.");
}

async fn run_burst(client: &reqwest::Client, token: &str, burst_count: usize) -> bool {
    let bj_tz = beijing_timezone();
    println!("============================================================");
    println!("  XIAOMI HYPEROS BURST UNLOCKER (RUST EDITION)");
    println!("============================================================");

    // 1. NTP sync & time calculation
    println!("[*] Калибровка системного времени с NTP серверами...");
    let local_now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let ntp_now = fetch_ntp_offset().await.unwrap_or(local_now);
    let offset_millis = (ntp_now.as_millis() as i128) - (local_now.as_millis() as i128);
    println!(
        "[+] Смещение часов хоста относительно NTP: {} мс",
        offset_millis
    );

    // Calculate next 23:59:59.750 Beijing time
    let now_utc = Utc::now();
    let now_bj = now_utc.with_timezone(&bj_tz);
    println!(
        "[*] Текущее время Пекина: {}",
        now_bj.format("%Y-%m-%d %H:%M:%S%.3f")
    );

    // Target: today at 23:59:59.750 Beijing, or tomorrow if already past
    let mut target_bj = bj_tz
        .with_ymd_and_hms(now_bj.year(), now_bj.month(), now_bj.day(), 23, 59, 59)
        .single()
        .expect("Valid datetime")
        + chrono::Duration::milliseconds(750);

    if now_bj >= target_bj {
        target_bj += chrono::Duration::days(1);
    }

    let time_until_target = (target_bj - now_bj).to_std().unwrap_or(Duration::ZERO);
    println!(
        "[*] Целевое время дедлайна: {} (до запуска: {:.3} сек)",
        target_bj.format("%Y-%m-%d %H:%M:%S%.3f"),
        time_until_target.as_secs_f64()
    );

    // 2. Wait until target minus 5 seconds for pre-warm
    if time_until_target > Duration::from_secs(6) {
        let sleep_until_prewarm = time_until_target - Duration::from_secs(5);
        println!(
            "[*] Ожидание начала фазы прогрева (сон {:.1} сек)...",
            sleep_until_prewarm.as_secs_f64()
        );
        sleep(sleep_until_prewarm).await;
    }

    // 3. Pre-warm TLS connection pool
    prewarm_connections(client, token, 8).await;

    // 4. Fine-grained wait for the exact millisecond
    let remaining_before_burst = {
        let current_bj = Utc::now().with_timezone(&bj_tz);
        (target_bj - current_bj).to_std().unwrap_or(Duration::ZERO)
    };

    if remaining_before_burst > Duration::from_millis(50) {
        println!(
            "[*] Точный отсчет до дедлайна: {:.3} сек...",
            remaining_before_burst.as_secs_f64()
        );
        sleep(remaining_before_burst - Duration::from_millis(15)).await;
    }

    // Spin-wait for the last milliseconds
    while Utc::now().with_timezone(&bj_tz) < target_bj {
        std::hint::spin_loop();
    }

    let fired_at = Utc::now().with_timezone(&bj_tz);
    println!(
        "[🚀 BURST] Время достигнуто: {}! Выпуск {} параллельных запросов...",
        fired_at.format("%H:%M:%S%.3f"),
        burst_count
    );

    // 5. Fire parallel burst requests
    let body_json = serde_json::json!({ "is_retry": true });
    let body_bytes = serde_json::to_vec(&body_json).unwrap();
    let approved = Arc::new(AtomicBool::new(false));
    let mut handles = Vec::with_capacity(burst_count);

    let start_instant = Instant::now();
    for i in 0..burst_count {
        let client = client.clone();
        let token = token.to_string();
        let body = body_bytes.clone();
        let approved_flag = Arc::clone(&approved);

        handles.push(tokio::spawn(async move {
            let dev_id = generate_device_id();
            let req_start = Instant::now();
            let headers = match build_headers(&token, &dev_id) {
                Ok(h) => h,
                Err(e) => {
                    println!("[Req #{}] Ошибка сборки заголовков: {}", i, e);
                    return;
                }
            };

            let res = client
                .post(APPLY_URL)
                .headers(headers)
                .body(body)
                .timeout(Duration::from_secs(5))
                .send()
                .await;

            let elapsed = req_start.elapsed();
            match res {
                Ok(resp) => {
                    let text = resp.text().await.unwrap_or_default();
                    if let Ok(parsed) = serde_json::from_str::<ApiResponse<ApplyData>>(&text) {
                        println!(
                            "[Req #{:02} | {:>4}ms] code={}, msg='{}', data={:?}",
                            i,
                            elapsed.as_millis(),
                            parsed.code,
                            parsed.message,
                            parsed.data
                        );
                        if parsed.code == 0 {
                            if let Some(d) = &parsed.data {
                                if d.apply_result == Some(1) {
                                    println!(
                                        "\n🎉 [SUCCESS] Заявка успешно ОДОБРЕНА сервером Xiaomi!"
                                    );
                                    approved_flag.store(true, Ordering::SeqCst);
                                } else if d.apply_result == Some(3) {
                                    println!(
                                        "[-] Лимит исчерпан. Повторите попытку: {}",
                                        d.deadline_format
                                    );
                                }
                            }
                        } else if parsed.code == 100003 {
                            println!(
                                "\n🎉 [SUCCESS] Заявка уже была отправлена или одобрена (100003)!"
                            );
                            approved_flag.store(true, Ordering::SeqCst);
                        }
                    } else {
                        println!(
                            "[Req #{:02} | {:>4}ms] Тело ответа: {}",
                            i,
                            elapsed.as_millis(),
                            text
                        );
                    }
                }
                Err(e) => {
                    println!(
                        "[Req #{:02} | {:>4}ms] Ошибка: {}",
                        i,
                        elapsed.as_millis(),
                        e
                    );
                }
            }
        }));
    }

    for h in handles {
        let _ = h.await;
    }

    let total_elapsed = start_instant.elapsed();
    let is_approved = approved.load(Ordering::SeqCst);
    println!(
        "[*] Пакет запросов завершен за {:?} (Успех: {})",
        total_elapsed, is_approved
    );

    is_approved
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let token = match load_token() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[-] Ошибка загрузки токена: {}", e);
            std::process::exit(1);
        }
    };

    println!(
        "[+] Загружен токен из конфигурации (длина: {} символов)",
        token.len()
    );

    let client = reqwest::Client::builder()
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(32)
        .tcp_nodelay(true)
        .build()
        .expect("Не удалось инициализировать HTTP клиент");

    match cli.command {
        Commands::Status => match check_status(&client, &token).await {
            Ok(ready) => {
                if ready {
                    println!("[✓] Проверка статуса завершена успешно.");
                    std::process::exit(0);
                } else {
                    eprintln!("[!] Статус аккаунта не готов к подаче заявки.");
                    std::process::exit(2);
                }
            }
            Err(e) => {
                eprintln!("[-] Ошибка при проверке статуса: {}", e);
                std::process::exit(1);
            }
        },
        Commands::Run { burst } => {
            println!("[*] Запуск в режиме RUN (burst: {})...", burst);
            let success = run_burst(&client, &token, burst).await;
            if success {
                println!("[✓] Разблокировка успешно подтверждена!");
                std::process::exit(0);
            } else {
                println!("[-] Разблокировка не подтверждена или лимит исчерпан.");
                std::process::exit(1);
            }
        }
    }
}
