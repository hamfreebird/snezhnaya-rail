#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
列车运行图绘制脚本（多线路版）
==============================

输入 : CSV 车次信息文件（含 车次/列车型号/编组/始发站-经停-终到站列表/
        始发时间-经停-终到时间列表/运行日/站台列表/停靠时间列表/
        列车名称/是否为标杆车次 等字段）

输出 : 按线路分别生成 PNG 列车运行图。默认把同一线路上运行日相同的车次
        合并到一张图；使用 --per-day 可改为逐日一张图。

线路 : 南环线、北环线、外环线、东线、中央—新基捷、新基捷支线。
        脚本按车次经过的站点自动归类；无法归类的车次进入“其他线路”。

纵轴 : 按线路资料中的实际站间距累计，不再依赖 CSV 首列车的站序。
        环线（南环/北环/外环）和矿区三角支线会把闭合区间展开到纵轴两端，
        因此同一个站名可能在纵轴顶部和底部各出现一次。

样式 : 所有运行线统一为红色实线，不区分车型、方向和高平峰；
        背景透明，文字白色，所有数据标签为黑底白字；
        图例只说明统一运行线，不区分高平峰，也不标注站间闭塞。

用法 :
    python3 scripts/train_diagram.py trains.csv
    python3 scripts/train_diagram.py trains.csv /tmp/diagrams --per-day
    python3 scripts/train_diagram.py trains.csv /tmp/diagrams --line 东线
    python3 scripts/train_diagram.py trains.csv /tmp/diagrams --annotations full
    python3 scripts/train_diagram.py --list-lines
"""

import argparse
import csv
import os
import re
from collections import defaultdict

import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.lines import Line2D

# ---------------------------------------------------------------- 中文字体
plt.rcParams["font.sans-serif"] = [
    "Noto Sans CJK SC", "Source Han Sans SC", "WenQuanYi Zen Hei",
    "Microsoft YaHei", "SimHei", "Arial Unicode MS", "DejaVu Sans",
]
plt.rcParams["axes.unicode_minus"] = False

# ---------------------------------------------------------------- 缺省参数
CFG = {
    # 线路
    "eff_len_default": 850,                # 到发线标准有效长 (m)

    # 技术站（在此停站记为“技术作业时间”）
    "tech_stations": {"中央车站"},

    # 列车类型 -> 速度 / 说明（线路资料：普快 60 km/h，直特 80 km/h）
    "train_type_info": {
        "普快": {"loco": "普通旅客列车", "speed": 60},
        "直特": {"loco": "直达特快列车", "speed": 80},
        "快速": {"loco": "快速旅客列车", "speed": 60},
        "特快": {"loco": "特别快速旅客列车", "speed": 80},
    },
}

# ---------------------------------------------------------------- 画面配色
TRAIN_COLOR = "#FF2D2D"       # 所有列车统一红色实线
TEXT_COLOR = "#FFFFFF"        # 所有文字白色
LABEL_BG = "#000000"          # 标签黑底
LABEL_EDGE = "#FFFFFF"        # 标签白色细边
GRID_COLOR = "#FFFFFF"        # 透明背景上的白色网格 / 站线

DAY_NAMES = ["星期一", "星期二", "星期三", "星期四", "星期五", "星期六", "星期日"]

# ---------------------------------------------------------------- 站名简写
SHUGUANG = "曙光车站"
ZHONGYANG = "中央车站"
CAIBING = "彩冰镇"
NINGLU = "凝露镇"
HAIXIE = "海屑镇"
BAIMIANGONG = "白冕宫"
XUEHUA = "雪桦车站"
YUNSHUJU = "运输局"
BEILUO = "贝洛沃迪港"
XINJIJIE = "新基捷城"
KUANGQU = "矿区"

# ---------------------------------------------------------------- 线路定义
# stations : 正向站序（站名, 累计里程 km）。首尾同名表示环线/三角闭合。
# forward_label / reverse_label : 正向 / 反向的运行方向名称。
# closing_label : 最后一个区间是否为闭合区间（用于左侧标注）。
# tracks : 正线说明。
LINE_DEFS = [
    {
        "name": "南环线",
        "stations": [
            (SHUGUANG, 0), (ZHONGYANG, 55), (CAIBING, 105),
            (NINGLU, 175), (SHUGUANG, 220),
        ],
        "forward_label": "顺时针",
        "reverse_label": "逆时针",
        "forward_key": "down",
        "reverse_key": "up",
        "closing_label": "环线闭合",
        "tracks": "单线",
        "description": "曙光—中央—彩冰—凝露—曙光（环线，220 km）",
    },
    {
        "name": "北环线",
        "stations": [
            (SHUGUANG, 0), (ZHONGYANG, 55), (HAIXIE, 95),
            (SHUGUANG, 155),
        ],
        "forward_label": "顺时针",
        "reverse_label": "逆时针",
        "forward_key": "down",
        "reverse_key": "up",
        "closing_label": "环线闭合",
        "tracks": "单线",
        "description": "曙光—中央—海屑—曙光（环线，155 km）",
    },
    {
        "name": "外环线",
        "stations": [
            (SHUGUANG, 0), (HAIXIE, 60), (CAIBING, 145),
            (NINGLU, 215), (SHUGUANG, 260),
        ],
        "forward_label": "顺时针",
        "reverse_label": "逆时针",
        "forward_key": "down",
        "reverse_key": "up",
        "closing_label": "环线闭合",
        "tracks": "单线",
        "description": "曙光—海屑—彩冰—凝露—曙光（外环线，260 km）",
    },
    {
        "name": "东线",
        "stations": [
            (ZHONGYANG, 0), (BAIMIANGONG, 30), (XUEHUA, 85),
            (YUNSHUJU, 135), (BEILUO, 190),
        ],
        "forward_label": "下行",
        "reverse_label": "上行",
        "forward_key": "down",
        "reverse_key": "up",
        "closing_label": None,
        "tracks": "上下行分离",
        "description": "中央—白冕宫—雪桦—运输局—贝洛沃迪港（190 km）",
    },
    {
        "name": "中央—新基捷",
        "stations": [
            (ZHONGYANG, 0), (BAIMIANGONG, 30), (XUEHUA, 85),
            (YUNSHUJU, 135), (XINJIJIE, 160), (KUANGQU, 180),
        ],
        "forward_label": "下行",
        "reverse_label": "上行",
        "forward_key": "down",
        "reverse_key": "up",
        "closing_label": None,
        "tracks": "上下行分离（支线单线）",
        "description": "中央—白冕宫—雪桦—运输局—新基捷—矿区（180 km）",
    },
    {
        "name": "新基捷支线",
        "stations": [
            (BEILUO, 0), (YUNSHUJU, 55), (XINJIJIE, 80),
            (KUANGQU, 100), (YUNSHUJU, 120),
        ],
        "forward_label": "上行",
        "reverse_label": "下行",
        "forward_key": "up",
        "reverse_key": "down",
        "closing_label": "三角闭合",
        "tracks": "单线",
        "description": "贝洛沃迪港—运输局—新基捷—矿区（三角支线，120 km）",
    },
]

LINE_BY_NAME = {line["name"]: line for line in LINE_DEFS}


# ================================================================ 工具函数
def hm_to_min(s):
    """'HH:MM' -> 分钟"""
    h, m = s.strip().split(":")
    return int(h) * 60 + int(m)


def min_to_hm(v):
    """分钟 -> 'HH:MM'"""
    v = int(round(v)) % (24 * 60)
    return f"{v // 60:02d}:{v % 60:02d}"


def min_to_hm_axis(v):
    """时刻轴刻度：24:00 不折回 00:00"""
    if int(round(v)) == 24 * 60:
        return "24:00"
    return min_to_hm(v)


def split_list(s):
    return [x.strip() for x in s.split("-") if x.strip()]


def label_box(pad=0.14, lw=0.35):
    """统一的黑底白字标签样式"""
    return dict(boxstyle=f"round,pad={pad}", fc=LABEL_BG,
                ec=LABEL_EDGE, lw=lw)


def safe_filename(s):
    """把线路名转成适合做文件名的形式"""
    s = s.replace("—", "-")
    s = re.sub(r'[\\/:*?"<>|]', "_", s)
    return s.strip() or "线路"


def day_set_label(days):
    """运行日集合 -> 图名中的文字"""
    days = sorted(set(int(d) for d in days))
    if days == [1, 2, 3, 4, 5, 6, 7]:
        return "全周"
    if len(days) == 1:
        return DAY_NAMES[days[0] - 1]
    return "运行日" + "-".join(str(d) for d in days)


# ================================================================ 解析 CSV
def parse_csv(path):
    trains = []
    with open(path, "r", encoding="utf-8-sig", newline="") as f:
        for row in csv.DictReader(f):
            row = {(k or "").strip(): (v or "").strip() for k, v in row.items()}
            if not row.get("车次"):
                continue

            stations = split_list(row["始发站-经停-终到站列表"])
            times = [hm_to_min(x) for x in split_list(row["始发时间-经停-终到时间列表"])]
            dwell = [int(x) for x in split_list(row["停靠时间列表"])]
            days = [int(x) for x in split_list(row["运行日"])]
            platforms = [int(x) for x in split_list(row["站台列表"])]

            if not stations or len(stations) != len(times):
                raise ValueError(f"车次 {row.get('车次', '?')} 的站序与时刻数量不一致")
            if len(dwell) != len(stations):
                raise ValueError(f"车次 {row.get('车次', '?')} 的停靠时间数量与站序不一致")

            # 跨日修正
            for i in range(1, len(times)):
                while times[i] < times[i - 1]:
                    times[i] += 24 * 60

            arr = times[:]
            dep = [times[i] + dwell[i] for i in range(len(times))]

            consist = int(row["编组"]) if row.get("编组", "").isdigit() else 0

            trains.append({
                "id": row["车次"],
                "type": row.get("列车型号", "普快"),
                "consist": consist,
                "stations": stations,
                "arr": arr,
                "dep": dep,
                "dwell": dwell,
                "days": days,
                "platforms": platforms,
                "name": row.get("列车名称", ""),
                "benchmark": row.get("是否为标杆车次", "0") == "1",
            })
    return trains


# ================================================================ 线路归类
def classify_train(t):
    """按车次经过的站点判断所属线路"""
    s = set(t["stations"])

    if XINJIJIE in s or KUANGQU in s:
        if ZHONGYANG in s:
            return "中央—新基捷"
        return "新基捷支线"

    if BEILUO in s:
        if ZHONGYANG in s:
            return "东线"
        return "新基捷支线"

    if HAIXIE in s:
        if CAIBING in s:
            return "外环线"
        return "北环线"

    if CAIBING in s or NINGLU in s:
        return "南环线"

    if BAIMIANGONG in s:
        return "东线"

    return "其他线路"


def build_other_line(other_trains):
    """为无法归类的车次动态生成一条线路（最长站序 + 缺省 40 km 间距）"""
    base = max(other_trains, key=lambda t: len(t["stations"]))["stations"]
    order = []
    for st in base:
        if st not in order:
            order.append(st)
    for t in other_trains:
        for st in t["stations"]:
            if st not in order:
                order.append(st)

    stations = [(order[0], 0)]
    for a, b in zip(order, order[1:]):
        stations.append((b, stations[-1][1] + 40))
    return {
        "name": "其他线路",
        "stations": stations,
        "forward_label": "正向",
        "reverse_label": "反向",
        "forward_key": "down",
        "reverse_key": "up",
        "closing_label": None,
        "tracks": "—",
        "description": "未归类车次（纵轴间距按缺省 40 km 估算）",
    }


def group_trains(trains):
    groups = defaultdict(list)
    for t in trains:
        groups[classify_train(t)].append(t)
    return groups


def get_line_def(name, trains):
    if name in LINE_BY_NAME:
        return LINE_BY_NAME[name]
    return build_other_line(trains)


def ordered_group_items(groups):
    """按 LINE_DEFS 的顺序输出线路，其他线路放最后"""
    items = []
    for line in LINE_DEFS:
        if line["name"] in groups:
            items.append((line["name"], groups[line["name"]]))
    if "其他线路" in groups:
        items.append(("其他线路", groups["其他线路"]))
    known = {name for name, _ in items}
    for name, line_trains in groups.items():
        if name not in known:
            items.append((name, line_trains))
    return items


# ================================================================ 运行线定位
def find_matches(route, seq):
    """在 route（站名, 里程）中寻找 seq 的保序匹配，返回 (索引列表, 跨度)"""
    matches = []

    def rec(i, indices):
        if i == len(seq):
            ys = [route[j][1] for j in indices]
            matches.append((indices[:], abs(ys[-1] - ys[0])))
            return
        start = indices[-1] + 1 if indices else 0
        for j in range(start, len(route)):
            if route[j][0] == seq[i]:
                indices.append(j)
                rec(i + 1, indices)
                indices.pop()

    rec(0, [])
    return matches


def candidate_routes(line):
    """返回正向、反向、终点折返三种走法

    环线/三角支线的站序首尾重复，如果把“折返”也作为候选，列车会在
    闭合区间里错误地回到起点那一份站名（跨度甚至变成 0）。因此只有
    站名不重复的线路才启用折返走法（当前用于东线 4110）。
    """
    fwd = line["stations"]
    rev = list(reversed(fwd))
    routes = [
        ("forward", fwd, line["forward_key"], line["forward_label"]),
        ("reverse", rev, line["reverse_key"], line["reverse_label"]),
    ]
    if len({st for st, _ in fwd}) == len(fwd):
        outback = fwd + rev[1:]
        routes.append(("outback", outback, line["forward_key"],
                       line["forward_label"] + "（折返）"))
    return routes


def place_train(t, line):
    """把一辆车次的各站映射到纵轴里程，返回定位结果"""
    seq = t["stations"]
    kind_order = {"forward": 0, "reverse": 1, "outback": 2}
    best = None

    for kind, route, dkey, dtext in candidate_routes(line):
        for indices, span in find_matches(route, seq):
            key = (round(span, 6), kind_order[kind], indices[0])
            if best is None or key < best["key"]:
                best = {
                    "key": key,
                    "kind": kind,
                    "route": route,
                    "indices": indices,
                    "ys": [route[j][1] for j in indices],
                    "dir_key": dkey,
                    "dir_text": dtext,
                }

    if best is None:
        # 兜底：按站名第一次出现的位置放置
        ymap = {}
        for st, y in line["stations"]:
            ymap.setdefault(st, y)
        best = {
            "kind": "forward",
            "indices": [],
            "ys": [ymap.get(st, 0) for st in seq],
            "dir_key": line["forward_key"],
            "dir_text": line["forward_label"],
        }

    # 矿区三角：只有经过新基捷/矿区的支线车次才用顺/逆时针称呼
    if line["name"] == "新基捷支线" and (XINJIJIE in seq or KUANGQU in seq):
        best["dir_text"] = "顺时针" if best["kind"] == "forward" else "逆时针"

    best["train"] = t
    return best


# ================================================================ 属性判定
def get_tracks(st, trains_all):
    """由停靠站台号推测股道数量（站台号最大值 + 1，最小 2）"""
    best = 1
    for t in trains_all:
        for k, s in enumerate(t["stations"]):
            if s == st and k < len(t["platforms"]):
                best = max(best, t["platforms"][k])
    return max(2, best + 1)


# ================================================================ 标签避让
def build_id_label_levels(placed_trains):
    """按始发站给车次标签分配纵向层级，避免同一站紧密始发的车次标签重叠"""
    groups = defaultdict(list)
    for p in placed_trains:
        t = p["train"]
        tag = t["id"]
        groups[round(p["ys"][0], 3)].append((t["dep"][0], id(p), tag))

    levels = {}
    for items in groups.values():
        items.sort(key=lambda z: z[0])
        last_on_level = [None] * 3
        for x, pid, tag in items:
            width = max(15.0, len(tag) * 6.5)
            chosen = 0
            for lv in range(3):
                last = last_on_level[lv]
                if last is None or x - last >= width:
                    chosen = lv
                    break
            else:
                chosen = min(
                    range(3),
                    key=lambda lv: x - last_on_level[lv],
                )
            last_on_level[chosen] = x
            levels[pid] = chosen
    return levels


def build_time_label_levels(placed_trains):
    """按车站给到发时刻标签分配纵向层级，只让真正拥挤的标签错开"""
    groups = defaultdict(list)
    for p in placed_trains:
        t = p["train"]
        for k, yy in enumerate(p["ys"]):
            if k == 0:
                txt = min_to_hm(t["dep"][k])
                x = t["dep"][k]
            elif k == len(t["stations"]) - 1:
                txt = min_to_hm(t["arr"][k])
                x = t["arr"][k]
            else:
                txt = f"{min_to_hm(t['arr'][k])}/{min_to_hm(t['dep'][k])}"
                x = t["arr"][k]
            groups[round(yy, 3)].append((x, id(p), k, txt))

    levels = {}
    for items in groups.values():
        items.sort(key=lambda z: z[0])
        last_on_level = [None] * 4   # (x, 文本估算宽度/分钟)
        for x, pid, k, txt in items:
            width = max(10.0, len(txt) * 4.8)
            chosen = 0
            for lv in range(4):
                last = last_on_level[lv]
                if last is None or x - last[0] >= last[1]:
                    chosen = lv
                    break
            else:
                chosen = min(
                    range(4),
                    key=lambda lv: x - last_on_level[lv][0],
                )
            last_on_level[chosen] = (x, width)
            levels[(pid, k)] = chosen
    return levels


# ================================================================ 单张运行图
def draw_diagram(line, placed_trains, scope_label, trains_all, outpath,
                 annotations="auto"):
    stations = line["stations"]
    total_km = stations[-1][1]
    pad = max(10.0, total_km * 0.07)
    y_lo, y_hi = -pad, total_km + pad

    # 高密度线路默认精简标注：保留车次和到发时刻，省略区间/停站时分
    full_ann = (annotations == "full"
                or (annotations == "auto" and len(placed_trains) <= 8))
    id_levels = build_id_label_levels(placed_trains)
    time_levels = build_time_label_levels(placed_trains)

    x_max = 1440
    for p in placed_trains:
        t = p["train"]
        x_max = max(x_max, t["dep"][-1], t["arr"][-1])
    x_max = int(x_max + 20)

    fig = plt.figure(figsize=(24, 13), dpi=130)
    fig.patch.set_alpha(0.0)

    ax_l = fig.add_axes([0.017, 0.065, 0.128, 0.775])   # 左侧车站栏
    ax_p = fig.add_axes([0.152, 0.065, 0.722, 0.775])   # 主运行图
    ax_l.set_facecolor("none")
    ax_p.set_facecolor("none")

    ax_l.set_xlim(0, 1)
    ax_l.set_ylim(y_hi, y_lo)          # 里程向下增大
    ax_l.axis("off")
    ax_p.set_xlim(0, x_max)
    ax_p.set_ylim(y_hi, y_lo)
    ax_p.set_yticks([])

    # ---------------- 时间网格
    for m in range(0, x_max + 1, 30):
        if m % 120 == 0:
            ax_p.axvline(m, color=GRID_COLOR, lw=0.85, alpha=0.55, zorder=1)
        elif m % 60 == 0:
            ax_p.axvline(m, color=GRID_COLOR, lw=0.55, alpha=0.35, zorder=1)
        else:
            ax_p.axvline(m, color=GRID_COLOR, lw=0.40, alpha=0.18, zorder=1)

    # ---------------- 车站横线
    for st, yy in stations:
        ax_p.axhline(yy, color=GRID_COLOR, lw=1.15, alpha=0.90, zorder=2)
        ax_l.axhline(yy, color=GRID_COLOR, lw=1.15, alpha=0.90, zorder=2)

    # ---------------- 左侧车站资料
    for i, (st, yy) in enumerate(stations):
        n_tr = get_tracks(st, trains_all)
        is_closing = (i == len(stations) - 1 and line.get("closing_label"))
        ax_l.text(0.50, yy, st, ha="center", va="center",
                  fontsize=10.5, fontweight="bold", color=TEXT_COLOR,
                  zorder=10, bbox=label_box(pad=0.32, lw=0.8))
        sub = (f"{line['closing_label']} ｜ 股道 {n_tr}"
               if is_closing else f"股道 {n_tr} ｜ 有效长 {CFG['eff_len_default']} m")
        ax_l.annotate(sub, (0.50, yy), xytext=(0, -13),
                      textcoords="offset points", ha="center", va="center",
                      fontsize=6.8, color=TEXT_COLOR, zorder=10,
                      bbox=label_box())

    # ---------------- 站间资料
    for i in range(len(stations) - 1):
        a, ya = stations[i]
        b, yb = stations[i + 1]
        ymid = (ya + yb) / 2.0
        km = abs(yb - ya)
        km_txt = f"{km:g} km"
        if i == len(stations) - 2 and line.get("closing_label"):
            km_txt += f"（{line['closing_label']}）"
        ax_l.annotate(km_txt, (0.50, ymid), xytext=(0, 0),
                      textcoords="offset points", ha="center", va="center",
                      fontsize=7.2, color=TEXT_COLOR, zorder=10,
                      bbox=label_box())

    # ---------------- 运行线（统一红色实线）
    for idx, p in enumerate(placed_trains):
        t = p["train"]
        ys = p["ys"]

        for k in range(len(t["stations"])):
            yy = ys[k]

            # 区间运行线
            if k > 0:
                ax_p.plot([t["dep"][k - 1], t["arr"][k]], [ys[k - 1], yy],
                          color=TRAIN_COLOR, lw=2.2, ls="-", zorder=5,
                          solid_capstyle="round")

            # 停站横线
            if t["dwell"][k] > 0:
                ax_p.plot([t["arr"][k], t["dep"][k]], [yy, yy],
                          color=TRAIN_COLOR, lw=2.2, ls="-", zorder=5)

        # ---- 车次标签（放在始发点左侧，避免和始发时刻重叠）
        x0, y0 = t["dep"][0], ys[0]
        tag = t["id"]
        tag_dy = 12 + id_levels.get(id(p), 0) * 10
        if x0 < 40:
            tag_xy, tag_ha = (10, tag_dy), "left"
        else:
            tag_xy, tag_ha = (-10, tag_dy), "right"
        ax_p.annotate(tag, (x0, y0), xytext=tag_xy,
                      textcoords="offset points", ha=tag_ha, va="bottom",
                      fontsize=9.5, fontweight="bold", color=TEXT_COLOR,
                      zorder=9, bbox=label_box(pad=0.25, lw=0.9))

        # ---- 各站到 / 发时刻
        for k, st in enumerate(t["stations"]):
            yy = ys[k]
            if k == 0:
                txt = min_to_hm(t["dep"][k])
                pt = (t["dep"][k], yy)
            elif k == len(t["stations"]) - 1:
                txt = min_to_hm(t["arr"][k])
                pt = (t["arr"][k], yy)
            else:
                txt = f"{min_to_hm(t['arr'][k])}/{min_to_hm(t['dep'][k])}"
                pt = (t["arr"][k], yy)
            dy = 5 if full_ann else 5 + time_levels.get((id(p), k), 0) * 8
            ax_p.annotate(txt, pt, xytext=(6, dy),
                          textcoords="offset points",
                          fontsize=6.6, color=TEXT_COLOR, zorder=8,
                          bbox=label_box())

        if full_ann:
            # ---- 停站 / 技术作业时间
            for k, st in enumerate(t["stations"]):
                if t["dwell"][k] <= 0:
                    continue
                yy = ys[k]
                xm = (t["arr"][k] + t["dep"][k]) / 2.0
                is_tech = st in CFG["tech_stations"]
                txt = (f"技术作业 {t['dwell'][k]}分" if is_tech
                       else f"停站 {t['dwell'][k]}分")
                ax_p.annotate(txt, (xm, yy), xytext=(0, -13),
                              textcoords="offset points", ha="center",
                              fontsize=6.4, color=TEXT_COLOR, zorder=8,
                              bbox=label_box())

            # ---- 区间运行时分
            for k in range(1, len(t["stations"])):
                x1, y1 = t["dep"][k - 1], ys[k - 1]
                x2, y2 = t["arr"][k], ys[k]
                run_min = x2 - x1
                xm, ym = (x1 + x2) / 2.0, (y1 + y2) / 2.0
                dy = 12 if y2 >= y1 else -12
                ax_p.annotate(f"{int(round(run_min))}分", (xm, ym),
                              xytext=(0, dy), textcoords="offset points",
                              ha="center", va="center", fontsize=6.4,
                              color=TEXT_COLOR, zorder=8,
                              bbox=label_box())

    # ---------------- 坐标轴修饰
    for side in ("top", "right", "bottom", "left"):
        ax_p.spines[side].set_color(GRID_COLOR)
        ax_p.spines[side].set_linewidth(1.0)

    ticks = list(range(0, x_max + 1, 120))
    ax_p.set_xticks(ticks)
    ax_p.set_xticklabels([min_to_hm_axis(m) for m in ticks],
                         fontsize=8.5, color=TEXT_COLOR)
    ax_p.tick_params(axis="x", which="both", direction="out", length=3,
                     pad=3, labeltop=True, labelbottom=True, top=True,
                     bottom=True, colors=TEXT_COLOR)
    ax_p.tick_params(axis="y", left=False, right=False)

    # ---------------- 图例（不再区分方向 / 高平峰 / 车型）
    legend_label = (f"列车运行线（{len(placed_trains)} 列）"
                    if placed_trains else "本图无列车")
    handles = [Line2D([0], [0], color=TRAIN_COLOR, lw=2.4, ls="-",
                      label=legend_label)]
    leg = ax_p.legend(handles=handles, loc="upper left",
                      bbox_to_anchor=(1.01, 1.0), borderaxespad=0.0,
                      fontsize=8.8, framealpha=1.0, facecolor=LABEL_BG,
                      edgecolor=LABEL_EDGE, title="图例", title_fontsize=9.2)
    leg.get_frame().set_facecolor(LABEL_BG)
    leg.get_frame().set_edgecolor(LABEL_EDGE)
    for txt in leg.get_texts():
        txt.set_color(TEXT_COLOR)
    leg.get_title().set_color(TEXT_COLOR)

    # ---------------- 标题与线路资料
    fig.text(0.017, 0.972,
             f"列车运行图 —— {line['name']}（{scope_label}）",
             fontsize=17, fontweight="bold", va="top", color=TEXT_COLOR)

    info1 = (f"区段：{line['description']}     ｜     正线：{line['tracks']}"
             f"     ｜     到发线标准有效长：{CFG['eff_len_default']} m")

    types = sorted({t["type"] for t in (p["train"] for p in placed_trains)})
    parts = []
    for ttype in types:
        info = CFG["train_type_info"].get(ttype)
        parts.append(f"{ttype} {info['speed']} km/h" if info else ttype)
    consists = sorted({t["consist"] for t in (p["train"] for p in placed_trains)
                       if t["consist"]})
    consist_txt = ("/".join(str(c) for c in consists) + " 辆"
                   if consists else "—")
    ann_txt = "完整" if full_ann else "精简（省略区间/停站时分）"
    info2 = ("列车类型：" + ("、".join(parts) if parts else "—")
             + f"     ｜     编组：{consist_txt}"
             + f"     ｜     本图列车：{len(placed_trains)} 列"
             + f"     ｜     标注：{ann_txt}")

    fig.text(0.017, 0.941, info1, fontsize=9, va="top", color=TEXT_COLOR)
    fig.text(0.017, 0.915, info2, fontsize=8.8, va="top", color=TEXT_COLOR)

    if not placed_trains:
        ax_p.text(x_max / 2, (y_lo + y_hi) / 2,
                  f"{scope_label}：本日无列车运行",
                  ha="center", va="center", fontsize=16, color=TEXT_COLOR,
                  bbox=label_box(pad=0.4, lw=0.8))

    fig.text(0.513, 0.020, "时  刻（时:分）", ha="center", va="center",
             fontsize=10.5, color=TEXT_COLOR)

    fig.savefig(outpath, dpi=130, transparent=True)
    plt.close(fig)
    print(f"[OK] 已输出：{outpath}（{len(placed_trains)} 列）")


# ================================================================ 列表输出
def list_lines(groups):
    print(f"共解析到 {sum(len(v) for v in groups.values())} 列，分属 "
          f"{len(groups)} 条线路：")
    for i, (name, line_trains) in enumerate(ordered_group_items(groups), 1):
        line = get_line_def(name, line_trains)
        route = " → ".join(st for st, _ in line["stations"])
        day_sets = sorted({tuple(sorted(set(t["days"]))) for t in line_trains})
        day_txt = "、".join(day_set_label(d) for d in day_sets)
        print(f"\n{i}. {name}：{len(line_trains)} 列（{day_txt}）")
        print(f"   站序：{route}")
        print(f"   车次：{', '.join(t['id'] for t in line_trains)}")


# ================================================================ 主流程
def main(argv=None):
    parser = argparse.ArgumentParser(
        description="按线路绘制列车运行图（多线路版；透明背景、红色实线、"
                    "黑底白字标签）",
    )
    parser.add_argument("csv", nargs="?", default="trains.csv",
                        help="车次 CSV 文件（默认：trains.csv）")
    parser.add_argument("outdir", nargs="?", default="运行图输出",
                        help="输出目录（默认：运行图输出）")
    parser.add_argument("--per-day", action="store_true",
                        help="逐日分别出图，而不是合并相同运行日的车次")
    parser.add_argument("--line", action="append", default=[], metavar="名称",
                        help="只绘制匹配的线路，可重复使用；按名称子串匹配")
    parser.add_argument("--annotations", choices=["auto", "full", "minimal"],
                        default="auto",
                        help="标注详细程度：auto=超过 8 列自动精简（默认），"
                             "full=全部标注，minimal=只标车次和到发时刻")
    parser.add_argument("--list-lines", action="store_true",
                        help="只列出线路分组和车次，不生成图片")
    args = parser.parse_args(argv)

    trains = parse_csv(args.csv)
    if not trains:
        raise SystemExit("CSV 中未解析到有效车次")

    groups = group_trains(trains)

    if args.list_lines:
        list_lines(groups)
        return

    # 选择线路
    selected = []
    for name, line_trains in ordered_group_items(groups):
        if args.line and not any(k in name for k in args.line):
            continue
        selected.append((name, line_trains))
    if not selected:
        available = "、".join(groups)
        raise SystemExit(f"没有匹配 --line 的线路；可用线路：{available}")

    os.makedirs(args.outdir, exist_ok=True)
    total_out = 0

    for name, line_trains in selected:
        line = get_line_def(name, line_trains)

        if args.per_day:
            jobs = []
            for d in range(1, 8):
                day_trains = [t for t in line_trains if d in t["days"]]
                jobs.append((DAY_NAMES[d - 1], day_trains))
        else:
            by_days = defaultdict(list)
            for t in line_trains:
                by_days[tuple(sorted(set(t["days"])))].append(t)
            jobs = [(day_set_label(days), by_days[days])
                    for days in sorted(by_days)]

        for scope_label, job_trains in jobs:
            if not job_trains:
                continue
            placed = [place_train(t, line) for t in job_trains]
            fname = (f"运行图_{safe_filename(name)}_{scope_label}.png")
            outpath = os.path.join(args.outdir, fname)
            draw_diagram(line, placed, scope_label, trains, outpath,
                         annotations=args.annotations)
            total_out += 1

    print(f"\n完成：共输出 {total_out} 张运行图到 {args.outdir}")


if __name__ == "__main__":
    main()
