#!/bin/bash

# ============================================================
# 至冬国铁路 服务管理脚本
# 功能：启动 cargo 服务 + tailscale funnel，定时重启，防休眠，记录日志
# ============================================================

# ---------- 配置项 ----------
PROJECT_DIR="$(cd "$(dirname "$0")" && pwd)"   # 脚本所在目录
LOG_FILE="$PROJECT_DIR/server.log"            # 日志文件
RESTART_INTERVAL=1800                         # 重启间隔（秒），默认 30 分钟
CARGO_PORT=3000                               # 网站端口
FUNNEL_PORT="$CARGO_PORT"                     # funnel 端口（与网站一致）

# ---------- 颜色输出 ----------
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# ---------- 日志函数 ----------
log() {
    echo -e "[$(date '+%Y-%m-%d %H:%M:%S')] $*" | tee -a "$LOG_FILE"
}

log_info() {
    log "${GREEN}[INFO]${NC} $*"
}

log_warn() {
    log "${YELLOW}[WARN]${NC} $*"
}

log_error() {
    log "${RED}[ERROR]${NC} $*"
}

# ---------- 防止屏幕休眠 ----------
prevent_sleep() {
    if command -v xset &>/dev/null; then
        log_info "正在禁用屏幕节能（xset）..."
        xset s off
        xset -dpms
    elif command -v caffeinate &>/dev/null; then
        log_info "正在启动 caffeinate 防止休眠..."
        caffeinate -d -i -s &
        CAFFEINATE_PID=$!
    else
        log_warn "未找到 xset 或 caffeinate，无法防止屏幕休眠。"
    fi
}

# ---------- 启动服务 ----------
start_services() {
    log_info "启动 cargo 服务..."
    cd "$PROJECT_DIR" || exit 1
    # 在后台启动 cargo，并将输出重定向到日志（同时保留终端输出）
    cargo run --release &
    CARGO_PID=$!
    log_info "cargo 进程 PID: $CARGO_PID"

    # 等待 cargo 启动（简单等待 3 秒）
    sleep 3

    log_info "启动 tailscale funnel..."
    tailscale funnel "$FUNNEL_PORT" &
    FUNNEL_PID=$!
    log_info "tailscale funnel PID: $FUNNEL_PID"

    # 保存 PID 文件以便后续终止
    echo "$CARGO_PID" > "$PROJECT_DIR/.cargo.pid"
    echo "$FUNNEL_PID" > "$PROJECT_DIR/.funnel.pid"
}

# ---------- 停止服务 ----------
stop_services() {
    log_info "停止所有服务..."
    # 读取 PID 并终止
    if [ -f "$PROJECT_DIR/.cargo.pid" ]; then
        CARGO_PID=$(cat "$PROJECT_DIR/.cargo.pid")
        if kill -0 "$CARGO_PID" 2>/dev/null; then
            kill "$CARGO_PID"
            log_info "已终止 cargo 进程 (PID: $CARGO_PID)"
        else
            log_warn "cargo 进程已不存在 (PID: $CARGO_PID)"
        fi
        rm -f "$PROJECT_DIR/.cargo.pid"
    else
        # 如果没有 PID 文件，尝试 pkill
        pkill -f "cargo run" 2>/dev/null && log_info "通过 pkill 终止 cargo"
    fi

    if [ -f "$PROJECT_DIR/.funnel.pid" ]; then
        FUNNEL_PID=$(cat "$PROJECT_DIR/.funnel.pid")
        if kill -0 "$FUNNEL_PID" 2>/dev/null; then
            kill "$FUNNEL_PID"
            log_info "已终止 funnel 进程 (PID: $FUNNEL_PID)"
        else
            log_warn "funnel 进程已不存在 (PID: $FUNNEL_PID)"
        fi
        rm -f "$PROJECT_DIR/.funnel.pid"
    else
        # 如果 PID 文件不存在，尝试 pkill 或 tailscale funnel reset
        tailscale funnel reset 2>/dev/null && log_info "通过 tailscale funnel reset 关闭 funnel"
        pkill -f "tailscale funnel" 2>/dev/null && log_info "通过 pkill 终止 funnel"
    fi
}

# ---------- 重启服务 ----------
restart_services() {
    log_info "========== 开始重启服务 =========="
    stop_services
    sleep 2
    start_services
    log_info "========== 重启完成 =========="
}

# ---------- 清理退出 ----------
cleanup() {
    log_info "捕获到退出信号，执行清理..."
    stop_services
    if [ -n "$CAFFEINATE_PID" ]; then
        kill "$CAFFEINATE_PID" 2>/dev/null
    fi
    log_info "脚本退出。"
    exit 0
}

# ---------- 主程序 ----------
main() {
    trap cleanup SIGINT SIGTERM

    # 检查必要命令
    command -v cargo >/dev/null 2>&1 || { log_error "cargo 未安装"; exit 1; }
    command -v tailscale >/dev/null 2>&1 || { log_error "tailscale 未安装"; exit 1; }

    log_info "========================================"
    log_info "  至冬国铁路 服务管理脚本启动"
    log_info "  项目目录: $PROJECT_DIR"
    log_info "  日志文件: $LOG_FILE"
    log_info "  重启间隔: $((RESTART_INTERVAL / 60)) 分钟"
    log_info "========================================"

    # 确保 tailscale 权限正确
    if ! tailscale funnel status &>/dev/null; then
        log_info "尝试设置 tailscale operator 权限..."
        sudo tailscale set --operator="$USER" 2>/dev/null || log_warn "设置权限失败，可能需要手动执行：sudo tailscale set --operator=$USER"
    fi

    prevent_sleep

    # 首次启动
    start_services

    # 主循环：定时重启
    while true; do
        log_info "等待 $((RESTART_INTERVAL / 60)) 分钟后执行下一次重启..."
        sleep "$RESTART_INTERVAL"
        restart_services
    done
}

# 如果脚本被直接执行，运行主程序
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    main
fi
