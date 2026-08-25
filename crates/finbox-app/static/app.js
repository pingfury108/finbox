/* finbox 前端逻辑 */
(function () {
  'use strict';

  // ---------- 工具 ----------
  function fmt(n, d) {
    if (n === null || n === undefined || isNaN(n)) return '-';
    d = d || 2;
    return Number(n).toFixed(d);
  }
  function cls(v) { return v >= 0 ? 'up' : 'down'; }
  function sign(v) { return v > 0 ? '+' : ''; }
  function pct(v) { return v === null || v === undefined ? '-' : sign(v) + fmt(v) + '%'; }
  function esc(s) {
    return String(s || '').replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  }
  async function get(url) {
    const r = await fetch(url);
    if (!r.ok) throw new Error(url + ' -> ' + r.status);
    return r.json();
  }
  function fmtTime(ms) {
    return new Date(ms).toLocaleString('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' });
  }
  function statusLabel(s) {
    return { parsed: '已解析', executed: '已执行', hold: '观望', rejected: '跳过', error: '出错' }[s] || s;
  }

  // ========== 全局状态条（60s 轮询） ==========
  let statusTimer = null;
  async function renderStatusBar() {
    const bar = document.getElementById('statusbar');
    if (!bar) return;
    let ov;
    try { ov = await get('/api/market/overview'); } catch (e) { return; }
    const ixHtml = ov.indexes.map(i =>
      '<span class="ix"><span class="nm">' + esc(i.name) + '</span>' +
      '<span class="pv ' + cls(i.pct) + '">' + fmt(i.price) + '</span>' +
      '<span class="' + cls(i.pct) + '">' + pct(i.pct) + '</span></span>'
    ).join('<span class="sep"></span>');
    const regimeText = { 'risk-on': 'risk-on 积极', 'neutral': 'neutral 中性', 'risk-off': 'risk-off 防守' }[ov.regime] || ov.regime;
    bar.innerHTML = ixHtml +
      '<span class="sep"></span>' +
      '<span class="breadth">涨 ' + ov.up + ' / ' + ov.total + '</span>' +
      '<span class="regime ' + ov.regime + '">' + regimeText + '</span>';
    // 系统状态灯：快照时间超过 10 分钟（非交易时段除外）提示
    const sys = document.getElementById('sys-status');
    if (sys) {
      const lag = Date.now() - ov.ts_ms;
      const stale = ov.ts_ms > 0 && lag > 10 * 60 * 1000;
      sys.className = 'sys-status' + (stale ? ' stale' : '');
      sys.innerHTML = '<span class="dot"></span>' +
        (ov.ts_ms > 0 ? '数据 ' + fmtTime(ov.ts_ms) : '待采集');
    }
  }
  function startStatusBar() {
    renderStatusBar();
    clearInterval(statusTimer);
    statusTimer = setInterval(renderStatusBar, 60000);
  }

  // 迷你资产曲线（SVG sparkline，不引 ECharts 实例，轻量）
  function sparklineSvg(data, w, h) {
    if (!data || data.length < 2) return '<div class="empty" style="padding:8px">—</div>';
    const min = Math.min.apply(null, data), max = Math.max.apply(null, data);
    const range = max - min || 1;
    const step = w / (data.length - 1);
    const pts = data.map((v, i) => (i * step).toFixed(1) + ',' + (h - (v - min) / range * (h - 4) - 2).toFixed(1)).join(' ');
    const up = data[data.length - 1] >= data[0];
    const color = up ? 'var(--up)' : 'var(--down)';
    return '<svg width="' + w + '" height="' + h + '" style="display:block">' +
      '<polyline points="' + pts + '" fill="none" stroke="' + color + '" stroke-width="1.5"/></svg>';
  }

  // ========== 模拟主页：驾驶舱 ==========
  async function renderHome() {
    const grid = document.getElementById('acct-grid');
    if (!grid) return;
    const accts = await get('/api/accounts');

    // 总览卡片（所有账户合计）
    const ovCards = document.getElementById('overview-cards');
    if (ovCards) {
      const totalSum = accts.reduce((s, a) => s + a.total, 0);
      const todaySum = accts.reduce((s, a) => s + a.today_pnl, 0);
      const retSum = accts.length ? accts.reduce((s, a) => s + a.return_pct, 0) / accts.length : 0;
      ovCards.innerHTML =
        ovCard('总资产', '¥' + fmt(totalSum, 0), '') +
        ovCard('今日盈亏', sign(todaySum) + fmt(todaySum, 0), cls(todaySum)) +
        ovCard('平均收益率', pct(retSum), cls(retSum));
    }

    const empty = document.getElementById('acct-empty');
    if (accts.length === 0) {
      grid.innerHTML = '';
      empty.style.display = '';
      return;
    }
    empty.style.display = 'none';
    grid.innerHTML = accts.map(a => {
      const rp = a.return_pct;
      return '<div class="acct-card">' +
        '<div class="acct-card-name">' + esc(a.name) + '</div>' +
        '<div class="acct-card-total">¥' + fmt(a.total, 0) + '</div>' +
        '<div class="acct-card-today">今日 <span class="' + cls(a.today_pnl) + '">' + sign(a.today_pnl) + fmt(a.today_pnl, 0) + '</span>' +
        ' · 收益率 <span class="' + cls(rp) + '">' + pct(rp) + '</span></div>' +
        (a.sparkline && a.sparkline.length > 1 ? '<div class="acct-card-spark">' + sparklineSvg(a.sparkline, 180, 36) + '</div>' : '') +
        '<div class="acct-card-row">持仓 ' + a.position_count + ' 只 · 现金 ¥' + fmt(a.cash, 0) + '</div>' +
        '<div class="acct-card-ops">' +
          '<a class="btn-ghost" href="/account/' + encodeURIComponent(a.name) + '">查看</a>' +
          '<button class="del-btn" data-name="' + esc(a.name) + '">删除</button>' +
        '</div></div>';
    }).join('');

    // 删除
    grid.querySelectorAll('.del-btn').forEach(b => {
      b.addEventListener('click', async () => {
        const name = b.dataset.name;
        if (!confirm('确定删除账户「' + name + '」？其全部资金/持仓/记录将永久删除！')) return;
        try {
          await fetch('/api/account/' + encodeURIComponent(name), { method: 'DELETE' });
          location.reload();
        } catch (e) { alert('删除失败：' + e); }
      });
    });

    // 今日决策动态
    renderDecisionFeed();

    // 盘中自动刷新（60s）
    if (!window._homeTimer) {
      window._homeTimer = setInterval(() => {
        if (document.getElementById('acct-grid')) { renderHome(); } else { clearInterval(window._homeTimer); window._homeTimer = null; }
      }, 60000);
    }
  }

  function ovCard(label, value, vcls) {
    return '<div class="ov-card"><div class="label">' + label + '</div>' +
      '<div class="value ' + (vcls || '') + '">' + value + '</div></div>';
  }

  async function renderDecisionFeed() {
    const feed = document.getElementById('decision-feed');
    if (!feed) return;
    let items = [];
    try { items = await get('/api/decisions/recent'); } catch (e) {}
    if (items.length === 0) {
      feed.innerHTML = '<div class="empty">今日暂无决策记录（交易日盘中每 30 分钟轮询）</div>';
      return;
    }
    feed.innerHTML = items.map(d =>
      '<div class="feed-item">' +
      '<span class="feed-time">' + fmtTime(d.ts_ms) + '</span>' +
      '<span class="feed-acct">' + esc(d.account) + '</span>' +
      '<span class="status status-' + d.status + '">' + statusLabel(d.status) + '</span>' +
      '<span class="feed-note">' + esc(d.note) + '</span>' +
      '</div>'
    ).join('');
  }

  // ========== 账户详情页 ==========
  async function renderAccount() {
    const cards = document.getElementById('acct-cards');
    if (!cards) return;
    // 从 URL 取账户名
    const m = location.pathname.match(/^\/account\/([^\/]+)/);
    if (!m) return;
    const name = decodeURIComponent(m[1]);
    document.getElementById('acct-title').textContent = '账户「' + name + '」';

    const accts = await get('/api/accounts');
    const acct = accts.find(a => a.name === name);
    if (acct) {
      const posPct = acct.total > 0 ? (acct.market_value / acct.total * 100) : 0;
      cards.innerHTML = [
        card('总资产', '¥' + fmt(acct.total, 0)),
        card('今日盈亏', sign(acct.today_pnl) + fmt(acct.today_pnl, 0), cls(acct.today_pnl)),
        card('累计收益', pct(acct.return_pct), cls(acct.return_pct)),
        card('可用现金', '¥' + fmt(acct.cash, 0)),
        card('持仓市值', '¥' + fmt(acct.market_value, 0)),
        card('仓位', fmt(posPct, 1) + '%', posPct > 60 ? 'up' : ''),
      ].join('');
    }

    renderRisk(name);

    // 资产曲线（含基准）
    let equity = null;
    try { equity = await get('/api/account/' + encodeURIComponent(name) + '/equity'); } catch (e) {}
    renderEquity(equity);

    // Tab 切换
    const tabs = document.getElementById('acct-tabs');
    if (tabs) {
      tabs.querySelectorAll('.tab').forEach(t => {
        t.addEventListener('click', () => {
          tabs.querySelectorAll('.tab').forEach(x => x.classList.remove('active'));
          t.classList.add('active');
          ['positions', 'trades', 'decisions'].forEach(p => {
            document.getElementById('tab-' + p).style.display = t.dataset.tab === p ? '' : 'none';
          });
        });
      });
    }
    await Promise.all([
      renderPositions(name),
      renderTrades(name),
      renderDecisions(name),
    ]);

    function card(label, value, vcls) {
      return '<div class="card"><div class="label">' + label + '</div>' +
        '<div class="value ' + (vcls || '') + '">' + value + '</div></div>';
    }
  }

  // 风控状态条
  async function renderRisk(name) {
    const el = document.getElementById('risk-status');
    if (!el) return;
    let r;
    try { r = await get('/api/account/' + encodeURIComponent(name) + '/risk'); } catch (e) { return; }
    let html = '<div class="risk-row">';
    if (r.fuse_active) {
      html += '<span class="regime risk-off">熔断中（回撤超限，暂停买入）</span>';
    } else {
      html += '<span class="regime neutral">风控正常</span>';
    }
    html += '<span class="risk-item">当前回撤 <b class="' + cls(-r.drawdown_pct) + '">' + fmt(-r.drawdown_pct, 1) + '%</b></span>';
    html += '<span class="risk-item">仓位 <b>' + fmt(r.position_pct, 1) + '%</b></span>';
    html += '</div>';
    if (r.positions && r.positions.length) {
      html += '<table class="tbl" style="margin-top:10px"><thead><tr><th>标的</th><th>现价</th><th>成本</th><th>距止损线(-5%)</th><th>距止盈线(+15%)</th></tr></thead><tbody>';
      html += r.positions.map(p => {
        const stopCls = p.to_stop_pct < 2 ? 'up' : '';
        return '<tr><td>' + esc(p.name) + '</td><td>' + fmt(p.price) + '</td><td>' + fmt(p.avg_cost) + '</td>' +
          '<td class="' + stopCls + '">' + fmt(p.to_stop_pct, 1) + '%</td>' +
          '<td>' + fmt(p.to_profit_pct, 1) + '%</td></tr>';
      }).join('');
      html += '</tbody></table>';
    } else {
      html += '<div class="empty">空仓，无风控敞口</div>';
    }
    el.innerHTML = html;
  }

  function renderEquity(data) {
    const el = document.getElementById('equity-chart');
    if (!el) return;
    const pts = (data && data.points) || [];
    const bench = (data && data.benchmark) || [];
    if (pts.length === 0) {
      el.innerHTML = '<div class="empty">暂无数据（首个交易日收盘后生成）</div>';
      return;
    }
    const chart = echarts.getInstanceByDom(el) || echarts.init(el);
    const series = [{
      name: '账户总资产',
      type: 'line', data: pts.map(p => [new Date(p.ts).toLocaleDateString('zh-CN'), p.total]),
      smooth: true, showSymbol: false,
      lineStyle: { color: '#58a6ff', width: 2 },
      areaStyle: { color: 'rgba(88,166,255,0.12)' },
    }];
    if (bench.length) {
      series.push({
        name: '沪深300（基准）',
        type: 'line', data: bench.map(p => [new Date(p.ts).toLocaleDateString('zh-CN'), p.total]),
        smooth: true, showSymbol: false,
        lineStyle: { color: '#8b949e', width: 1, type: 'dashed' },
      });
    }
    chart.setOption({
      backgroundColor: 'transparent',
      legend: { textStyle: { color: '#8b949e' }, top: 0 },
      grid: { left: 70, right: 20, top: 30, bottom: 30 },
      tooltip: { trigger: 'axis', valueFormatter: v => '¥' + Number(v).toLocaleString() },
      xAxis: {
        type: 'category',
        axisLine: { lineStyle: { color: '#2a3140' } },
        axisLabel: { color: '#8b949e' },
      },
      yAxis: {
        type: 'value', scale: true,
        splitLine: { lineStyle: { color: '#2a3140' } },
        axisLabel: { color: '#8b949e' },
      },
      series: series,
    });
    window.addEventListener('resize', () => chart.resize());
  }

  async function renderPositions(name) {
    const pane = document.getElementById('tab-positions');
    if (!pane) return;
    let positions = [];
    try { positions = await get('/api/account/' + encodeURIComponent(name) + '/positions'); } catch (e) {}
    if (positions.length === 0) {
      pane.innerHTML = '<div class="empty">（空仓）</div>';
      return;
    }
    pane.innerHTML = '<table class="tbl"><thead><tr><th>代码</th><th>名称</th><th>数量</th><th>成本</th><th>现价</th><th>浮动盈亏</th><th>盈亏率</th></tr></thead><tbody>' +
      positions.map(p =>
        '<tr><td>' + p.thscode + '</td><td>' + esc(p.name) + '</td><td>' + p.quantity + '</td>' +
        '<td>' + fmt(p.avg_cost) + '</td><td>' + fmt(p.price) + '</td>' +
        '<td class="' + cls(p.pnl) + '">' + sign(p.pnl) + fmt(p.pnl, 0) + '</td>' +
        '<td class="' + cls(p.pnl_pct) + '">' + pct(p.pnl_pct) + '</td></tr>'
      ).join('') + '</tbody></table>';
  }

  async function renderTrades(name) {
    const pane = document.getElementById('tab-trades');
    if (!pane) return;
    let trades = [];
    try { trades = await get('/api/account/' + encodeURIComponent(name) + '/trades'); } catch (e) {}
    if (trades.length === 0) {
      pane.innerHTML = '<div class="empty">暂无成交记录</div>';
      return;
    }
    pane.innerHTML = '<table class="tbl"><thead><tr><th>时间</th><th>方向</th><th>代码</th><th>名称</th><th>数量</th><th>价格</th><th>金额</th><th>费用</th></tr></thead><tbody>' +
      trades.map(t =>
        '<tr><td>' + fmtTime(t.ts_ms) + '</td>' +
        '<td class="' + (t.side === 'BUY' ? 'up' : 'down') + '">' + (t.side === 'BUY' ? '买入' : '卖出') + '</td>' +
        '<td>' + t.thscode + '</td><td>' + esc(t.name) + '</td><td>' + t.quantity + '</td>' +
        '<td>' + fmt(t.price) + '</td><td>' + fmt(t.amount) + '</td><td>' + fmt(t.fee) + '</td></tr>'
      ).join('') + '</tbody></table>';
  }

  async function renderDecisions(name) {
    const pane = document.getElementById('tab-decisions');
    if (!pane) return;
    let decisions = [];
    try { decisions = await get('/api/account/' + encodeURIComponent(name) + '/decisions'); } catch (e) {}
    if (decisions.length === 0) {
      pane.innerHTML = '<div class="empty">暂无 AI 建议记录</div>';
      return;
    }
    pane.innerHTML = decisions.map((d, i) => {
      let acts = [];
      try { acts = JSON.parse(d.actions || '[]'); } catch (e) {}
      const actHtml = acts.map(a => {
        const clsMap = { buy: 'up', sell: 'down', hold: '' };
        const labelMap = { buy: '买入', sell: '卖出', hold: '观望' };
        return '<span class="act-badge ' + (clsMap[a.action] || '') + '">' +
          (labelMap[a.action] || a.action) + ' ' + esc(a.symbol || '') +
          (a.quantity ? ' ' + a.quantity + '股' : '') + '</span>';
      }).join(' ');
      return '<div class="dec-card">' +
        '<div class="dec-card-head">' +
          '<span class="feed-time">' + fmtTime(d.ts_ms) + '</span>' +
          '<span class="status status-' + d.status + '">' + statusLabel(d.status) + '</span>' +
          '<span class="dec-model">' + esc(d.model) + '</span>' +
        '</div>' +
        (actHtml ? '<div class="dec-acts">' + actHtml + '</div>' : '') +
        '<div class="note">' + esc(d.note) + '</div>' +
        (d.raw_response ? '<div class="dec-toggle" data-i="' + i + '">展开原文 ▾</div>' +
          '<div class="raw" id="dec-raw-' + i + '" style="display:none">' + esc(d.raw_response.slice(0, 800)) + '</div>' : '') +
        '</div>';
    }).join('');
    // 展开/收起原文
    pane.querySelectorAll('.dec-toggle').forEach(t => {
      t.addEventListener('click', () => {
        const raw = document.getElementById('dec-raw-' + t.dataset.i);
        const show = raw.style.display === 'none';
        raw.style.display = show ? '' : 'none';
        t.textContent = show ? '收起原文 ▴' : '展开原文 ▾';
      });
    });
  }

  // ========== 行情页（K线 + 全景）==========
  const INDEXES = [
    { code: '000001.SH', name: '上证指数' },
    { code: '399001.SZ', name: '深证成指' },
    { code: '399006.SZ', name: '创业板指' },
    { code: '000300.SH', name: '沪深300' },
    { code: '000905.SH', name: '中证500' },
  ];

  let curKlineCode = INDEXES[0].code;
  let klineTimer = null;

  function initMarket() {
    const input = document.getElementById('sym-search');
    if (!input) return;
    const suggest = document.getElementById('sym-suggest');

    const tabs = document.getElementById('index-tabs');
    if (tabs) {
      tabs.addEventListener('click', e => {
        const b = e.target.closest('.index-tab');
        if (!b) return;
        tabs.querySelectorAll('.index-tab').forEach(x => x.classList.remove('active'));
        b.classList.add('active');
        loadKline(b.dataset.code);
      });
    }

    let timer = null;
    input.addEventListener('input', () => {
      clearTimeout(timer);
      timer = setTimeout(async () => {
        const q = input.value.trim();
        if (q.length < 2) { suggest.style.display = 'none'; return; }
        const list = await get('/api/search?q=' + encodeURIComponent(q));
        suggest.innerHTML = list.map(s =>
          '<div class="item" data-code="' + s.thscode + '"><span>' + esc(s.name) + '</span>' +
          '<span class="code">' + s.thscode + '</span></div>'
        ).join('');
        suggest.style.display = list.length ? 'block' : 'none';
      }, 250);
    });
    suggest.addEventListener('click', e => {
      const item = e.target.closest('.item');
      if (item) {
        input.value = item.dataset.code;
        suggest.style.display = 'none';
        tabs && tabs.querySelectorAll('.index-tab').forEach(x => x.classList.remove('active'));
        loadKline(item.dataset.code);
      }
    });
    input.addEventListener('keydown', e => {
      if (e.key === 'Enter' && input.value.trim().length >= 4) {
        tabs && tabs.querySelectorAll('.index-tab').forEach(x => x.classList.remove('active'));
        loadKline(input.value.trim().toUpperCase());
        suggest.style.display = 'none';
      }
    });

    loadKline(INDEXES[0].code);
    refreshIndexTabs();
    renderDistribution();
    renderHotList();
    // 60s 自动刷新：指数条/分布/热榜/当前K线
    setInterval(() => {
      if (!document.getElementById('sym-search')) return;
      refreshIndexTabs();
      renderDistribution();
      loadKline(curKlineCode, true);
    }, 60000);
  }

  // 指数切换条（带实时价）
  async function refreshIndexTabs() {
    const tabs = document.getElementById('index-tabs');
    if (!tabs) return;
    let ov;
    try { ov = await get('/api/market/overview'); } catch (e) { return; }
    const active = tabs.querySelector('.index-tab.active');
    const activeCode = active ? active.dataset.code : INDEXES[0].code;
    tabs.innerHTML = INDEXES.map(ix => {
      const q = ov.indexes.find(i => i.thscode === ix.code);
      const price = q ? fmt(q.price) : '-';
      const pctTxt = q ? pct(q.pct) : '';
      const c = q ? cls(q.pct) : '';
      return '<button class="index-tab' + (ix.code === activeCode ? ' active' : '') + '" data-code="' + ix.code + '">' +
        ix.name + ' <span class="' + c + '">' + price + ' ' + pctTxt + '</span></button>';
    }).join('');
  }

  // 涨跌分布柱状图
  async function renderDistribution() {
    const el = document.getElementById('dist-chart');
    if (!el) return;
    let dist;
    try { dist = await get('/api/market/distribution'); } catch (e) { return; }
    const order = ['涨停', '>5%', '0~5%', '平', '-5~0%', '<-5%', '跌停'];
    const map = {};
    dist.forEach(d => { map[d[0]] = d[1]; });
    const colors = { '涨停': '#f6465d', '>5%': '#ff7a8c', '0~5%': '#ff9eaa', '平': '#8b949e', '-5~0%': '#4ade9e', '<-5%': '#22d68a', '跌停': '#0ecb81' };
    const chart = echarts.getInstanceByDom(el) || echarts.init(el);
    chart.setOption({
      backgroundColor: 'transparent',
      grid: { left: 8, right: 8, top: 10, bottom: 20 },
      xAxis: { type: 'category', data: order, axisLabel: { color: '#8b949e', fontSize: 10, interval: 0 },
        axisLine: { lineStyle: { color: '#2a3140' } } },
      yAxis: { type: 'value', show: false },
      tooltip: { trigger: 'axis' },
      series: [{
        type: 'bar', barWidth: '60%',
        data: order.map(k => ({ value: map[k] || 0, itemStyle: { color: colors[k], borderRadius: [3, 3, 0, 0] } })),
        label: { show: true, position: 'top', color: '#8b949e', fontSize: 10 },
      }],
    });
  }

  // 热股榜 TOP10
  async function renderHotList() {
    const el = document.getElementById('hot-list');
    if (!el) return;
    let data;
    try { data = await get('/api/market/hot'); } catch (e) { el.innerHTML = '<div class="empty">加载失败</div>'; return; }
    const items = (data.item || []).slice(0, 10);
    if (!items.length) { el.innerHTML = '<div class="empty">暂无数据</div>'; return; }
    el.innerHTML = items.map(it => {
      const trendIcon = it.rank_trend === 'up' ? '↑' : it.rank_trend === 'down' ? '↓' : '—';
      const trendCls = it.rank_trend === 'up' ? 'up' : it.rank_trend === 'down' ? 'down' : '';
      return '<div class="hot-item" data-code="' + it.thscode + '">' +
        '<span class="hot-rank' + (it.rank <= 3 ? ' top' : '') + '">' + it.rank + '</span>' +
        '<span class="hot-name">' + esc(it.name) + '</span>' +
        '<span class="hot-pct ' + trendCls + '">' + trendIcon + '</span></div>';
    }).join('');
    el.querySelectorAll('.hot-item').forEach(item => {
      item.addEventListener('click', () => {
        const tabs = document.getElementById('index-tabs');
        tabs && tabs.querySelectorAll('.index-tab').forEach(x => x.classList.remove('active'));
        loadKline(item.dataset.code);
      });
    });
  }

  async function loadKline(code, silent) {
    const el = document.getElementById('kline-chart');
    if (!el) return;
    curKlineCode = code;
    let data;
    try { data = await get('/api/kline/' + encodeURIComponent(code)); }
    catch (e) {
      if (!silent) document.getElementById('kline-name').textContent = '未找到 ' + code;
      return;
    }
    const ix = INDEXES.find(i => i.code === code);
    document.getElementById('kline-name').textContent =
      (ix ? ix.name : data.name) + '（' + data.thscode + '）';
    const last = data.points[data.points.length - 1];
    if (last) {
      const chg = (last.ohlc[1] - last.ohlc[3]) / last.ohlc[3] * 100;
      document.getElementById('kline-quote').textContent =
        '最新 ' + fmt(last.ohlc[1]) + '  ' + pct(chg);
    }

    const chart = echarts.getInstanceByDom(el) || echarts.init(el);
    chart.setOption({
      backgroundColor: 'transparent',
      animation: false,
      legend: { data: ['MA5', 'MA10', 'MA20', 'MA60'], textStyle: { color: '#8b949e' }, top: 0 },
      tooltip: {
        trigger: 'axis', axisPointer: { type: 'cross' },
        formatter: params => {
          const d = params[0] && params[0].dataIndex;
          if (d === undefined) return '';
          const p = data.points[d];
          return '<b>' + p.date + '</b><br/>开 ' + fmt(p.ohlc[0]) +
            ' 收 ' + fmt(p.ohlc[1]) + '<br/>低 ' + fmt(p.ohlc[2]) + ' 高 ' + fmt(p.ohlc[3]) +
            '<br/>量 ' + Number(p.volume).toExponential(2);
        },
      },
      axisPointer: { link: [{ xAxisIndex: 'all' }] },
      grid: [
        { left: 60, right: 20, top: 32, height: '58%' },
        { left: 60, right: 20, top: '74%', height: '18%' },
      ],
      xAxis: [
        { type: 'category', data: data.points.map(p => p.date), gridIndex: 0,
          axisLine: { lineStyle: { color: '#2a3140' } }, axisLabel: { color: '#8b949e' } },
        { type: 'category', data: data.points.map(p => p.date), gridIndex: 1,
          axisLine: { lineStyle: { color: '#2a3140' } }, axisLabel: { show: false } },
      ],
      yAxis: [
        { scale: true, gridIndex: 0, splitLine: { lineStyle: { color: '#2a3140' } },
          axisLabel: { color: '#8b949e' } },
        { scale: true, gridIndex: 1, splitLine: { show: false },
          axisLabel: { color: '#8b949e' } },
      ],
      dataZoom: [
        { type: 'inside', xAxisIndex: [0, 1], start: 82, end: 100 },
        { type: 'slider', xAxisIndex: [0, 1], bottom: 2, height: 16,
          borderColor: '#2a3140', fillerColor: 'rgba(88,166,255,0.1)' },
      ],
      series: [
        { name: '日K', type: 'candlestick', data: data.points.map(p => p.ohlc),
          itemStyle: { color: '#f6465d', color0: '#0ecb81', borderColor: '#f6465d', borderColor0: '#0ecb81' } },
        { name: 'MA5', type: 'line', data: data.points.map(p => p.ma5), smooth: true, showSymbol: false, lineStyle: { width: 1, color: '#f0883e' } },
        { name: 'MA10', type: 'line', data: data.points.map(p => p.ma10), smooth: true, showSymbol: false, lineStyle: { width: 1, color: '#58a6ff' } },
        { name: 'MA20', type: 'line', data: data.points.map(p => p.ma20), smooth: true, showSymbol: false, lineStyle: { width: 1, color: '#d2a8ff' } },
        { name: 'MA60', type: 'line', data: data.points.map(p => p.ma60), smooth: true, showSymbol: false, lineStyle: { width: 1, color: '#3fb950' } },
        { name: '量', type: 'bar', xAxisIndex: 1, yAxisIndex: 1, data: data.points.map(p => p.volume),
          itemStyle: { color: 'rgba(88,166,255,0.4)' } },
      ],
    });
    window.addEventListener('resize', () => chart.resize());
  }

  // ---------- 初始化 ----------
  async function init() {
    startStatusBar();
    try { await renderHome(); } catch (e) { console.error(e); }
    try { await renderAccount(); } catch (e) { console.error(e); }
    initMarket();
  }
  document.addEventListener('DOMContentLoaded', init);
})();
