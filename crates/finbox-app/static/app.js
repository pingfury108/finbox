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

  const acctSel = document.getElementById('acct-select');
  let activeAcct = '';

  // ---------- 账户切换器 ----------
  async function loadAccounts() {
    const list = await get('/api/accounts');
    if (!acctSel) return;
    acctSel.innerHTML = '';
    if (list.length === 0) {
      acctSel.innerHTML = '<option>暂无账户，请新建</option>';
      return;
    }
    list.forEach(a => {
      const o = document.createElement('option');
      o.value = a.name;
      o.textContent = a.name;
      acctSel.appendChild(o);
    });
    activeAcct = window.ACTIVE_ACCT && list.some(a => a.name === window.ACTIVE_ACCT)
      ? window.ACTIVE_ACCT : list[0].name;
    acctSel.value = activeAcct;
    localStorage.setItem('finbox_acct', activeAcct);
  }
  acctSel && acctSel.addEventListener('change', () => {
    activeAcct = acctSel.value;
    localStorage.setItem('finbox_acct', activeAcct);
    location.reload();
  });

  // ---------- 概览页 ----------
  async function renderOverview() {
    if (!document.getElementById('ov-cards')) return;
    const accts = await get('/api/accounts');
    const acct = accts.find(a => a.name === activeAcct) || accts[0];

    // 账户管理条（列表 + 删除）
    const mgmt = document.getElementById('acct-mgmt');
    if (mgmt) {
      mgmt.innerHTML = accts.map(a =>
        '<span class="acct-chip">' + esc(a.name) + '（¥' + fmt(a.total, 0) + '）' +
        '<button class="del-btn" data-name="' + esc(a.name) + '" title="删除此账户">✕</button></span>'
      ).join('') || '<span class="empty" style="display:inline">暂无账户</span>';
      mgmt.querySelectorAll('.del-btn').forEach(b => {
        b.addEventListener('click', async () => {
          const name = b.dataset.name;
          if (!confirm('确定删除账户「' + name + '」？其全部资金/持仓/记录将永久删除！')) return;
          try {
            await fetch('/api/account/' + encodeURIComponent(name), { method: 'DELETE' });
            location.reload();
          } catch (e) { alert('删除失败：' + e); }
        });
      });
    }

    if (!acct) {
      document.getElementById('ov-cards').innerHTML =
        '<div class="empty"><a href="/accounts/new">点击新建第一个模拟账户</a></div>';
      return;
    }
    document.getElementById('ov-cards').innerHTML = [
      card('总资产', '¥' + fmt(acct.total, 0)),
      card('可用现金', '¥' + fmt(acct.cash, 0)),
      card('收益率', pct(acct.return_pct), cls(acct.return_pct)),
      card('持仓', acct.position_count + ' 只'),
    ].join('');

    // 资产曲线
    let equity = [];
    try { equity = await get('/api/account/' + encodeURIComponent(acct.name) + '/equity'); } catch (e) {}
    renderEquity(equity);

    // 持仓
    let positions = [];
    try { positions = await get('/api/account/' + encodeURIComponent(acct.name) + '/positions'); } catch (e) {}
    const ptb = document.querySelector('#ov-positions tbody');
    document.getElementById('ov-pos-empty').style.display = positions.length === 0 ? '' : 'none';
    ptb.innerHTML = positions.map(p =>
      '<tr><td>' + p.thscode + '</td><td>' + esc(p.name) + '</td><td>' + p.quantity + '</td>' +
      '<td>' + fmt(p.avg_cost) + '</td><td>' + fmt(p.price) + '</td>' +
      '<td class="' + cls(p.pnl) + '">' + sign(p.pnl) + fmt(p.pnl, 0) + ' (' + pct(p.pnl_pct) + ')</td></tr>'
    ).join('');

    // 最近成交
    let trades = [];
    try { trades = await get('/api/account/' + encodeURIComponent(acct.name) + '/trades'); } catch (e) {}
    document.querySelector('#ov-trades tbody').innerHTML = trades.slice(0, 8).map(t =>
      '<tr><td>' + fmtTime(t.ts_ms) + '</td><td class="' + (t.side === 'BUY' ? 'up' : 'down') + '">' +
      (t.side === 'BUY' ? '买入' : '卖出') + '</td><td>' + t.thscode + '</td><td>' + t.quantity +
      '</td><td>' + fmt(t.price) + '</td></tr>'
    ).join('') || '<tr><td colspan=5 class="empty">暂无成交</td></tr>';

    // 最近 AI 建议
    let decisions = [];
    try { decisions = await get('/api/account/' + encodeURIComponent(acct.name) + '/decisions'); } catch (e) {}
    document.getElementById('ov-decisions').innerHTML = decisions.slice(0, 5).map(d =>
      '<div class="dec-item"><div class="meta">' + fmtTime(d.ts_ms) + ' · ' + esc(d.model) +
      '<span class="status status-' + d.status + '">' + statusLabel(d.status) + '</span></div>' +
      '<div class="note">' + esc(d.note) + '</div></div>'
    ).join('') || '<div class="empty">暂无 AI 建议</div>';

    function card(label, value, vcls) {
      return '<div class="card"><div class="label">' + label + '</div>' +
        '<div class="value ' + (vcls || '') + '">' + value + '</div></div>';
    }
  }

  function statusLabel(s) {
    return { parsed: '已解析', executed: '已执行', hold: '观望', rejected: '跳过', error: '出错' }[s] || s;
  }
  function fmtTime(ms) {
    return new Date(ms).toLocaleString('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' });
  }

  function renderEquity(pts) {
    const el = document.getElementById('equity-chart');
    if (!el) return;
    if (pts.length === 0) {
      el.innerHTML = '<div class="empty">暂无数据（首个交易日收盘后生成）</div>';
      return;
    }
    const chart = echarts.init(el);
    chart.setOption({
      backgroundColor: 'transparent',
      grid: { left: 60, right: 20, top: 20, bottom: 30 },
      tooltip: { trigger: 'axis', valueFormatter: v => '¥' + Number(v).toLocaleString() },
      xAxis: {
        type: 'category',
        data: pts.map(p => new Date(p.ts).toLocaleDateString('zh-CN')),
        axisLine: { lineStyle: { color: '#2a3140' } },
        axisLabel: { color: '#8b949e' },
      },
      yAxis: {
        type: 'value', scale: true,
        splitLine: { lineStyle: { color: '#2a3140' } },
        axisLabel: { color: '#8b949e' },
      },
      series: [{
        type: 'line', data: pts.map(p => p.total),
        smooth: true, showSymbol: false,
        lineStyle: { color: '#58a6ff', width: 2 },
        areaStyle: { color: 'rgba(88,166,255,0.12)' },
      }],
    });
    window.addEventListener('resize', () => chart.resize());
  }

  // ---------- 行情页（K线） ----------
  // 几大 A 股指数
  const INDEXES = [
    { code: '000001.SH', name: '上证指数' },
    { code: '399001.SZ', name: '深证成指' },
    { code: '399006.SZ', name: '创业板指' },
    { code: '000300.SH', name: '沪深300' },
    { code: '000905.SH', name: '中证500' },
  ];

  function initMarket() {
    const input = document.getElementById('sym-search');
    const suggest = document.getElementById('sym-suggest');
    if (!input) return;

    // 指数切换条
    const tabs = document.getElementById('index-tabs');
    if (tabs) {
      tabs.innerHTML = INDEXES.map((ix, i) =>
        '<button class="index-tab' + (i === 0 ? ' active' : '') + '" data-code="' + ix.code + '">' + ix.name + '</button>'
      ).join('');
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

    // 默认展示上证指数
    loadKline(INDEXES[0].code);
  }

  async function loadKline(code) {
    const el = document.getElementById('kline-chart');
    if (!el) return;
    let data;
    try { data = await get('/api/kline/' + encodeURIComponent(code)); }
    catch (e) {
      document.getElementById('kline-name').textContent = '未找到 ' + code;
      return;
    }
    // 指数用中文名，个股用 API 返回名
    const ix = INDEXES.find(i => i.code === code);
    document.getElementById('kline-name').textContent =
      (ix ? ix.name : data.name) + '（' + data.thscode + '）';
    const last = data.points[data.points.length - 1];
    if (last) {
      const chg = (last.ohlc[1] - last.ohlc[3]) / last.ohlc[3] * 100;
      document.getElementById('kline-quote').textContent =
        '最新 ' + fmt(last.ohlc[1]) + '  ' + pct(chg);
    }

    const chart = echarts.init(el);
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
          let s = '<b>' + p.date + '</b><br/>开 ' + fmt(p.ohlc[0]) +
            ' 收 ' + fmt(p.ohlc[1]) + '<br/>低 ' + fmt(p.ohlc[2]) + ' 高 ' + fmt(p.ohlc[3]) +
            '<br/>量 ' + Number(p.volume).toExponential(2);
          return s;
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
        {
          name: '日K', type: 'candlestick', data: data.points.map(p => p.ohlc),
          itemStyle: {
            color: '#f6465d', color0: '#0ecb81',
            borderColor: '#f6465d', borderColor0: '#0ecb81',
          },
        },
        { name: 'MA5', type: 'line', data: data.points.map(p => p.ma5),
          smooth: true, showSymbol: false, lineStyle: { width: 1, color: '#f0883e' } },
        { name: 'MA10', type: 'line', data: data.points.map(p => p.ma10),
          smooth: true, showSymbol: false, lineStyle: { width: 1, color: '#58a6ff' } },
        { name: 'MA20', type: 'line', data: data.points.map(p => p.ma20),
          smooth: true, showSymbol: false, lineStyle: { width: 1, color: '#d2a8ff' } },
        { name: 'MA60', type: 'line', data: data.points.map(p => p.ma60),
          smooth: true, showSymbol: false, lineStyle: { width: 1, color: '#3fb950' } },
        {
          name: '量', type: 'bar', xAxisIndex: 1, yAxisIndex: 1,
          data: data.points.map(p => p.volume),
          itemStyle: { color: 'rgba(88,166,255,0.4)' },
        },
      ],
    });
    window.addEventListener('resize', () => chart.resize());
  }

  // ---------- 持仓页 ----------
  async function renderPositions() {
    const tbody = document.querySelector('#pos-table tbody');
    if (!tbody) return;
    let positions = [];
    try { positions = await get('/api/account/' + encodeURIComponent(activeAcct) + '/positions'); } catch (e) {}
    document.getElementById('pos-empty').style.display = positions.length === 0 ? '' : 'none';
    tbody.innerHTML = positions.map(p =>
      '<tr><td>' + p.thscode + '</td><td>' + esc(p.name) + '</td><td>' + p.quantity + '</td>' +
      '<td>' + fmt(p.avg_cost) + '</td><td>' + fmt(p.price) + '</td>' +
      '<td class="' + cls(p.pnl) + '">' + sign(p.pnl) + fmt(p.pnl, 0) + '</td>' +
      '<td class="' + cls(p.pnl_pct) + '">' + pct(p.pnl_pct) + '</td></tr>'
    ).join('');
  }

  // ---------- 交易页 ----------
  async function renderTrades() {
    const tbody = document.querySelector('#trades-table tbody');
    if (!tbody) return;
    let trades = [];
    try { trades = await get('/api/account/' + encodeURIComponent(activeAcct) + '/trades'); } catch (e) {}
    tbody.innerHTML = trades.map(t =>
      '<tr><td>' + fmtTime(t.ts_ms) + '</td>' +
      '<td class="' + (t.side === 'BUY' ? 'up' : 'down') + '">' + (t.side === 'BUY' ? '买入' : '卖出') + '</td>' +
      '<td>' + t.thscode + '</td><td>' + esc(t.name) + '</td><td>' + t.quantity + '</td>' +
      '<td>' + fmt(t.price) + '</td><td>' + fmt(t.amount) + '</td><td>' + fmt(t.fee) + '</td></tr>'
    ).join('') || '<tr><td colspan=8 class="empty">暂无成交记录</td></tr>';
  }

  // ---------- 决策页 ----------
  async function renderDecisions() {
    const list = document.getElementById('dec-list');
    if (!list) return;
    let decisions = [];
    try { decisions = await get('/api/account/' + encodeURIComponent(activeAcct) + '/decisions'); } catch (e) {}
    list.innerHTML = decisions.map(d =>
      '<div class="dec-item"><div class="meta">' + fmtTime(d.ts_ms) + ' · ' + esc(d.model) +
      '<span class="status status-' + d.status + '">' + statusLabel(d.status) + '</span></div>' +
      '<div class="note">' + esc(d.note) + '</div>' +
      (d.raw_response ? '<div class="raw">' + esc(d.raw_response.slice(0, 400)) + '</div>' : '') +
      '</div>'
    ).join('') || '<div class="empty">暂无 AI 建议记录</div>';
  }

  // ---------- 初始化 ----------
  async function init() {
    try { await loadAccounts(); } catch (e) {}
    // 各页面渲染（并行，各自容错）
    await Promise.all([
      renderOverview(),
      renderPositions(),
      renderTrades(),
      renderDecisions(),
    ].map(p => p.catch(e => console.error(e))));
    initMarket();
  }
  document.addEventListener('DOMContentLoaded', init);
})();
