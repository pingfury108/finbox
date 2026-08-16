"""add daily_bars

Revision ID: 0002
Revises: 0001
Create Date: 2025-01-02

"""
import sqlalchemy as sa
from alembic import op

revision = "0002"
down_revision = "0001"
branch_labels = None
depends_on = None


def upgrade() -> None:
    op.create_table(
        "daily_bars",
        sa.Column("id", sa.Integer(), primary_key=True),
        sa.Column("symbol", sa.String(16), nullable=False),
        sa.Column("date", sa.Date(), nullable=False),
        sa.Column("open", sa.Float(), nullable=False),
        sa.Column("high", sa.Float(), nullable=False),
        sa.Column("low", sa.Float(), nullable=False),
        sa.Column("close", sa.Float(), nullable=False),
        sa.Column("volume", sa.Float(), nullable=True),
        sa.Column("amount", sa.Float(), nullable=True),
        sa.UniqueConstraint("symbol", "date"),
    )
    op.create_index("ix_daily_bars_symbol_date", "daily_bars", ["symbol", "date"])


def downgrade() -> None:
    op.drop_index("ix_daily_bars_symbol_date", table_name="daily_bars")
    op.drop_table("daily_bars")
