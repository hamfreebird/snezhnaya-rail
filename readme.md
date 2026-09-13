# ❄️ 至冬国铁路 · Snezhnaya Rail

> 基于《原神》世界观，仿 12306 的在线铁路售票系统  

![Rust](https://img.shields.io/badge/Rust-1.85+-orange?logo=rust)
![Axum](https://img.shields.io/badge/Axum-0.7-blue)
![License](https://img.shields.io/badge/License-MIT-green)

---

## 项目简介

**至冬国铁路** 是一款以《原神》中“至冬”为背景的 Web 应用，完整模拟了 12306 铁路售票系统的核心业务逻辑。
项目融合了**动态余票算法**、**中转换乘**、**改签/退签**、**纸质车票生成**等高级功能，并采用了至冬主题的毛玻璃视觉风格。

---

## 技术栈

### 后端
- **语言**：Rust 1.85+
- **Web 框架**：Axum 0.7
- **模板引擎**：Tera 1.19
- **时间处理**：Chrono 0.4
- **序列化**：Serde + Serde JSON
- **CSV 解析**：Csv 1.3
- **随机数**：Rand 0.8
- **UUID**：Uuid 1.x
- **图像处理**：Image 0.24 + Imageproc 0.23 + QRCode 0.12 + ab_glyph 0.2
- **限流**：Tower-Governor 0.8
- **日志**：Tracing 0.1 + Tracing Appender 0.2
- **配置管理**：Config 0.14
- **异步运行时**：Tokio

### 前端
- **纯 HTML + CSS + JavaScript**（无框架）
- **图标**：Font Awesome 6

---

## 项目结构

```
snezhnaya-rail/
├── Cargo.toml                  # 项目依赖
├── README.md
├── config.toml                 # 外部配置（可选，不存在则使用默认值）
├── trains.csv                  # 车次数据（核心）
├── notifications.txt           # 通知内容
├── orders.json                 # 订单持久化（自动生成）
├── order_index.json            # 订单索引（自动生成）
├── sold_counts.json            # 已售座位持久化（自动生成）
├── src/
│   ├── main.rs                 # 入口文件
│   ├── config.rs               # 配置结构体与加载逻辑
│   ├── state.rs                # 应用状态（AppState）
│   ├── cleanup.rs              # 订单超时清理任务
│   ├── seats.rs                # 座位分配与释放
│   ├── ticket.rs               # 车票生成（图片）
│   ├── transfer.rs             # 换乘算法
│   ├── db/                     # 数据持久化
│   │   ├── mod.rs
│   │   ├── csv_loader.rs       # 加载 trains.csv / notifications.txt
│   │   └── order_store.rs      # 加载/保存 orders.json / order_index.json
│   ├── models/                 # 数据结构
│   │   ├── mod.rs
│   │   ├── train.rs
│   │   ├── order.rs
│   │   └── notification.rs
│   ├── routes/                 # 路由处理函数
│   │   ├── mod.rs              # 路由注册
│   │   ├── index.rs            # 首页
│   │   ├── search.rs           # 搜索（直达+换乘）
│   │   ├── order.rs            # 订单填写、创建、换乘订单
│   │   ├── payment.rs          # 支付确认与完成
│   │   ├── exchange.rs         # 改签/退签
│   │   ├── tickets.rs          # 车票打印
│   │   ├── detail.rs           # 车次详情
│   │   ├── diagram.rs          # 运行图
│   │   ├── admin.rs            # 管理端点（热重载）
│   │   ├── my_orders.rs        # 我的订单
│   │   └── transfer_confirm.rs # 换乘订单确认
│   └── utils/                  # 工具函数
│       ├── mod.rs
│       ├── logging.rs          # 日志初始化
│       ├── fonts.rs            # 字体加载
│       └── helpers.rs          # 通用辅助函数
├── templates/                  # Tera 模板
│   ├── base.html               # 基础布局
│   ├── index.html              # 首页
│   ├── search_results.html     # 搜索结果（直达/换乘）
│   ├── order_form.html         # 订单填写
│   ├── order_success.html      # 占座成功
│   ├── payment.html            # 支付确认
│   ├── order_result.html       # 订单详情
│   ├── order_query.html        # 订单查询
│   ├── my_orders.html          # 我的订单
│   ├── exchange.html           # 改签/退签查询
│   ├── exchange_detail.html    # 改签/退签详情
│   ├── exchange_result.html    # 操作结果
│   ├── tickets.html            # 车票打印列表
│   ├── transfer_confirm.html   # 换乘订单确认
│   ├── order_transfer_form.html # 换乘订单填写
│   ├── notifications.html      # 通知公告
│   ├── diagram.html            # 运行图
│   └── detail.html             # 车次详情
└── static/                     # 静态资源
    ├── css/
    │   └── style.css           # 全局样式
    ├── js/
    │   └── countdown.js        # 倒计时脚本
    ├── fonts/                  # 中文字体（可选）
    └── images/                 # 背景、运行图等
```

---

## 核心功能

- **车次管理**：从 CSV 读取车次、站点、时间、票价、权重、座位数、运行日、站台、停靠时间等，支持一周七天周期。
- **动态余票**：基于距离发车天数/小时和区间权重实时计算余票（第 2 天 20%，第 3 天 60%，第 4 天 90%……发车前 2 小时快速下降）。
- **中转换乘**：支持一次换乘方案搜索，按总时长排序，显示票价和余票，一键预订。
- **订单管理**：占座、支付、车票打印、订单查询、我的订单（按用户名索引）。
- **改签/退签**：支持改签（自动计算差价）、退签（释放余票），并同步更新座位和订单索引。
- **纸质车票**：生成蓝白渐变背景的 PNG 图片，包含二维码、座位号、乘车信息等。
- **限流保护**：基于 IP 的请求频率限制（每秒 10 次，突发 30 次）。
- **订单超时自动清理**：后台任务定期扫描过期未支付订单，自动取消并释放座位，避免座位泄漏。
- **原子持久化与自愈**：座位分配与订单保存在同一临界区完成，失败自动回滚；启动时从 sold_counts.json 恢复已售座位，保证数据一致性。
- **配置外部化**：所有路径、超时、业务规则、安全参数均可通过 config.toml 或环境变量（SNEZ_前缀）调整，无需重新编译。
- **热重载**：每 30 秒自动检测 `trains.csv` 和 `notifications.txt` 变更并重新加载。
- **日志记录**：使用 `tracing` 记录控制台和文件日志（按日轮转）。

---

## 快速开始

### 1. 克隆仓库
```bash
git clone https://github.com/your-username/snezhnaya-rail.git
cd snezhnaya-rail
```

### 2. 准备数据
- 编辑 `trains.csv` 按示例格式填写车次数据（12 列，见项目根目录示例）。
- 编辑 `notifications.txt` 添加通知（每两行为一组：标题 + 内容）。
- 编辑 `config.toml` 修改配置（可选）。

### 3. 安装 Rust
确保已安装 [Rust](https://rustup.rs/)。

### 4. 运行
```bash
cargo run
```
服务启动后访问 `http://localhost:3000`。若使用了自定义的端口配置则替换为你自定义的端口号。

---

## 许可证

MIT License © 2026

---

## 致谢

- 背景图片来自《原神》官网。
- 火车配线图素材来自 B站 up主 @Genshigros（UID:1740836015）。

---

**祝您旅途愉快！**