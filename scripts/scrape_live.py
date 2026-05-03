"""
Capture a time-series of public market data for the bot's animation
pipeline. **No synthetic data is ever written.** Two paths are tried in
order; if both fail the script exits non-zero so the caller can decide
what to do.

  1. **Live snapshot mode (default).** Polls the public REST endpoints we
     trade against (Hyperliquid /info, Polymarket gamma + CLOB, Kalshi v2)
     once per `--interval` seconds for `--secs` seconds. Anonymous; well
     inside every venue's free rate limit (≤ 2 RPS per host).

  2. **Historical replay mode (`--mode historical`, or auto-fallback).**
     Reconstructs a recent window from each venue's archived market-data
     APIs:
       * Polymarket  `/prices-history?market=<token_id>&interval=...`
       * Kalshi      `/markets/<ticker>/candlesticks`
       * CoinGecko   `/coins/bitcoin/market_chart` (for BTC perp mid)
     These endpoints are public and unauthenticated. The window is
     interpolated to the same `--interval` cadence as the live mode so
     the produced JSON is a drop-in replacement.

The output file (`docs/diagrams/live_snapshots.json`) is consumed by
`scripts/animate.py` to render the README GIFs.

    python3 scripts/scrape_live.py [--secs 60] [--interval 0.5] \\
        [--mode {live,historical,auto}] \\
        [--btc-coin BTC] \\
        [--poly-token TOKEN_ID] \\
        [--kalshi-ticker KXBTCD-...]
"""
from __future__ import annotations

import argparse
import json
import math
import pathlib
import sys
import time
from typing import Any, Optional

try:
    import requests
except ImportError as exc:  # pragma: no cover
    print(f"missing dep: {exc}; pip install requests", file=sys.stderr)
    sys.exit(2)

OUT = pathlib.Path(__file__).resolve().parent.parent / "docs" / "diagrams"
OUT.mkdir(parents=True, exist_ok=True)

HL_URL = "https://api.hyperliquid.xyz/info"
POLY_GAMMA = "https://gamma-api.polymarket.com/markets"
POLY_CLOB = "https://clob.polymarket.com"
KALSHI = "https://api.elections.kalshi.com/trade-api/v2"
COINGECKO = "https://api.coingecko.com/api/v3"
USER_AGENT = "hl-omm-bot/0.1 (research; live + historical scrape for animations)"


def make_session() -> requests.Session:
    s = requests.Session()
    s.headers.update({"User-Agent": USER_AGENT, "Content-Type": "application/json"})
    return s


def can_reach(session: requests.Session, url: str, post_body: Optional[dict] = None) -> bool:
    try:
        if post_body is not None:
            r = session.post(url, json=post_body, timeout=3)
        else:
            r = session.get(url, timeout=3)
        return r.ok
    except Exception:  # noqa: BLE001
        return False


# ---------------------------------------------------------------------------
# Live snapshot path.
# ---------------------------------------------------------------------------

def hl_snapshot(session: requests.Session, coin: str) -> dict[str, Any]:
    state = session.post(HL_URL, json={"type": "allMids"}, timeout=4).json()
    perp_book = session.post(
        HL_URL,
        json={"type": "l2Book", "coin": coin, "nSigFigs": 5},
        timeout=4,
    ).json()
    # HIP-4 outcome book — Hyperliquid uses the same /info l2Book request,
    # the coin string is just the OUT:... ticker.
    outcome_yes_book = session.post(
        HL_URL,
        json={"type": "l2Book", "coin": "OUT:BTC-78213-2026-05-03-YES"},
        timeout=4,
    ).json()
    return {
        "mids": state,
        "btc_book": perp_book,
        "outcome_yes_book": _normalise_hl_book(outcome_yes_book),
    }


def _normalise_hl_book(book: dict) -> dict:
    levels = book.get("levels") or [[], []]
    bids = [[lvl["px"], lvl["sz"]] for lvl in levels[0][:10]]
    asks = [[lvl["px"], lvl["sz"]] for lvl in levels[1][:10]]
    return {"bids": bids, "asks": asks}


def poly_yes_token(session: requests.Session, override: Optional[str]) -> Optional[str]:
    if override:
        return override
    r = session.get(
        POLY_GAMMA,
        params={"search": "Bitcoin price", "active": "true", "closed": "false", "limit": 5},
        timeout=4,
    )
    r.raise_for_status()
    for m in r.json():
        for t in m.get("tokens") or []:
            if t.get("outcome", "").lower() == "yes":
                return t.get("token_id")
    return None


def poly_book(session: requests.Session, token_id: str) -> dict[str, Any]:
    r = session.get(f"{POLY_CLOB}/book", params={"token_id": token_id}, timeout=4)
    r.raise_for_status()
    return r.json()


def kalshi_btc_markets(session: requests.Session, ticker: Optional[str]) -> dict[str, Any]:
    if ticker:
        r = session.get(f"{KALSHI}/markets/{ticker}/orderbook", timeout=4)
        r.raise_for_status()
        return {"orderbook": r.json(), "ticker": ticker}
    r = session.get(
        f"{KALSHI}/markets",
        params={"status": "open", "limit": 50, "event_ticker": "KXBTCD"},
        timeout=4,
    )
    r.raise_for_status()
    return {"markets": r.json().get("markets", [])}


def capture_live(args, session: requests.Session) -> list[dict[str, Any]]:
    if not can_reach(session, HL_URL, post_body={"type": "allMids"}):
        raise NetworkUnreachable("Hyperliquid /info unreachable")
    poly_token = poly_yes_token(session, args.poly_token)
    snaps: list[dict[str, Any]] = []
    t0 = time.time()
    n_ticks = max(1, int(args.secs / args.interval))
    next_tick = t0
    for i in range(n_ticks):
        snap = {
            "ts": time.time(),
            "source": "live",
            "hl": _safe(lambda: hl_snapshot(session, args.btc_coin)),
            "poly": {
                "token_id": poly_token,
                "book": _safe(lambda: poly_book(session, poly_token)) if poly_token else None,
            },
            "kalshi": _safe(lambda: kalshi_btc_markets(session, args.kalshi_ticker)),
        }
        snaps.append(snap)
        elapsed = time.time() - t0
        print(f"  tick {i+1:>3d}/{n_ticks}  t={elapsed:6.2f}s", flush=True)
        next_tick += args.interval
        time.sleep(max(0.0, next_tick - time.time()))
    return snaps


def _safe(fn):
    try:
        return fn()
    except Exception as e:  # noqa: BLE001
        return {"_error": str(e)}


# ---------------------------------------------------------------------------
# Historical replay path. Reconstructs a recent time-series from public
# archive endpoints — no synthesis, just resampling real prints to a common
# cadence.
# ---------------------------------------------------------------------------

class NetworkUnreachable(RuntimeError):
    pass


def coingecko_btc(session: requests.Session, secs: float) -> list[tuple[float, float]]:
    """Return [(unix_seconds, btc_usd), ...] over the last `secs` seconds."""
    days = max(1, int(math.ceil(secs / 86_400.0)))
    r = session.get(
        f"{COINGECKO}/coins/bitcoin/market_chart",
        params={"vs_currency": "usd", "days": days, "precision": "1"},
        timeout=8,
    )
    r.raise_for_status()
    prices = r.json().get("prices") or []
    return [(p[0] / 1000.0, p[1]) for p in prices]


def polymarket_history(
    session: requests.Session, token_id: str, secs: float
) -> list[tuple[float, float]]:
    """Polymarket CLOB price history for a YES token. Returns (ts, p_yes)."""
    end_ts = int(time.time())
    start_ts = end_ts - int(secs)
    interval = "1m"  # finest publicly exposed granularity
    r = session.get(
        f"{POLY_CLOB}/prices-history",
        params={"market": token_id, "startTs": start_ts, "endTs": end_ts, "interval": interval},
        timeout=8,
    )
    r.raise_for_status()
    pts = r.json().get("history") or []
    return [(p["t"], p["p"]) for p in pts]


def kalshi_history(
    session: requests.Session, ticker: str, secs: float
) -> list[tuple[float, float]]:
    """Kalshi candlesticks → (ts, yes_mid_in_[0,1])."""
    end_ts = int(time.time())
    start_ts = end_ts - int(secs)
    r = session.get(
        f"{KALSHI}/markets/{ticker}/candlesticks",
        params={"start_ts": start_ts, "end_ts": end_ts, "period_interval": 1},
        timeout=8,
    )
    r.raise_for_status()
    candles = r.json().get("candlesticks") or []
    return [(c["end_period_ts"], (c.get("yes_bid", {}).get("close", 0) +
                                   c.get("yes_ask", {}).get("close", 0)) / 200.0)
            for c in candles]


def hl_history(session: requests.Session, secs: float) -> list[tuple[float, float]]:
    """Hyperliquid /info candleSnapshot for BTC perp."""
    end_ms = int(time.time() * 1000)
    start_ms = end_ms - int(secs * 1000)
    body = {
        "type": "candleSnapshot",
        "req": {"coin": "BTC", "interval": "1m", "startTime": start_ms, "endTime": end_ms},
    }
    r = session.post(HL_URL, json=body, timeout=8)
    r.raise_for_status()
    candles = r.json() or []
    return [(c["t"] / 1000.0, float(c["c"])) for c in candles]


def resample(series: list[tuple[float, float]], t0: float, n: int, interval: float) -> list[float]:
    """Linear-interpolate `series` (ts, val) onto evenly-spaced ticks."""
    if not series:
        return [float("nan")] * n
    series = sorted(series)
    ts = [s[0] for s in series]
    vs = [s[1] for s in series]
    out = []
    for i in range(n):
        target = t0 + i * interval
        # Edge cases.
        if target <= ts[0]:
            out.append(vs[0]); continue
        if target >= ts[-1]:
            out.append(vs[-1]); continue
        # Binary search.
        lo, hi = 0, len(ts) - 1
        while hi - lo > 1:
            mid = (lo + hi) // 2
            if ts[mid] <= target:
                lo = mid
            else:
                hi = mid
        alpha = (target - ts[lo]) / max(1e-9, ts[hi] - ts[lo])
        out.append(vs[lo] * (1 - alpha) + vs[hi] * alpha)
    return out


def capture_historical(args, session: requests.Session) -> list[dict[str, Any]]:
    poly_token = poly_yes_token(session, args.poly_token)
    if not poly_token:
        raise NetworkUnreachable("could not resolve a Polymarket YES token")
    if not args.kalshi_ticker:
        raise NetworkUnreachable("--kalshi-ticker is required for historical mode")

    btc_series = hl_history(session, args.secs) or coingecko_btc(session, args.secs)
    poly_series = polymarket_history(session, poly_token, args.secs)
    kalshi_series = kalshi_history(session, args.kalshi_ticker, args.secs)

    n = max(1, int(args.secs / args.interval))
    t0 = time.time() - args.secs
    btc = resample(btc_series, t0, n, args.interval)
    poly_yes = resample(poly_series, t0, n, args.interval)
    kalshi_yes = resample(kalshi_series, t0, n, args.interval)

    snaps: list[dict[str, Any]] = []
    for i in range(n):
        snaps.append({
            "ts": t0 + i * args.interval,
            "source": "historical",
            "hl": {"mids": {"BTC": f"{btc[i]:.1f}"}},
            "poly": {"token_id": poly_token, "yes": poly_yes[i]},
            "kalshi": {"ticker": args.kalshi_ticker, "yes": kalshi_yes[i]},
        })
    return snaps


# ---------------------------------------------------------------------------
# Driver.
# ---------------------------------------------------------------------------

def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--secs", type=float, default=60.0)
    ap.add_argument("--interval", type=float, default=0.5)
    ap.add_argument("--btc-coin", default="BTC")
    ap.add_argument("--poly-token", default=None,
                    help="explicit Polymarket YES token id (skips auto-discovery)")
    ap.add_argument("--kalshi-ticker", default=None,
                    help="explicit Kalshi market ticker (e.g. KXBTCD-26MAY03-78213-Y)")
    ap.add_argument("--mode", choices=("live", "historical", "auto"), default="auto",
                    help="auto = live first, fall back to historical on network failure")
    args = ap.parse_args()

    session = make_session()
    snaps: list[dict[str, Any]] = []
    if args.mode in ("live", "auto"):
        try:
            snaps = capture_live(args, session)
            print("captured live snapshots ✓")
        except NetworkUnreachable as e:
            if args.mode == "live":
                print(f"live capture failed: {e}", file=sys.stderr)
                return 1
            print(f"live capture failed ({e}); falling back to historical replay")
        except Exception as e:  # noqa: BLE001
            if args.mode == "live":
                print(f"live capture failed: {e}", file=sys.stderr)
                return 1
            print(f"live capture failed ({e}); falling back to historical replay")

    if not snaps and args.mode in ("historical", "auto"):
        try:
            snaps = capture_historical(args, session)
            print("captured historical snapshots ✓")
        except Exception as e:  # noqa: BLE001
            print(
                "historical replay failed:\n"
                f"  {e}\n\n"
                "no synthetic fallback is available — re-run on a host with\n"
                "outbound HTTPS to api.hyperliquid.xyz / clob.polymarket.com /\n"
                "api.elections.kalshi.com (or pass --kalshi-ticker / --poly-token).",
                file=sys.stderr,
            )
            return 1

    if not snaps:
        print("no snapshots produced", file=sys.stderr)
        return 1

    out = OUT / "live_snapshots.json"
    out.write_text(json.dumps({"snapshots": snaps}, indent=1))
    print(f"wrote {out}  ({len(snaps)} snapshots, source={snaps[0].get('source')})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
