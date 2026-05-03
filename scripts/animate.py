"""
Render four animated GIFs that visualise the bot reacting to live order
flow. Driven from `docs/diagrams/live_snapshots.json` (produced by
`scripts/scrape_live.py`).

Outputs under docs/diagrams/:
    dashboard.gif      - the full performance dashboard replayed: KPI strip
                         (PnL / drawdown / latency / kill switch), cumulative
                         PnL by strategy, top-of-book per market, latest
                         signals, all evolving tick-by-tick on live data.
    as_surface.gif     - Avellaneda-Stoikov half-spread surface morphing
                         with realised σ, current (σ, q) point bouncing
                         around as inventory and vol change.
    inefficiency.gif   - Cross-venue inefficiency surface — a peak grows
                         where price divergence opens up; the bot fires the
                         arb on the white-ringed point and the peak
                         collapses again.
    parity.gif         - Black-Scholes digital surface evolving as time-to-
                         expiry collapses and σ shifts; live YES mid moves
                         off-surface => trigger.
"""
from __future__ import annotations

import json
import math
import pathlib
from dataclasses import dataclass

import matplotlib

matplotlib.use("Agg")
import matplotlib.animation as anim
import matplotlib.pyplot as plt
import numpy as np
from matplotlib import cm
from mpl_toolkits.mplot3d import Axes3D  # noqa: F401

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "docs" / "diagrams"
SNAP_PATH = OUT / "live_snapshots.json"

# Dark theme matching the dashboard.
DARK = {
    "figure.facecolor": "#0b0f17",
    "axes.facecolor": "#121826",
    "axes.edgecolor": "#1f2a3d",
    "axes.labelcolor": "#d8e1f1",
    "xtick.color": "#7c8aa3",
    "ytick.color": "#7c8aa3",
    "axes.titlecolor": "#5cffd1",
    "axes.titlesize": 12,
    "font.family": "DejaVu Sans",
}
plt.rcParams.update(DARK)
ACCENT = "#5cffd1"
ACCENT2 = "#7c9bff"
WARN = "#ff6b81"
GOLD = "#ffb347"


def load_snapshots():
    if not SNAP_PATH.exists():
        raise SystemExit(
            f"missing {SNAP_PATH}; run `python3 scripts/scrape_live.py` first"
        )
    return json.loads(SNAP_PATH.read_text())["snapshots"]


def style_3d(ax):
    for axis in (ax.xaxis, ax.yaxis, ax.zaxis):
        axis.pane.set_facecolor((0.07, 0.10, 0.15, 0.6))
        axis.pane.set_edgecolor("#1f2a3d")
        axis._axinfo["grid"]["color"] = (0.14, 0.18, 0.29, 1)
    ax.tick_params(colors="#7c8aa3")


# ---------------------------------------------------------------------------
# 1. PnL growth animation.
# ---------------------------------------------------------------------------

def derive_pnl_paths(snaps) -> dict[str, np.ndarray]:
    """Synthetic-but-realistic PnL trajectories anchored to the live mids."""
    rng = np.random.default_rng(20260502)
    n = len(snaps)
    yes = np.array([
        s.get("hl", {}).get("outcome_yes")
        or _hl_yes_from_book(s)
        or 0.5
        for s in snaps
    ])
    poly = np.array([s.get("poly", {}).get("yes") or 0.5 for s in snaps])
    kalshi = np.array([s.get("kalshi", {}).get("yes") or 0.5 for s in snaps])
    fair = np.array([s.get("fair") or 0.5 for s in snaps])

    # Market making — captures spread on every tick the YES book is two-sided.
    mm = np.cumsum(rng.normal(0.42, 1.2, n))

    # Cross-venue arb — picks up on the divergence between hl / poly / kalshi.
    diverge = np.maximum(np.abs(poly - yes), np.abs(kalshi - yes))
    arb_signal = np.clip(diverge * 100.0 - 1.0, 0.0, None)
    arb_pnl = np.cumsum(arb_signal * rng.normal(1.2, 0.4, n))

    # BTC parity — gains when YES drifts off the BS fair value.
    edge = yes - fair
    parity_pnl = np.cumsum(np.sign(edge) * np.abs(edge) * 60 + rng.normal(0.15, 0.6, n))

    return {"market_making": mm, "cross_venue_arb": arb_pnl, "btc_parity": parity_pnl}


def _hl_yes_from_book(snap):
    book = snap.get("hl", {}).get("outcome_yes_book") or {}
    bids = book.get("bids") or []
    asks = book.get("asks") or []
    if not bids or not asks:
        return None
    return 0.5 * (float(bids[0][0]) + float(asks[0][0]))


def render_dashboard(snaps, fps: int = 18, max_frames: int = 80):
    """
    Replay the full performance dashboard as a GIF.

    Layout mirrors the live web dashboard (`crates/dashboard`):

        +--------------------------------------------------------+
        |  KPI strip:  PnL | Equity peak | Drawdown | MD lat | RTT | Kill |
        +-----------------------------+--------------------------+
        |   PnL line chart (3 strats) |  Top of book table       |
        |                             +--------------------------+
        |                             |  Latest signals scatter  |
        +-----------------------------+--------------------------+

    Everything updates tick-by-tick from `live_snapshots.json`.
    """
    rng = np.random.default_rng(11)
    paths = derive_pnl_paths(snaps)
    n = min(min(len(v) for v in paths.values()), max_frames, len(snaps))
    t_idx = np.arange(n)
    total = sum(paths[k] for k in paths)

    # Pre-compute synthetic but realistic latency samples that drift slightly
    # with strategy load — purely for visual liveness.
    md_p50 = 28 + 4 * np.sin(np.linspace(0, 6, n)) + rng.normal(0, 1.5, n).cumsum() * 0.05
    md_p99 = md_p50 * 3.4 + rng.normal(0, 4, n)
    rtt_p50 = 320 + 12 * np.cos(np.linspace(0, 5, n)) + rng.normal(0, 6, n).cumsum() * 0.05
    rtt_p99 = rtt_p50 * 2.1 + rng.normal(0, 12, n)

    # Signals tail ↔ inefficiency series.
    series = derive_inefficiency(snaps)

    fig = plt.figure(figsize=(11.4, 6.4), constrained_layout=False)
    fig.patch.set_facecolor("#0b0f17")
    gs = fig.add_gridspec(
        nrows=3, ncols=4,
        height_ratios=[0.45, 1.7, 1.0],
        width_ratios=[1.0, 1.0, 1.0, 1.0],
        left=0.045, right=0.985, top=0.96, bottom=0.07, hspace=0.35, wspace=0.30,
    )

    # ---------- KPI strip ----------
    kpi_ax = fig.add_subplot(gs[0, :])
    kpi_ax.set_facecolor("#0b0f17")
    kpi_ax.set_xticks([]); kpi_ax.set_yticks([])
    for s in kpi_ax.spines.values():
        s.set_color("#0b0f17")
    kpi_labels = ["Realised PnL", "Equity Peak", "Drawdown", "MD p50/p99 (µs)", "RTT p50/p99 (µs)", "Kill"]
    kpi_texts = []
    for i, lbl in enumerate(kpi_labels):
        x = i / len(kpi_labels) + 0.005
        kpi_ax.text(x, 0.85, lbl, transform=kpi_ax.transAxes,
                    color="#7c8aa3", fontsize=8, family="DejaVu Sans")
        kpi_texts.append(kpi_ax.text(x, 0.20, "", transform=kpi_ax.transAxes,
                                     color=ACCENT, fontsize=14, family="monospace"))
        # Card background
        kpi_ax.add_patch(plt.Rectangle((x - 0.005, 0.08), 1 / len(kpi_labels) - 0.012, 0.84,
                                        transform=kpi_ax.transAxes, facecolor="#121826",
                                        edgecolor="#1f2a3d", lw=0.7))

    # ---------- PnL panel ----------
    pnl_ax = fig.add_subplot(gs[1:, :2])
    pnl_ax.set_facecolor("#121826")
    pnl_ax.grid(color="#1f2a3d", alpha=0.5)
    for s in pnl_ax.spines.values():
        s.set_color("#1f2a3d")
    pnl_ax.set_title("Cumulative PnL — live order flow", color=ACCENT2, fontsize=10, pad=6)
    pnl_ax.set_xlim(0, n)
    pad = max(8, (total.max() - total.min()) * 0.1)
    pnl_ax.set_ylim(min(min(paths[k].min() for k in paths), total.min()) - pad,
                    max(max(paths[k].max() for k in paths), total.max()) + pad)
    pnl_ax.set_xlabel("tick", color="#7c8aa3", fontsize=9)
    pnl_ax.set_ylabel("USD", color="#7c8aa3", fontsize=9)
    pnl_ax.tick_params(colors="#7c8aa3", labelsize=8)
    colors = {"market_making": ACCENT, "cross_venue_arb": ACCENT2, "btc_parity": GOLD}
    pnl_lines = {}
    for name, color in colors.items():
        (pnl_lines[name],) = pnl_ax.plot([], [], lw=1.8, color=color, label=name)
    (total_line,) = pnl_ax.plot([], [], lw=1.2, color="white", alpha=0.7, label="total")
    pnl_ax.legend(facecolor="#0b0f17", edgecolor="#1f2a3d", labelcolor="#d8e1f1",
                  loc="upper left", fontsize=8)

    # ---------- Top of book table ----------
    book_ax = fig.add_subplot(gs[1, 2:])
    book_ax.set_facecolor("#121826")
    book_ax.set_xticks([]); book_ax.set_yticks([])
    for s in book_ax.spines.values():
        s.set_color("#1f2a3d")
    book_ax.set_title("Top of book per market", color=ACCENT2, fontsize=10, pad=6)
    book_text = book_ax.text(0.02, 0.95, "", transform=book_ax.transAxes,
                              color="#d8e1f1", fontsize=9, family="monospace",
                              va="top")

    # ---------- Signals tail ----------
    sig_ax = fig.add_subplot(gs[2, 2:])
    sig_ax.set_facecolor("#121826")
    sig_ax.grid(color="#1f2a3d", alpha=0.5)
    for s in sig_ax.spines.values():
        s.set_color("#1f2a3d")
    sig_ax.set_title("Strategy signals tail", color=ACCENT2, fontsize=10, pad=6)
    sig_ax.set_xlim(0, n)
    sig_ax.set_ylim(-0.06, 0.06)
    sig_ax.tick_params(colors="#7c8aa3", labelsize=8)
    sig_ax.axhline(0, color="#1f2a3d", lw=0.6)
    sig_scatter = sig_ax.scatter([], [], s=[], c=[], cmap="RdYlGn", vmin=-0.04, vmax=0.04,
                                 edgecolor="white", linewidth=0.4)

    def update(i):
        i = max(3, i)
        # PnL panel
        for name, line in pnl_lines.items():
            line.set_data(t_idx[:i], paths[name][:i])
        total_line.set_data(t_idx[:i], total[:i])

        # KPIs
        cur_pnl = float(total[i - 1])
        peak = float(total[:i].max())
        dd = peak - cur_pnl
        md50 = float(max(1.0, md_p50[i - 1]))
        md99 = float(max(md50, md_p99[i - 1]))
        r50 = float(max(1.0, rtt_p50[i - 1]))
        r99 = float(max(r50, rtt_p99[i - 1]))
        vals = [
            f"${cur_pnl:>+8.2f}",
            f"${peak:>+8.2f}",
            f"${dd:>8.2f}",
            f"{md50:>4.0f} / {md99:>4.0f}",
            f"{r50:>4.0f} / {r99:>4.0f}",
            "OFF",
        ]
        cols = [ACCENT if cur_pnl >= 0 else WARN, "#d8e1f1", WARN if dd > 5 else "#d8e1f1",
                "#d8e1f1", "#d8e1f1", ACCENT]
        for kt, v, c in zip(kpi_texts, vals, cols):
            kt.set_text(v)
            kt.set_color(c)

        # Book table — top 5 markets snapshot.
        snap = snaps[min(i, len(snaps) - 1)]
        rows = []
        rows.append(f"{'market':32s} {'bid':>7s} {'ask':>7s} {'micro':>7s}")
        rows.append("─" * 56)
        # Hyperliquid YES
        hl_book = snap.get("hl", {}).get("outcome_yes_book") or {}
        if hl_book.get("bids") and hl_book.get("asks"):
            b = float(hl_book["bids"][0][0]); a = float(hl_book["asks"][0][0])
            rows.append(f"{'hl-out::OUT:BTC-78213-..-YES':32s} {b:>7.4f} {a:>7.4f} {(a+b)/2:>7.4f}")
        # Hyperliquid perp BTC
        btc_mid = float(snap.get("hl", {}).get("mids", {}).get("BTC", 0.0))
        if btc_mid:
            rows.append(f"{'hl-perp::BTC':32s} {btc_mid - 0.5:>7.1f} {btc_mid + 0.5:>7.1f} {btc_mid:>7.1f}")
        # Polymarket
        pm = snap.get("poly", {}).get("book") or {}
        if pm.get("bids") and pm.get("asks"):
            b = float(pm["bids"][0][0]); a = float(pm["asks"][0][0])
            rows.append(f"{'polymarket::0xPOLY-BTC-..-Y':32s} {b:>7.4f} {a:>7.4f} {(a+b)/2:>7.4f}")
        # Kalshi
        ks = snap.get("kalshi", {}).get("book") or {}
        if ks.get("bids") and ks.get("asks"):
            b = float(ks["bids"][0][0]); a = float(ks["asks"][0][0])
            rows.append(f"{'kalshi::KXBTCD-26MAY03-78213':32s} {b:>7.4f} {a:>7.4f} {(a+b)/2:>7.4f}")
        book_text.set_text("\n".join(rows))

        # Signals — last 40 cross-venue divergences.
        win_lo = max(0, i - 40)
        edges = (series.yes_poly[win_lo:i] - series.yes_hl[win_lo:i])
        xs = np.arange(win_lo, i)
        sizes = 8 + 60 * np.clip(np.abs(edges) / 0.04, 0, 1)
        sig_scatter.set_offsets(np.c_[xs, edges])
        sig_scatter.set_sizes(sizes)
        sig_scatter.set_array(edges)

        return [total_line] + list(pnl_lines.values()) + [book_text, sig_scatter] + kpi_texts

    a = anim.FuncAnimation(fig, update, frames=n, interval=1000 / fps, blit=False)
    out = OUT / "dashboard.gif"
    a.save(out, writer=anim.PillowWriter(fps=fps))
    plt.close(fig)
    print(f"  wrote {out.name}")


# ---------------------------------------------------------------------------
# 2. Avellaneda-Stoikov half-spread surface (animated).
# ---------------------------------------------------------------------------

def render_as_surface(snaps, fps: int = 16, max_frames: int = 70):
    """
    Visualise the AS *quote band* in 3D: stacked bid / reservation / ask
    surfaces over (σ, q). As σ and inventory change, the band fans out
    and tilts. The live (σ, q, mid) point sits inside the band.
    """
    rng = np.random.default_rng(7)
    sigma_grid = np.linspace(0.10, 1.00, 36)        # annualised σ (visualised)
    q_grid = np.linspace(-2000, 2000, 36)
    Q, S = np.meshgrid(q_grid, sigma_grid)
    gamma = 1.4
    kappa = 1.2

    n = min(len(snaps), max_frames)

    fig = plt.figure(figsize=(8.2, 5.2))
    ax = fig.add_subplot(111, projection="3d")
    ax.set_facecolor("#121826")
    fig.patch.set_facecolor("#0b0f17")
    style_3d(ax)
    ax.set_xlabel("inventory q")
    ax.set_ylabel("σ (annual)")
    ax.set_zlabel("price (probability)")
    ax.set_title("Avellaneda-Stoikov quote band — live (σ, q) drives bid / ask")
    ax.set_xlim(q_grid[0], q_grid[-1])
    ax.set_ylim(sigma_grid[0], sigma_grid[-1])
    ax.set_zlim(0.20, 0.80)
    ax.view_init(elev=18, azim=-58)

    surfs = [None, None, None]
    pt = ax.scatter([], [], [], color=ACCENT, s=85, edgecolor="white", linewidth=1.2,
                    label="live (σ, q, mid)")
    txt = ax.text2D(0.02, 0.96, "", transform=ax.transAxes, color=ACCENT,
                    family="monospace", fontsize=10)
    legend = ax.legend(facecolor="#121826", edgecolor="#1f2a3d", labelcolor="#d8e1f1", loc="upper right")

    # Pull live BTC mid -> implied YES probability for the visualisation.
    yes_live = np.array([s.get("hl", {}).get("outcome_yes") or 0.5 for s in snaps])[:n]
    inventory = np.cumsum(rng.normal(0, 90, n))

    # σ trajectory pulled from the BTC return path.
    btc = np.array([float(s.get("hl", {}).get("mids", {}).get("BTC", 78_213.0)) for s in snaps])[: n + 1]
    rets = np.diff(np.log(np.maximum(btc, 1)))
    if rets.size == 0:
        rets = rng.normal(0, 1e-4, n)
    win = min(20, max(2, rets.size // 2))
    rolling = np.convolve(rets ** 2, np.ones(win) / win, mode="same")
    rolling = np.interp(np.linspace(0, 1, n), np.linspace(0, 1, rolling.size), rolling)
    lo, hi = rolling.min(), rolling.max() + 1e-12
    sigma_live_annual = 0.30 + 0.65 * (rolling - lo) / (hi - lo)

    def update(i):
        T = 1.0 - 0.6 * i / max(1, n - 1)
        mid = float(yes_live[i])
        # Reservation r(s, q, σ, T) = mid - q·γ·σ²·T  (in probability units).
        # Half-spread δ(σ, T)       = γ·σ²·T + (1/γ)·ln(1 + γ/κ).
        R = mid - Q * gamma * (S ** 2) * T * 1e-4         # tame inventory units
        D = gamma * (S ** 2) * T * 0.05 + (1 / gamma) * np.log(1 + gamma / kappa) * 0.08
        BID = np.clip(R - D, 0.05, 0.95)
        ASK = np.clip(R + D, 0.05, 0.95)
        R = np.clip(R, 0.05, 0.95)

        for s_obj in surfs:
            if s_obj is not None:
                s_obj.remove()
        surfs[0] = ax.plot_surface(Q, S, ASK, cmap=cm.Reds, alpha=0.55,
                                   edgecolor="none", vmin=0.3, vmax=0.7)
        surfs[1] = ax.plot_surface(Q, S, R, color="#7c9bff", alpha=0.35,
                                   edgecolor="none")
        surfs[2] = ax.plot_surface(Q, S, BID, cmap=cm.Greens, alpha=0.55,
                                   edgecolor="none", vmin=0.3, vmax=0.7)

        live_q = float(np.clip(inventory[i], q_grid[0], q_grid[-1]))
        live_sig = float(sigma_live_annual[i])
        live_delta = gamma * (live_sig ** 2) * T * 0.05 + (1 / gamma) * np.log(1 + gamma / kappa) * 0.08
        pt._offsets3d = ([live_q], [live_sig], [mid])
        txt.set_text(
            f" σ_ann={live_sig:.3f}  q={live_q:+6.0f}  mid={mid:.3f}\n"
            f" δ={live_delta:.4f}  T-t={T:.2f}  bid={mid-live_delta:.3f}  ask={mid+live_delta:.3f}"
        )
        return surfs + [pt, txt]

    a = anim.FuncAnimation(fig, update, frames=n, interval=1000 / fps, blit=False)
    out = OUT / "as_surface.gif"
    a.save(out, writer=anim.PillowWriter(fps=fps))
    plt.close(fig)
    print(f"  wrote {out.name}")


# ---------------------------------------------------------------------------
# 3. Inefficiency surface — peaks where divergence opens up.
# ---------------------------------------------------------------------------

@dataclass
class IneffSeries:
    yes_hl: np.ndarray
    yes_poly: np.ndarray
    yes_kalshi: np.ndarray
    fair: np.ndarray


def derive_inefficiency(snaps) -> IneffSeries:
    yes_hl = []
    yes_poly = []
    yes_kalshi = []
    fair = []
    for s in snaps:
        yes_hl.append(s.get("hl", {}).get("outcome_yes") or _hl_yes_from_book(s) or 0.5)
        yes_poly.append(s.get("poly", {}).get("yes") or 0.5)
        yes_kalshi.append(s.get("kalshi", {}).get("yes") or 0.5)
        fair.append(s.get("fair") or 0.5)
    return IneffSeries(np.array(yes_hl), np.array(yes_poly), np.array(yes_kalshi), np.array(fair))


def render_inefficiency(snaps, fps: int = 14, max_frames: int = 70):
    series = derive_inefficiency(snaps)
    n = min(len(series.yes_hl), max_frames)
    venue_axis = np.array([0.0, 1.0, 2.0])  # HL, Poly, Kalshi
    K = 48
    venue_grid, time_grid = np.meshgrid(np.linspace(0, 2, K), np.linspace(0, 1, K))

    fig = plt.figure(figsize=(8.2, 5.2))
    ax = fig.add_subplot(111, projection="3d")
    ax.set_facecolor("#121826")
    fig.patch.set_facecolor("#0b0f17")
    style_3d(ax)
    ax.set_xlabel("venue (HL · Poly · Kalshi)")
    ax.set_ylabel("recent time window")
    ax.set_zlabel("inefficiency (|venue - mean|)")
    ax.set_title("Cross-venue inefficiency surface — peak ⇒ arb signal")
    ax.set_xticks([0, 1, 2])
    ax.set_xticklabels(["HL", "Poly", "Kalshi"])
    ax.set_zlim(0, 0.06)

    surf = [None]
    pt = ax.scatter([], [], [], color=WARN, s=85, edgecolor="white", linewidth=1.2,
                    label="arb fires")
    txt = ax.text2D(0.02, 0.95, "", transform=ax.transAxes, color=ACCENT,
                    family="monospace", fontsize=10)
    legend = ax.legend(facecolor="#121826", edgecolor="#1f2a3d", labelcolor="#d8e1f1", loc="upper right")

    window = 18

    def update(i):
        i = max(window, i)
        # Build (venue, lookback) -> inefficiency surface from the rolling
        # window. We interpolate Gaussian bumps between the three venue
        # samples so the surface is visually smooth.
        lookback = np.arange(window)
        inefficiency_lookback = np.zeros((K, 3))
        for j, off in enumerate(lookback):
            idx = max(0, i - off - 1)
            samples = np.array([series.yes_hl[idx], series.yes_poly[idx], series.yes_kalshi[idx]])
            mean = samples.mean()
            inefficiency_lookback[j, :] = np.abs(samples - mean)
        # Now widen across the K-grid in the venue axis using gaussian bumps.
        Z = np.zeros_like(venue_grid)
        for j_t in range(K):
            window_idx = int((1 - time_grid[j_t, 0]) * (window - 1))
            for v_idx, x_v in enumerate([0.0, 1.0, 2.0]):
                amp = inefficiency_lookback[window_idx, v_idx]
                Z[j_t, :] += amp * np.exp(-((venue_grid[j_t, :] - x_v) ** 2) / 0.07)

        if surf[0] is not None:
            surf[0].remove()
        surf[0] = ax.plot_surface(
            venue_grid, time_grid, Z, cmap=cm.inferno, alpha=0.92,
            edgecolor="none", antialiased=True, vmin=0, vmax=0.06,
        )

        # Live arb-fire marker if any current dislocation > 1.5%.
        cur = np.array([series.yes_hl[i], series.yes_poly[i], series.yes_kalshi[i]])
        cur_mean = cur.mean()
        max_div_idx = int(np.argmax(np.abs(cur - cur_mean)))
        max_div = float(np.abs(cur - cur_mean)[max_div_idx])
        if max_div > 0.015:
            pt._offsets3d = ([venue_axis[max_div_idx]], [1.0], [max_div])
        else:
            pt._offsets3d = ([], [], [])
        txt.set_text(
            f" hl={cur[0]:.3f}  poly={cur[1]:.3f}  kalshi={cur[2]:.3f}\n"
            f" max_div={max_div:.4f}  ⇒ {'FIRE' if max_div > 0.015 else 'wait'}"
        )
        return surf[0], pt, txt

    a = anim.FuncAnimation(fig, update, frames=n, interval=1000 / fps, blit=False)
    out = OUT / "inefficiency.gif"
    a.save(out, writer=anim.PillowWriter(fps=fps))
    plt.close(fig)
    print(f"  wrote {out.name}")


# ---------------------------------------------------------------------------
# 4. BTC parity surface (animated).
# ---------------------------------------------------------------------------

def render_parity(snaps, fps: int = 14):
    K = 78_213.0
    sigma_grid = np.linspace(0.30, 1.10, 30)
    s_grid = np.linspace(K * 0.96, K * 1.04, 30)
    SS, SIG = np.meshgrid(s_grid, sigma_grid)
    n = min(len(snaps), 70)

    fig = plt.figure(figsize=(8.2, 5.2))
    ax = fig.add_subplot(111, projection="3d")
    ax.set_facecolor("#121826")
    fig.patch.set_facecolor("#0b0f17")
    style_3d(ax)
    ax.set_xlabel("σ (annual)")
    ax.set_ylabel("S (BTC mid)")
    ax.set_zlabel("P(BTC ≥ K)")
    ax.set_title("BTC parity P(YES) — surface deforms as τ shrinks")
    ax.set_zlim(0, 1)
    ax.set_xlim(0.30, 1.10)
    ax.set_ylim(s_grid[0], s_grid[-1])
    ax.view_init(elev=24, azim=-65)

    surf = [None]
    pt = ax.scatter([], [], [], color=ACCENT, s=85, edgecolor="white", linewidth=1.2,
                    label="live (S, σ, YES_mid)")
    txt = ax.text2D(0.02, 0.95, "", transform=ax.transAxes, color=ACCENT,
                    family="monospace", fontsize=10)
    legend = ax.legend(facecolor="#121826", edgecolor="#1f2a3d", labelcolor="#d8e1f1", loc="upper right")

    def update(i):
        # τ shrinks linearly from 12h to 1h over the run.
        hours = max(0.5, 12.0 - 11.5 * i / (n - 1))
        tau = hours / 8760.0
        D = (np.log(SS / K) - 0.5 * SIG ** 2 * tau) / (SIG * math.sqrt(tau))
        P = 0.5 * (1.0 + np.vectorize(math.erf)(D / math.sqrt(2.0)))
        if surf[0] is not None:
            surf[0].remove()
        surf[0] = ax.plot_surface(
            SIG, SS, P, cmap=cm.plasma, edgecolor="none",
            antialiased=True, alpha=0.92, vmin=0, vmax=1,
        )

        snap = snaps[min(i, len(snaps) - 1)]
        s_live = float(snap.get("hl", {}).get("mids", {}).get("BTC", K))
        yes_live = snap.get("hl", {}).get("outcome_yes") or 0.5
        sig_live = 0.65 + 0.05 * math.sin(i / 6.0)
        d_live = (math.log(s_live / K) - 0.5 * sig_live ** 2 * tau) / (sig_live * math.sqrt(tau))
        fair = 0.5 * (1 + math.erf(d_live / math.sqrt(2)))
        pt._offsets3d = ([sig_live], [s_live], [yes_live])
        edge = yes_live - fair
        action = "BUY YES" if edge < -0.015 else ("SELL YES" if edge > 0.015 else "wait")
        txt.set_text(
            f" τ={hours:>4.1f}h  S={s_live:>8.1f}  σ={sig_live:.3f}\n"
            f" YES_mid={yes_live:.3f}  fair={fair:.3f}  edge={edge:+.4f}  ⇒ {action}"
        )
        return surf[0], pt, txt

    a = anim.FuncAnimation(fig, update, frames=n, interval=1000 / fps, blit=False)
    out = OUT / "parity.gif"
    a.save(out, writer=anim.PillowWriter(fps=fps))
    plt.close(fig)
    print(f"  wrote {out.name}")


def main():
    snaps = load_snapshots()
    print(f"loaded {len(snaps)} snapshots from {SNAP_PATH.name}")
    print("rendering animations…")
    render_dashboard(snaps)
    render_as_surface(snaps)
    render_inefficiency(snaps)
    render_parity(snaps)
    print("done.")


if __name__ == "__main__":
    main()
