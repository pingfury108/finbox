"""initial schema

Revision ID: 0001
Revises:
Create Date: 2025-01-01

"""
import sqlalchemy as sa
from alembic import op

revision = "0001"
down_revision = None
branch_labels = None
depends_on = None


def upgrade() -> None:
    op.create_table(
        "account",
        sa.Column("id", sa.Integer(), primary_key=True),
        sa.Column("cash", sa.Float(), nullable=False),
        sa.Column("initial_capital", sa.Float(), nullable=False),
    )
    op.create_table(
        "quotes",
        sa.Column("id", sa.Integer(), primary_key=True),
        sa.Column("symbol", sa.String(16), nullable=False),
        sa.Column("name", sa.String(64), nullable=False),
        sa.Column("ts", sa.DateTime(), nullable=False),
        sa.Column("price", sa.Float(), nullable=False),
        sa.Column("pct_change", sa.Float(), nullable=True),
        sa.Column("volume", sa.Float(), nullable=True),
        sa.Column("amount", sa.Float(), nullable=True),
    )
    op.create_index("ix_quotes_symbol_ts", "quotes", ["symbol", "ts"])
    op.create_table(
        "positions",
        sa.Column("id", sa.Integer(), primary_key=True),
        sa.Column("symbol", sa.String(16), nullable=False, unique=True),
        sa.Column("name", sa.String(64), nullable=False),
        sa.Column("quantity", sa.Integer(), nullable=False),
        sa.Column("avg_cost", sa.Float(), nullable=False),
        sa.Column("updated_at", sa.DateTime(), nullable=False),
    )
    op.create_table(
        "ai_decisions",
        sa.Column("id", sa.Integer(), primary_key=True),
        sa.Column("ts", sa.DateTime(), nullable=False),
        sa.Column("model", sa.String(64), nullable=False),
        sa.Column("context", sa.Text(), nullable=False),
        sa.Column("raw_response", sa.Text(), nullable=False),
        sa.Column("actions", sa.Text(), nullable=False),
        sa.Column("status", sa.String(16), nullable=False),
        sa.Column("note", sa.Text(), nullable=False),
    )
    op.create_table(
        "trades",
        sa.Column("id", sa.Integer(), primary_key=True),
        sa.Column("symbol", sa.String(16), nullable=False),
        sa.Column("name", sa.String(64), nullable=False),
        sa.Column("side", sa.String(4), nullable=False),
        sa.Column("price", sa.Float(), nullable=False),
        sa.Column("quantity", sa.Integer(), nullable=False),
        sa.Column("amount", sa.Float(), nullable=False),
        sa.Column("ts", sa.DateTime(), nullable=False),
        sa.Column("decision_id", sa.Integer(), sa.ForeignKey("ai_decisions.id"), nullable=True),
    )
    op.create_table(
        "account_snapshots",
        sa.Column("id", sa.Integer(), primary_key=True),
        sa.Column("ts", sa.DateTime(), nullable=False),
        sa.Column("cash", sa.Float(), nullable=False),
        sa.Column("market_value", sa.Float(), nullable=False),
        sa.Column("total_asset", sa.Float(), nullable=False),
    )
    op.create_table(
        "reviews",
        sa.Column("id", sa.Integer(), primary_key=True),
        sa.Column("decision_id", sa.Integer(), sa.ForeignKey("ai_decisions.id"), nullable=False),
        sa.Column("days_after", sa.Integer(), nullable=False),
        sa.Column("ts", sa.DateTime(), nullable=False),
        sa.Column("summary", sa.Text(), nullable=False),
        sa.Column("pnl", sa.Float(), nullable=True),
    )


def downgrade() -> None:
    op.drop_table("reviews")
    op.drop_table("account_snapshots")
    op.drop_table("trades")
    op.drop_table("ai_decisions")
    op.drop_table("positions")
    op.drop_index("ix_quotes_symbol_ts", table_name="quotes")
    op.drop_table("quotes")
    op.drop_table("account")
