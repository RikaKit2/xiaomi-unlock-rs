# xiaomi-unlock-rs ⚡

[![Rust](https://img.shields.io/badge/Rust-Nightly%20%2F%20Stable-DEA584.svg?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![License: GPL-3.0](https://img.shields.io/badge/License-GPL--3.0-blue.svg?style=flat-square)](LICENSE)
[![Tokio](https://img.shields.io/badge/Runtime-Tokio-black.svg?style=flat-square)](https://tokio.rs/)

Ultra-high-performance, sub-millisecond asynchronous Rust engine for automated **Xiaomi HyperOS & MIUI bootloader unlock quota** submissions.

Rewritten from the ground up in Rust for zero GC jitter, pre-warmed TLS 1.3 connections, and atomic Stratum-1 NTP synchronization.

---

## 🚀 Key Advantages Over Python Implementations

Starting with HyperOS, Xiaomi enforces strict global daily quotas (~1,500–2,000 slots worldwide) that reset exactly at **00:00:00 Beijing Time (UTC+8 / 19:00:00 MSK)**. The quota is exhausted globally in less than a second (300–600ms).

`xiaomi-unlock-rs` solves this with systems-level performance optimizations:

* **Pre-warmed TLS 1.3 Connection Pool:** 5 seconds before the quota boundary, the engine establishes persistent HTTPS connections and completes TLS handshakes with `sgp-api.buy.mi.com`. At the target millisecond, POST requests are dispatched over **already-open, encrypted sockets with 0ms TLS handshake latency** (saving ~150–250ms of network RTT).
* **Stratum-1 NTP Atomic Clock Synchronization:** Direct UDP socket queries to global atomic clocks (`time.cloudflare.com`, `pool.ntp.org`, `time.google.com`, `time.android.com`) compute sub-millisecond local system clock drift.
* **Microsecond Spin-Wait:** Eliminates OS sleep scheduler jitter by combining coarse cooperative sleep with `std::hint::spin_loop()` during the final milliseconds.
* **Zero GIL & Zero Garbage Collection:** Pure multithreaded asynchronous dispatch directly to Linux `epoll` via Tokio.

---

## 📦 Installation & Building

### Standard Build (Rust Stable/Nightly)
```bash
git clone https://github.com/RikaKit2/xiaomi-unlock-rs.git
cd xiaomi-unlock-rs
cargo build --release
```

### High-Speed Build (Cranelift + Wild Linker)
This repository is configured out of the box with `.cargo/config.toml` and `rust-toolchain.toml` for Cranelift codegen:
```bash
cargo build --release -j 2
```
The resulting binary will be in `target/release/xiaomi-unlock`.

---

## ⚙️ Configuration

`xiaomi-unlock-rs` reads the standard authorization configuration file:
`~/.config/xiaomi-unlock-tool/config.json`

```json
{
  "cookie": "YOUR_NEW_BBS_SERVICE_TOKEN",
  "skip_cookie_check": false
}
```

> **How to get `serviceToken`:**
> 1. Log in to [Xiaomi Community Global](https://account.xiaomi.com/fe/service/login/password?sid=18n_bbs_global&callback=https://sgp-api.buy.mi.com/bbs/api/global/user/login-back) in your browser.
> 2. Open Developer Tools (F12) $\rightarrow$ Storage / Cookies $\rightarrow$ copy `serviceToken` (or `new_bbs_serviceToken`).

---

## 🛠️ Usage

### 1. Check Account Eligibility (`status`)
Verifies your token and confirms account eligibility against Xiaomi's official API:

```bash
./target/release/xiaomi-unlock status
```

Output:
```text
[+] Загружен токен из конфигурации (длина: 178 символов)
[*] Запрос состояния аккаунта (GET https://sgp-api.buy.mi.com/bbs/api/global/user/bl-switch/state)...
[+] Ответ сервера: code=0, is_pass=4, button_state=1, deadline=''
[+] Аккаунт готов к подаче заявки.
[✓] Проверка статуса завершена успешно.
```

### 2. Automated Quota Submission (`run`)
Calibrates atomic time, pre-warms TLS connections, waits for the reset, and fires a parallel burst:

```bash
./target/release/xiaomi-unlock run --burst 12
```

Options:
* `-b, --burst <BURST>` — Number of parallel requests in the burst packet (default: 10).

---

## 📊 Comparison

| Feature | Python (`miunlocktool`) | `xiaomi-unlock-rs` (Rust) |
| :--- | :--- | :--- |
| **TLS Handshake at boundary** | ~150–250ms (on trigger) | **0ms (Pre-warmed pool)** |
| **Timer Jitter** | 5–20ms (GIL / OS sleep) | **< 1ms (Spin-loop + Monotonic)** |
| **Memory & CPU footprint** | ~50–90 MB | **< 5 MB** |
| **Parallel I/O Engine** | Threading + requests (blocking) | **Tokio asynchronous non-blocking epoll** |

---

## 📜 Credits & License

* Based on reverse-engineered Xiaomi Community APIs and protocols documented by the 4PDA / XDA communities and [AsInsideOut/miunlocktool](https://github.com/AsInsideOut/miunlocktool).
* Licensed under the [GNU General Public License v3.0](LICENSE).
