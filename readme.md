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
- **异步**：Tokio

### 前端
- **纯 HTML + CSS + JavaScript**（无框架）
- **图标**：Font Awesome 6

---

## 项目结构

```
snezhnaya-rail/
├── src/
│   └── main.rs                 # 全部后端代码（路由、逻辑、数据）
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
├── static/                     # 静态资源
│   ├── css/
│   │   └── style.css           # 全局样式
│   ├── js/
│   │   └── countdown.js        # 倒计时脚本
│   ├── fonts/                  # 中文字体（可选）
│   └── images/                 # 背景、运行图等
├── trains.csv                  # 车次数据（核心）
├── notifications.txt           # 通知内容
├── orders.json                 # 订单持久化（自动生成）
├── order_index.json            # 订单索引（自动生成）
├── Cargo.toml
└── README.md
```

---

## 快速开始

### 1. 克隆仓库
```bash
git clone https://github.com/your-username/snezhnaya-rail.git
cd snezhnaya-rail
```

### 2. 准备数据
- 编辑 `trains.csv` 按示例格式填写车次数据（12 列）。
- 编辑 `notifications.txt` 添加通知（两行为一组：标题 + 内容）。

### 3. 安装 Rust
确保已安装 [Rust](https://rustup.rs/)。

### 4. 运行
```bash
cargo run
```
服务启动后访问 `http://localhost:3000`。

---

## 许可证

MIT License © 2026

---

## 致谢

- 背景图片来自《原神》官网。
- 火车配线图素材来自 B站 up主 @Genshigros（UID:1740836015）。

---

**祝您旅途愉快！**