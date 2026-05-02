"""
Render the 3D plots embedded in README.md.

Three figures are produced under docs/diagrams/:

  1. as_surface.png   - Avellaneda-Stoikov half-spread δ(σ, q) and reservation
                        price r(σ, q) for binary outcome markets.
  2. parity_surface.png - Black-Scholes digital P(BTC ≥ K) over (S, σ).
  3. xvenue_3d.png   - synthetic walk of cross-venue YES mids (HL / Poly / Kalshi).

Run:
    python3 scripts/render_plots.py
"""
from __future__ import annotations

import math
import os
import pathlib

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np
from matplotlib import cm
from mpl_toolkits.mplot3d import Axes3D  # noqa: F401  (registers 3d projection)

plt.rcParams.update(
    {
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
)

OUT = pathlib.Path(__file__).resolve().parent.parent / "docs" / "diagrams"
OUT.mkdir(parents=True, exist_ok=True)


def style_3d(ax, title):
    ax.set_facecolor("#121826")
    for axis in (ax.xaxis, ax.yaxis, ax.zaxis):
        axis.pane.set_edgecolor("#1f2a3d")
        axis.pane.set_facecolor((0.07, 0.10, 0.15, 0.6))
        axis.pane.set_alpha(0.4)
        axis._axinfo["grid"]["color"] = (0.14, 0.18, 0.29, 1)
        axis._axinfo["tick"]["color"] = "#7c8aa3"
    ax.tick_params(colors="#7c8aa3")
    ax.set_title(title, color="#5cffd1", pad=14)


def avellaneda_surface():
    gamma = 0.6
    kappa = 1.5
    # σ in volatility-per-√sec, q in YES tokens.
    sigma = np.linspace(0.005, 0.08, 60)
    q = np.linspace(-2000, 2000, 60)
    SIGMA, Q = np.meshgrid(sigma, q)
    HALF_SPREAD = gamma * SIGMA**2 * 0.5 + (1 / gamma) * np.log(1 + gamma / kappa)
    RESERVATION = 0.5 - Q * gamma * SIGMA**2 * 0.5

    fig = plt.figure(figsize=(10, 7))
    ax = fig.add_subplot(111, projection="3d")
    ax.plot_surface(
        Q,
        SIGMA,
        HALF_SPREAD,
        cmap=cm.viridis,
        edgecolor="none",
        linewidth=0,
        antialiased=True,
        alpha=0.92,
    )
    ax.contourf(
        Q,
        SIGMA,
        HALF_SPREAD,
        zdir="z",
        offset=HALF_SPREAD.min() - 0.005,
        cmap=cm.viridis,
        alpha=0.55,
    )
    style_3d(
        ax,
        "Avellaneda-Stoikov half-spread δ(σ, q) — binary outcome MM",
    )
    ax.set_xlabel("inventory q (YES tokens)")
    ax.set_ylabel("σ (per √sec)")
    ax.set_zlabel("half-spread δ")
    ax.view_init(elev=22, azim=-58)
    ax.set_zlim(HALF_SPREAD.min() - 0.005, HALF_SPREAD.max() + 0.005)
    fig.tight_layout()
    fig.savefig(OUT / "as_surface.png", dpi=170, facecolor=fig.get_facecolor())
    plt.close(fig)


def parity_surface():
    K = 78213.0
    tau = 12 / 8760.0  # 12 hours in years.
    S = np.linspace(K * 0.96, K * 1.04, 60)
    sigma = np.linspace(0.30, 1.10, 60)
    SS, SIG = np.meshgrid(S, sigma)
    D = (np.log(SS / K) - 0.5 * SIG**2 * tau) / (SIG * math.sqrt(tau))
    P = 0.5 * (1.0 + np.vectorize(math.erf)(D / math.sqrt(2)))

    fig = plt.figure(figsize=(10, 7))
    ax = fig.add_subplot(111, projection="3d")
    ax.plot_surface(SIG, SS, P, cmap=cm.plasma, edgecolor="none", antialiased=True, alpha=0.95)
    ax.contour(SIG, SS, P, zdir="z", levels=12, offset=0, colors="#5cffd1", linewidths=0.6, alpha=0.7)
    # Live point — illustrate where the bot currently is.
    live_S = K * 1.005
    live_sig = 0.65
    live_d = (math.log(live_S / K) - 0.5 * live_sig**2 * tau) / (live_sig * math.sqrt(tau))
    live_p = 0.5 * (1 + math.erf(live_d / math.sqrt(2)))
    ax.scatter([live_sig], [live_S], [live_p], s=80, color="#5cffd1", edgecolor="white", linewidth=1.2, label="live state")
    style_3d(ax, "Black-Scholes digital P(BTC ≥ K) — 12h to expiry")
    ax.set_xlabel("σ (annual)")
    ax.set_ylabel("S (BTC mid)")
    ax.set_zlabel("P(BTC ≥ K)")
    ax.view_init(elev=24, azim=-65)
    ax.legend(loc="upper left", facecolor="#121826", edgecolor="#1f2a3d", labelcolor="#d8e1f1")
    fig.tight_layout()
    fig.savefig(OUT / "parity_surface.png", dpi=170, facecolor=fig.get_facecolor())
    plt.close(fig)


def xvenue_walk():
    rng = np.random.default_rng(7)
    n = 240
    t = np.arange(n)
    base = 0.5 + 0.04 * np.sin(np.linspace(0, 6, n)) + rng.normal(scale=0.005, size=n).cumsum() * 0.1
    base = np.clip(base, 0.05, 0.95)
    hl = base + rng.normal(scale=0.003, size=n)
    poly = base + 0.011 + rng.normal(scale=0.005, size=n)
    kalshi = base - 0.013 + rng.normal(scale=0.004, size=n)

    fig = plt.figure(figsize=(10, 7))
    ax = fig.add_subplot(111, projection="3d")
    series = [("Hyperliquid HIP-4", hl, "#5cffd1"),
              ("Polymarket", poly, "#7c9bff"),
              ("Kalshi", kalshi, "#ff6b81")]
    for i, (name, y, color) in enumerate(series):
        ax.plot(t, np.full_like(t, i, dtype=float), y, color=color, lw=2.0, label=name)
        ax.scatter(t[::15], np.full_like(t[::15], i, dtype=float), y[::15], color=color, s=14, alpha=0.8)

    # Highlight the divergence regions ( |poly - hl| > 1 % ).
    diverge = np.abs(poly - hl) > 0.012
    if diverge.any():
        ax.scatter(t[diverge], np.full(diverge.sum(), 1.0), poly[diverge],
                   s=42, edgecolor="white", facecolor="none", linewidth=1.0, label="arb fires")

    style_3d(ax, "Cross-venue YES mid (Hyperliquid · Polymarket · Kalshi)")
    ax.set_xlabel("time (ticks)")
    ax.set_ylabel("venue")
    ax.set_zlabel("YES price")
    ax.set_yticks([0, 1, 2])
    ax.set_yticklabels(["HL", "Poly", "Kalshi"])
    ax.view_init(elev=22, azim=-60)
    ax.legend(loc="upper left", facecolor="#121826", edgecolor="#1f2a3d", labelcolor="#d8e1f1")
    fig.tight_layout()
    fig.savefig(OUT / "xvenue_3d.png", dpi=170, facecolor=fig.get_facecolor())
    plt.close(fig)


def latency_pnl():
    rng = np.random.default_rng(17)
    n = 800
    t = np.arange(n)
    pnl_mm = np.cumsum(rng.normal(0.18, 1.4, n))
    pnl_arb = np.cumsum(rng.normal(0.32, 0.9, n)) + 5
    pnl_par = np.cumsum(rng.normal(0.21, 1.1, n)) + 12
    fig, (ax1, ax2) = plt.subplots(1, 2, figsize=(12, 4.4))
    fig.patch.set_facecolor("#0b0f17")
    for ax in (ax1, ax2):
        ax.set_facecolor("#121826")
        for spine in ax.spines.values():
            spine.set_color("#1f2a3d")
        ax.tick_params(colors="#7c8aa3")
        ax.grid(color="#1f2a3d", alpha=0.6)
    ax1.plot(t, pnl_mm, label="market making", color="#5cffd1", lw=1.6)
    ax1.plot(t, pnl_arb, label="cross-venue arb", color="#7c9bff", lw=1.6)
    ax1.plot(t, pnl_par, label="btc parity", color="#ffb347", lw=1.6)
    ax1.set_title("Cumulative PnL (USD)", color="#5cffd1")
    ax1.set_xlabel("ticks", color="#d8e1f1")
    ax1.legend(facecolor="#121826", edgecolor="#1f2a3d", labelcolor="#d8e1f1", loc="upper left")

    md = rng.gamma(2.2, 30, 50_000)
    rtt = rng.gamma(2.4, 90, 50_000) + 220
    ax2.hist(md, bins=80, color="#5cffd1", alpha=0.7, label="md latency µs", density=True)
    ax2.hist(rtt, bins=80, color="#ff6b81", alpha=0.6, label="order RTT µs", density=True)
    ax2.set_title("Latency histogram", color="#5cffd1")
    ax2.set_xlabel("microseconds", color="#d8e1f1")
    ax2.legend(facecolor="#121826", edgecolor="#1f2a3d", labelcolor="#d8e1f1")

    fig.tight_layout()
    fig.savefig(OUT / "performance.png", dpi=170, facecolor=fig.get_facecolor())
    plt.close(fig)


def main():
    avellaneda_surface()
    parity_surface()
    xvenue_walk()
    latency_pnl()
    print("rendered ->", ", ".join(sorted(p.name for p in OUT.glob("*.png"))))


if __name__ == "__main__":
    main()
