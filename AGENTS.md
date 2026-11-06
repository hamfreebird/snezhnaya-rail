# AGENTS.md

Rust/Axum 0.8 server-rendered railway booking simulator: single binary crate, Tera templates, vanilla JS, Chinese UI. No frontend build step.

## Communication

- 与用户沟通时始终使用简体中文，包括进度说明、总结、错误解释和提问。

`readme.md` is partly stale: it claims Axum 0.7 (actual `0.8.9`, edition 2024), and claims IP rate limiting that is not wired up (`security.rate_limit_*` are unused; no governor layer is registered).

## Commands

- Run from the repo root (all default paths are relative to CWD): `cargo run` serves on `http://localhost:3000`.
- Fast verification: `cargo check`. Baseline is 4 dead-code warnings (unused config fields and `update_order_index_async`).
- There is no test suite, CI, or lint/format config; manual flow testing is the only verification.
- To test without touching live state or the default port:

  ```bash
  SNEZ_SERVER__PORT=3001 \
  SNEZ_PATHS__ORDERS_JSON=/tmp/snez-orders.json \
  SNEZ_PATHS__ORDER_INDEX_JSON=/tmp/snez-order-index.json \
  SNEZ_PATHS__SOLD_COUNTS_JSON=/tmp/snez-sold-counts.json \
  cargo run
  ```

- Port 3000 may already be occupied by a release instance started via the untracked/ignored `run_server.sh` (`cargo run --release` + tailscale funnel; PID in `.cargo.pid`, logs in `server.log`). Check before killing or rebinding.

## Configuration and reload behavior

- Merge order is env `SNEZ_*` > `config.toml` > code defaults. Nested keys use `__` (`SNEZ_SERVER__PORT=8080`); `SNEZ_CONFIG_FILE` selects a different config file. Config changes require a restart.
- `trains.csv` and `notifications.txt` hot-reload every 30 seconds. Templates are parsed once at startup, so template edits require a restart; static files are served from disk.
- `/admin/reload?token=...` only works when `security.admin_token` is non-empty.

## Architecture invariants

- Entrypoint is `src/main.rs`; routes are hand-registered in `src/routes/mod.rs` — add new handlers there. Handlers render Tera HTML, except AJAX endpoints that return JSON.
- `trains.csv` has 12 columns. Each row expands into one `Train` per run day, with id `{base_id}-{run_day}` (`1` = Monday … `7` = Sunday). For N stations, `prices`/`weights` need N-1 entries and `platforms`/`stop_mins` need N; any mismatch panics during load.
- Seat inventory is duplicated in `Train.available_seats[interval]` and `AppState.sold_counts[(train_id, interval)]`. Every allocation, release, refund, rollback, and exchange must update both, then refresh `Train.seats` via `compute_interval_seats_with_date`.
- When holding multiple state locks, preserve the order `trains -> sold_counts -> orders -> order_index` (documented in `src/routes/exchange.rs`); violating it risks deadlock.
- `sold_counts` has no travel-date component and is never aged out. Because ids repeat by weekday, the same weekday instance across dates/weeks shares inventory and past sales keep reducing it. Changing this requires coordinated edits to startup loading, booking, cleanup, refund, and exchange.
- Search/order handlers call `refresh_trains_with_date`, which globally rewrites `seats`/`is_selling` for the requested date; state is date-sensitive and not cached per calendar date.
- `create_order` trusts form-supplied `total_price` and `from_platform`. If you change pricing or platform logic, update both `src/routes/order.rs` and `templates/order_form.html`.
- Ticket PNG generation panics if no font in `paths.font_paths` loads. `static/fonts/msyh.ttf` is the committed fallback CJK font.

## Data files and leftovers

- `orders.json`, `order_index.json`, and `sold_counts.json` are gitignored live state, loaded at startup and written atomically via temp file + rename. Back them up or redirect them before manual testing.
- `src/main.rs.backup` is a stale pre-refactor monolith; ignore it.
