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

    // 持仓 + 最近成交 + 决策 用服务端渲染太重，这里留空提示（数据经账户页/持仓页）
    document.getElementById('ov-positions').querySelector('tbody').innerHTML =
      acct.position_count === 0 ? '' : '<tr><td colspan=6 class="empty">详见「持仓」页</td></tr>';
    document.getElementById('ov-pos-empty').style.display = acct.position_count === 0 ? '' : 'none';
    document.getElementById('ov-trades').querySelector('tbody').innerHTML =
      '<tr><td colspan=5 class="empty">详见「交易」页</td></tr>';
    document.getElementById('ov-decisions').innerHTML =
      '<div class="empty">详见「AI 建议」页</div>';

    function card(label, value, vcls) {
      return '<div class="card"><div class="label">' + label + '</div>' +
        '<div class="value ' + (vcls || '') + '">' + value + '</div></div>';
    }
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
  function initMarket() {
    const input = document.getElementById('sym-search');
    const suggest = document.getElementById('sym-suggest');
    if (!input) return;

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
        loadKline(item.dataset.code);
      }
    });
    input.addEventListener('keydown', e => {
      if (e.key === 'Enter' && input.value.trim().length >= 4) {
        loadKline(input.value.trim().toUpperCase());
        suggest.style.display = 'none';
      }
    });

    // 默认展示一只
    loadKline('600519.SH');
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
    document.getElementById('kline-name').textContent = data.name + '（' + data.thscode + '）';
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
        { type: 'inside', xAxisIndex: [0, 1], start: 40, end: 100 },
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
    // 持仓数据当前无 API，从账户页渲染；此处用占位
    tbody.innerHTML = '<tr><td colspan=7 class="empty">持仓数据请查看概览页（交易时段自动更新）</td></tr>';
  }

  // ---------- 初始化 ----------
  async function init() {
    try { await loadAccounts(); } catch (e) {}
    try { await renderOverview(); } catch (e) { console.error(e); }
    initMarket();
    try { await renderPositions(); } catch (e) {}
  }
  document.addEventListener('DOMContentLoaded', init);
})();
