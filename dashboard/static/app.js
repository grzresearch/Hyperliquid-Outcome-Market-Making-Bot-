// Live dashboard for the Hyperliquid Outcome MM bot.
// Connects to /api/stream over WebSocket, renders 2D + 3D Plotly charts.

const fmtUsd = v =>
  (Math.abs(v) >= 1000 ? v.toLocaleString(undefined, { maximumFractionDigits: 0 }) : v.toFixed(2));

const dark = {
  template: 'plotly_dark',
  paper_bgcolor: '#121826',
  plot_bgcolor: '#121826',
  font: { color: '#d8e1f1', family: 'ui-sans-serif, system-ui, sans-serif' },
  margin: { l: 48, r: 16, t: 24, b: 36 },
  legend: { bgcolor: 'rgba(0,0,0,0)' },
  scene: {
    xaxis: { gridcolor: '#23304a' },
    yaxis: { gridcolor: '#23304a' },
    zaxis: { gridcolor: '#23304a' },
    bgcolor: '#121826',
  },
};

const $ = q => document.querySelector(q);

const PLOTS = {
  pnl: 'plot-pnl',
  asSurface: 'plot-as-surface',
  xvenue: 'plot-xvenue',
  parity: 'plot-parity',
  signals: 'plot-signals',
};

let socketReady = false;
function connect() {
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  const ws = new WebSocket(`${proto}//${location.host}/api/stream`);
  ws.onopen = () => { socketReady = true; };
  ws.onclose = () => { socketReady = false; setTimeout(connect, 1000); };
  ws.onmessage = ev => {
    try { render(JSON.parse(ev.data)); } catch (e) { console.error(e); }
  };
}

function render(snap) {
  // KPIs.
  $('#kpi-pnl').textContent = '$' + fmtUsd(parseFloat(snap.risk.realised_pnl));
  $('#kpi-peak').textContent = '$' + fmtUsd(parseFloat(snap.risk.peak_equity));
  const dd = parseFloat(snap.risk.peak_equity) - parseFloat(snap.risk.realised_pnl);
  $('#kpi-dd').textContent = '$' + fmtUsd(dd);
  $('#kpi-md').textContent = `${snap.latency.md_p50_us.toFixed(0)} / ${snap.latency.md_p99_us.toFixed(0)} µs`;
  $('#kpi-rtt').textContent = `${snap.latency.order_rtt_p50_us.toFixed(0)} / ${snap.latency.order_rtt_p99_us.toFixed(0)} µs`;
  const kill = $('#kpi-kill');
  kill.textContent = snap.risk.kill_switch ? 'ON' : 'OFF';
  kill.className = snap.risk.kill_switch ? 'kill-on' : 'kill-off';

  renderPnl(snap.history);
  renderAsSurface(snap);
  renderXvenue(snap);
  renderParitySurface(snap);
  renderBookTable(snap.books);
  renderPositions(snap.risk.positions);
  renderSignals(snap.signals);
}

function renderPnl(history) {
  const byStrat = new Map();
  for (const p of history) {
    if (!byStrat.has(p.strategy)) byStrat.set(p.strategy, { x: [], y: [] });
    const s = byStrat.get(p.strategy);
    s.x.push(new Date(p.ts_ms));
    s.y.push(p.pnl);
  }
  const traces = [...byStrat.entries()].map(([name, d]) => ({
    type: 'scatter', mode: 'lines', name, x: d.x, y: d.y,
    line: { width: 2 }, hovertemplate: '%{y:.2f} USD<extra>%{fullData.name}</extra>',
  }));
  Plotly.react(PLOTS.pnl, traces, {
    ...dark,
    yaxis: { title: 'USD' },
    xaxis: { title: 'time' },
    showlegend: true,
  }, { displayModeBar: false, responsive: true });
}

function renderAsSurface(_snap) {
  // Independent of live data — visualises the model surface for the
  // currently-configured (γ, κ). Static-ish but recomputed every tick so we
  // can drop the live (σ, q) point on top.
  const gamma = 0.6;
  const kappa = 1.5;
  const sigmas = linspace(0.005, 0.08, 24);
  const invs = linspace(-2000, 2000, 24);
  const z = sigmas.map(sigma =>
    invs.map(_ =>
      gamma * sigma * sigma * 0.5 + (1 / gamma) * Math.log(1 + gamma / kappa)
    )
  );
  const r = sigmas.map((sigma, i) =>
    invs.map((q, j) => 0.5 - q * gamma * sigma * sigma * 0.5)
  );
  Plotly.react(PLOTS.asSurface, [
    {
      type: 'surface',
      x: invs, y: sigmas, z,
      colorscale: 'Viridis',
      opacity: 0.85,
      name: 'half-spread δ',
      colorbar: { title: 'δ', thickness: 12 },
    },
    {
      type: 'surface',
      x: invs, y: sigmas, z: r,
      colorscale: 'Cividis', showscale: false, opacity: 0.45,
      name: 'reservation r',
    },
  ], {
    ...dark,
    scene: {
      ...dark.scene,
      xaxis: { ...dark.scene.xaxis, title: 'inventory q' },
      yaxis: { ...dark.scene.yaxis, title: 'σ (per √sec)' },
      zaxis: { ...dark.scene.zaxis, title: 'price' },
      camera: { eye: { x: 1.5, y: -1.4, z: 0.85 } },
    },
  }, { displayModeBar: false, responsive: true });
}

function renderXvenue(snap) {
  const byVenue = new Map();
  for (const b of snap.books) {
    if (!b.microprice) continue;
    const v = b.market.venue;
    if (!byVenue.has(v)) byVenue.set(v, []);
    byVenue.get(v).push({ ts: snap.ts_ms, mid: b.microprice, market: `${b.market.venue}::${b.market.instrument}` });
  }
  const traces = [...byVenue.entries()].map(([v, points], i) => ({
    type: 'scatter3d',
    mode: 'lines+markers',
    name: v,
    x: points.map(p => new Date(p.ts)),
    y: points.map(_ => i),
    z: points.map(p => p.mid),
    line: { width: 4 },
    marker: { size: 3 },
  }));
  Plotly.react(PLOTS.xvenue, traces, {
    ...dark,
    scene: {
      ...dark.scene,
      xaxis: { ...dark.scene.xaxis, title: 'time' },
      yaxis: { ...dark.scene.yaxis, title: 'venue' },
      zaxis: { ...dark.scene.zaxis, title: 'YES price' },
      camera: { eye: { x: 1.4, y: -1.7, z: 0.6 } },
    },
  }, { displayModeBar: false, responsive: true });
}

function renderParitySurface(snap) {
  // Fair value of a digital under Black-Scholes:
  //   Φ((ln(S/K) − ½σ²τ) / (σ √τ))
  // We render it on a (S, σ) grid for the next 12h horizon (τ ~ 1.4e-3 yr).
  const tau = 12 / 8760;
  const K = guessStrike(snap) || 78213;
  const Ss = linspace(K * 0.95, K * 1.05, 32);
  const sigs = linspace(0.30, 1.10, 24);
  const z = Ss.map(S =>
    sigs.map(sig => {
      const d = (Math.log(S / K) - 0.5 * sig * sig * tau) / (sig * Math.sqrt(tau));
      return phi(d);
    })
  );
  const livePoint = liveBtcPoint(snap);
  const traces = [{
    type: 'surface',
    x: sigs, y: Ss, z,
    colorscale: 'Plasma',
    opacity: 0.85,
    colorbar: { title: 'P(YES)', thickness: 12 },
  }];
  if (livePoint) traces.push({
    type: 'scatter3d', mode: 'markers',
    x: [livePoint.sigma], y: [livePoint.S], z: [livePoint.yesMid],
    marker: { size: 6, color: '#5cffd1' },
    name: 'live YES mid',
  });
  Plotly.react(PLOTS.parity, traces, {
    ...dark,
    scene: {
      ...dark.scene,
      xaxis: { ...dark.scene.xaxis, title: 'σ (annual)' },
      yaxis: { ...dark.scene.yaxis, title: 'S (BTC mid)' },
      zaxis: { ...dark.scene.zaxis, title: 'P(BTC ≥ K)' },
      camera: { eye: { x: 1.4, y: -1.7, z: 0.7 } },
    },
  }, { displayModeBar: false, responsive: true });
}

function renderBookTable(books) {
  const rows = books.map(b => `<tr>
    <td>${b.market.venue}::${b.market.instrument}</td>
    <td>${b.bid?.toFixed(4) ?? '—'}</td>
    <td>${b.ask?.toFixed(4) ?? '—'}</td>
    <td>${b.microprice?.toFixed(4) ?? '—'}</td>
  </tr>`).join('');
  document.querySelector('#book-table tbody').innerHTML = rows;
}

function renderPositions(positions) {
  const rows = (positions || []).map(([market, p]) => `<tr>
    <td>${market.venue}::${market.instrument}</td>
    <td>${parseFloat(p.qty).toFixed(4)}</td>
    <td>${parseFloat(p.avg_entry).toFixed(4)}</td>
    <td>${parseFloat(p.realised_pnl).toFixed(2)}</td>
  </tr>`).join('');
  document.querySelector('#pos-table tbody').innerHTML = rows;
}

function renderSignals(signals) {
  const traces = [{
    type: 'scatter',
    mode: 'markers',
    x: signals.map(s => new Date(s.ts_ms)),
    y: signals.map(s => s.edge),
    text: signals.map(s => `${s.strategy} · ${s.market} · ${s.kind}`),
    marker: {
      size: 7,
      color: signals.map(s => s.edge),
      colorscale: 'RdYlGn',
      line: { width: 0 },
      colorbar: { title: 'edge', thickness: 10 },
    },
    hovertemplate: '%{text}<br>edge=%{y:.4f}<extra></extra>',
  }];
  Plotly.react(PLOTS.signals, traces, {
    ...dark, yaxis: { title: 'edge' }, xaxis: { title: 'time' },
  }, { displayModeBar: false, responsive: true });
}

function linspace(a, b, n) {
  const out = []; const dx = (b - a) / (n - 1);
  for (let i = 0; i < n; i++) out.push(a + dx * i);
  return out;
}
function phi(x) {
  // Abramowitz & Stegun 26.2.17 — accurate to ~7e-8.
  const a1 = 0.254829592, a2 = -0.284496736, a3 = 1.421413741,
        a4 = -1.453152027, a5 = 1.061405429, p = 0.3275911;
  const sign = x < 0 ? -1 : 1; x = Math.abs(x) / Math.SQRT2;
  const t = 1 / (1 + p * x);
  const y = 1 - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * Math.exp(-x * x);
  return 0.5 * (1 + sign * y);
}
function guessStrike(snap) {
  for (const b of snap.books) {
    const m = /-(\d+)-/.exec(b.market.instrument || '');
    if (m) return parseFloat(m[1]);
  }
  return null;
}
function liveBtcPoint(snap) {
  const perp = snap.books.find(b => b.market.venue === 'hl-perp');
  const yes = snap.books.find(b => /YES/.test(b.market.instrument));
  if (!perp?.microprice || !yes?.microprice) return null;
  return { S: perp.microprice, sigma: 0.65, yesMid: yes.microprice };
}

connect();
