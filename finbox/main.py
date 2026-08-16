"""FastAPI Web：账户概览 / 交易记录 / AI 决策日志"""

import logging
from contextlib import asynccontextmanager

from fastapi import FastAPI, Form, Request
from fastapi.responses import HTMLResponse, RedirectResponse
from fastapi.templating import Jinja2Templates
from sqlalchemy import func, select

from . import config, engine, scheduler
from .db import SessionLocal
from .models import AccountSnapshot, AIDecision, DailyBar, Position, Quote, Review, Screening, Trade

logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(name)s %(message)s")

templates = Jinja2Templates(directory="finbox/templates")


@asynccontextmanager
async def lifespan(app: FastAPI):
    sched = scheduler.start_scheduler()
    yield
    sched.shutdown()


app = FastAPI(title="finbox", lifespan=lifespan)


@app.get("/", response_class=HTMLResponse)
def index(request: Request):
    with SessionLocal() as s:
        account = engine.get_account(s)
        positions = s.scalars(select(Position)).all()
        prices = engine.latest_prices(s, [p.symbol for p in positions])
        market_value = sum(prices.get(p.symbol, p.avg_cost) * p.quantity for p in positions)
        snaps = s.scalars(select(AccountSnapshot).order_by(AccountSnapshot.ts)).all()
        recent_trades = s.scalars(select(Trade).order_by(Trade.ts.desc()).limit(10)).all()
        quote_count = s.scalar(select(func.count(Quote.id)))
    rows = [
        {
            "symbol": p.symbol, "name": p.name, "quantity": p.quantity,
            "avg_cost": p.avg_cost, "price": prices.get(p.symbol),
            "pnl_pct": ((prices[p.symbol] / p.avg_cost - 1) * 100) if p.symbol in prices else None,
        }
        for p in positions
    ]
    return templates.TemplateResponse(
        request,
        "index.html",
        {
            "cash": account.cash,
            "initial": account.initial_capital,
            "market_value": market_value,
            "total": account.cash + market_value,
            "positions": rows,
            "snaps": snaps,
            "trades": recent_trades,
            "quote_count": quote_count,
        },
    )


@app.get("/trades", response_class=HTMLResponse)
def trades(request: Request):
    with SessionLocal() as s:
        items = s.scalars(select(Trade).order_by(Trade.ts.desc()).limit(200)).all()
    return templates.TemplateResponse(request, "trades.html", {"trades": items})


@app.get("/decisions", response_class=HTMLResponse)
def decisions(request: Request):
    with SessionLocal() as s:
        items = s.scalars(select(AIDecision).order_by(AIDecision.ts.desc()).limit(100)).all()
        reviews = s.scalars(select(Review)).all()
    review_map: dict[int, list[Review]] = {}
    for r in reviews:
        review_map.setdefault(r.decision_id, []).append(r)
    return templates.TemplateResponse(
        request, "decisions.html", {"decisions": items, "review_map": review_map}
    )


@app.get("/history", response_class=HTMLResponse)
def history(request: Request, symbol: str = "", days: int = 60):
    from .collector import live_quote

    with SessionLocal() as s:
        # 可选股票 = 本地有数据的 + 自选池；名称从行情/初筛/持仓补
        symbols = sorted(
            {
                *config.WATCHLIST,
                *s.scalars(select(DailyBar.symbol).distinct()).all(),
                *s.scalars(select(Quote.symbol).distinct()).all(),
            }
        )
        names = _name_map(s)
        if not symbol and symbols:
            symbol = symbols[0]
        bars, minutes, name = [], [], names.get(symbol, "")
        live = None
        if symbol:
            bars = s.scalars(
                select(DailyBar)
                .where(DailyBar.symbol == symbol)
                .order_by(DailyBar.date.desc())
                .limit(days)
            ).all()
            latest_q = s.scalars(
                select(Quote).where(Quote.symbol == symbol).order_by(Quote.ts.desc()).limit(300)
            ).all()
            if latest_q:
                name = latest_q[0].name
                day = latest_q[0].ts.date()
                minutes = [q for q in latest_q if q.ts.date() == day]
            if not bars and not minutes:  # 本地无数据：实时拉一次（不落库）
                live = live_quote(symbol)
                if live:
                    name = live["name"]
    closes = [b.close for b in reversed(bars)]
    return templates.TemplateResponse(
        request,
        "history.html",
        {
            "symbols": symbols, "names": names, "symbol": symbol, "name": name,
            "days": days, "bars": bars, "closes": closes,
            "closes_min": min(closes) if closes else 0,
            "closes_max": max(closes) if closes else 1,
            "minutes": minutes, "live": live,
        },
    )


@app.post("/history/backfill")
def history_backfill(symbol: str = Form(...)):
    """为任意股票回填日线历史并跳转查看"""
    from . import collector

    symbol = symbol.strip()
    if symbol:
        with SessionLocal() as s:
            collector.backfill_daily_history(s, [symbol], config.HISTORY_DAYS)
            s.commit()
    return RedirectResponse(f"/history?symbol={symbol}", status_code=303)


def _name_map(s) -> dict[str, str]:
    """代码 -> 中文名（行情/初筛/持仓多渠道补全）"""
    names: dict[str, str] = {}
    for model in (Quote, Screening, Position):
        rows = s.execute(select(model.symbol, model.name).distinct()).all()
        for sym, n in rows:
            names.setdefault(sym, n)
    return names


@app.post("/decisions/run")
def run_decision_now():
    """手动触发一轮 AI 决策（非交易时段仅记录分析，不下单）"""
    from . import decision as decision_svc

    with SessionLocal() as s:
        decision_svc.run_decision(s)
        s.commit()
    return RedirectResponse("/decisions", status_code=303)


@app.get("/api/overview")
def api_overview():
    with SessionLocal() as s:
        account = engine.get_account(s)
        positions = s.scalars(select(Position)).all()
        prices = engine.latest_prices(s, [p.symbol for p in positions])
        market_value = sum(prices.get(p.symbol, p.avg_cost) * p.quantity for p in positions)
    return {
        "cash": account.cash,
        "market_value": round(market_value, 2),
        "total_asset": round(account.cash + market_value, 2),
        "initial_capital": account.initial_capital,
    }
